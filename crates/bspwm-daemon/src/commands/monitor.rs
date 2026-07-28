use super::{
    ArgCursor, CommandArgument, CommandEffect, CommandHandler, CommandRestArguments, Coordinates,
    Response, fail, invalid_argument, io, locate_desktop, not_enough, parse_command_argument,
    parse_command_rest_arguments, parse_rectangle, parse_terminal, text,
};

command_set! {
    domain: b"monitor";
    enum MonitorCommand<'a> {
        Rename {
            name: &'a [u8] = raw,
        } => [b"-n", b"--rename"],
        Swap {
            selector: &'a [u8] = raw,
        } => [b"-s", b"--swap"],
        AddDesktops {
            arguments: CommandRestArguments<'a> = map(parse_command_rest_arguments),
        } => [b"-a", b"--add-desktops"],
        ResetDesktops {
            arguments: CommandRestArguments<'a> = map(parse_command_rest_arguments),
        } => [b"-d", b"--reset-desktops"],
        ReorderDesktops {
            arguments: CommandRestArguments<'a> = map(parse_command_rest_arguments),
        } => [b"-o", b"--reorder-desktops"],
        Rectangle {
            argument: CommandArgument<'a> = custom(parse_command_argument),
        } => [b"-g", b"--rectangle"],
        Remove {
            terminal: () = custom(parse_terminal),
        } => [b"-r", b"--remove"],
        Focus {
            selector: Option<&'a [u8]> = optional,
        } => [b"-f", b"--focus"],
    }
}

impl CommandHandler<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn handle_monitor(
        &mut self,
        args: &[&[u8]],
        rsp: &mut dyn Response,
    ) -> io::Result<()> {
        let Some((reference, mut target, index)) =
            self.domain_preamble(args, b"monitor", Self::resolve_monitor, rsp)?
        else {
            return Ok(());
        };
        let mut cursor = ArgCursor::new(&args[index..]);
        while let Some(command) = MonitorCommand::next(&mut cursor, rsp)? {
            match command {
                MonitorCommand::Rename { name } => {
                    let (Some(monitor), Some(name)) = (target.monitor, text(name)) else {
                        return fail(rsp, b"");
                    };
                    let previous = self.state.world.monitor(monitor).name.clone();
                    self.state.world.rename_monitor(monitor, name);
                    // The stored name is length-bounded, so a rename to an
                    // over-long variant of the current one is not a change.
                    let current = self.state.world.monitor(monitor).name.clone();
                    if current != previous {
                        self.broadcast(
                            crate::types::SubscriberMask::MONITOR_RENAME,
                            format!(
                                "monitor_rename 0x{:08X} {previous} {current}\n",
                                self.state.world.monitor(monitor).external_id,
                            ),
                        );
                        self.report_effect();
                    }
                    self.state
                        .pending_effects
                        .push(CommandEffect::UpdateMonitorRoot { monitor });
                }
                MonitorCommand::Swap { selector: argument } => {
                    let Some(destination) = Self::selector_failure(
                        self.resolve_monitor(argument, reference),
                        b"monitor -s",
                        argument,
                        rsp,
                    )?
                    else {
                        return Ok(());
                    };
                    let (Some(first), Some(second)) = (target.monitor, destination.monitor) else {
                        return fail(rsp, b"");
                    };
                    if !self.state.world.swap_monitors(first, second) {
                        return fail(rsp, b"");
                    }
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    self.broadcast(
                        crate::types::SubscriberMask::MONITOR_SWAP,
                        format!(
                            "monitor_swap 0x{:08X} 0x{:08X}\n",
                            self.state.world.monitor(first).external_id,
                            self.state.world.monitor(second).external_id,
                        ),
                    );
                    self.report_effect();
                }
                MonitorCommand::AddDesktops { arguments } => {
                    let CommandRestArguments {
                        command,
                        values: names,
                    } = arguments;
                    let monitor = require!(target.monitor, rsp);
                    if names.is_empty() {
                        return not_enough(rsp, b"monitor", command);
                    }
                    for name in names {
                        let name = require!(text(name), rsp);
                        let external_id = self.state.world.next_external_id();
                        let desktop = self.state.world.create_desktop(
                            external_id,
                            Some(name),
                            &self.state.settings,
                        );
                        self.state.world.add_desktop(monitor, desktop);
                    }
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    break;
                }
                MonitorCommand::ResetDesktops { arguments } => {
                    let CommandRestArguments {
                        command,
                        values: names,
                    } = arguments;
                    let monitor = require!(target.monitor, rsp);
                    if names.is_empty() {
                        return not_enough(rsp, b"monitor", command);
                    }
                    let existing = self.state.world.monitor(monitor).desktops.clone();
                    for (desktop, name) in existing.iter().copied().zip(names.iter()) {
                        let name = require!(text(name), rsp);
                        self.state.world.rename_desktop(desktop, name);
                    }
                    if names.len() > existing.len() {
                        for name in &names[existing.len()..] {
                            let name = require!(text(name), rsp);
                            let external_id = self.state.world.next_external_id();
                            let desktop = self.state.world.create_desktop(
                                external_id,
                                Some(name),
                                &self.state.settings,
                            );
                            self.state.world.add_desktop(monitor, desktop);
                        }
                    } else if names.len() < existing.len() {
                        for desktop in existing[names.len()..].iter().rev().copied() {
                            let Some(removal) = self
                                .state
                                .world
                                .remove_desktop(desktop, self.state.settings.split_ratio)
                            else {
                                return fail(rsp, b"");
                            };
                            self.purge_removal(&removal);
                        }
                    }
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    break;
                }
                MonitorCommand::ReorderDesktops { arguments } => {
                    let CommandRestArguments {
                        command,
                        values: names,
                    } = arguments;
                    let monitor = require!(target.monitor, rsp);
                    if names.is_empty() {
                        return not_enough(rsp, b"monitor", command);
                    }
                    let requested: Vec<_> = names
                        .iter()
                        .filter_map(|name| {
                            text(name).and_then(|name| locate_desktop(&self.state.world, name))
                        })
                        .filter(|location| location.monitor == Some(monitor))
                        .filter_map(|location| location.desktop)
                        .collect();
                    self.state.world.reorder_desktops(monitor, &requested);
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    break;
                }
                MonitorCommand::Rectangle { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let (Some(monitor), Some(rectangle)) =
                        (target.monitor, text(argument).and_then(parse_rectangle))
                    else {
                        return invalid_argument(rsp, b"monitor", command, argument);
                    };
                    if self
                        .state
                        .world
                        .update_monitor_rectangle(monitor, rectangle)
                    {
                        self.state
                            .pending_effects
                            .push(CommandEffect::UpdateMonitorRoot { monitor });
                        for desktop in self.state.world.monitor(monitor).desktops.clone() {
                            self.arrange_effect(monitor, desktop);
                        }
                        self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    }
                }
                MonitorCommand::Remove { terminal: () } => {
                    let monitor = require!(target.monitor, rsp);
                    let external_id = self.state.world.monitor(monitor).external_id;
                    let removed_desktops: Vec<_> = self
                        .state
                        .world
                        .monitor(monitor)
                        .desktops
                        .iter()
                        .map(|desktop| self.state.world.desktop(*desktop).external_id)
                        .collect();
                    let was_focused = self.state.world.focused_monitor == Some(monitor);
                    // Read before the removal frees the slot.
                    let root_id = self.state.world.monitor(monitor).root_id;
                    let removal = require!(self.state.world.remove_monitor(monitor), rsp);
                    self.purge_removal(&removal);
                    if let Some(root) = root_id {
                        self.state
                            .pending_effects
                            .push(CommandEffect::DestroyMonitorRoot { root });
                    }
                    self.state
                        .pending_effects
                        .extend([CommandEffect::SyncEwmh, CommandEffect::RefreshBorders]);
                    for desktop in removed_desktops {
                        self.broadcast(
                            crate::types::SubscriberMask::DESKTOP_REMOVE,
                            format!("desktop_remove 0x{external_id:08X} 0x{desktop:08X}\n"),
                        );
                        self.report_effect();
                    }
                    self.broadcast(
                        crate::types::SubscriberMask::MONITOR_REMOVE,
                        format!("monitor_remove 0x{external_id:08X}\n"),
                    );
                    self.report_effect();
                    if was_focused
                        && let Some(focused) = self.state.world.focused_monitor
                        && let Some(desktop) = self.state.world.monitor(focused).active_desktop
                    {
                        let _ = self.focus_location(
                            Coordinates::in_desktop(
                                focused,
                                desktop,
                                self.state.world.desktop(desktop).tree.focus,
                            ),
                            false,
                        );
                    }
                    return Ok(());
                }
                MonitorCommand::Focus { selector } => {
                    let mut destination = target;
                    if let Some(argument) = selector {
                        let Some(mut location) = Self::selector_failure(
                            self.resolve_monitor(argument, reference),
                            b"monitor -f",
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        location.desktop = location
                            .monitor
                            .and_then(|monitor| self.state.world.monitor(monitor).active_desktop);
                        location.node = location
                            .desktop
                            .and_then(|desktop| self.state.world.desktop(desktop).tree.focus);
                        destination = location;
                    } else if let Some(monitor) = destination.monitor {
                        destination.desktop = self.state.world.monitor(monitor).active_desktop;
                        destination.node = destination
                            .desktop
                            .and_then(|desktop| self.state.world.desktop(desktop).tree.focus);
                    }
                    if !self.focus_location(destination, false) {
                        return fail(rsp, b"");
                    }
                    target = destination;
                }
            }
        }
        Ok(())
    }
}
