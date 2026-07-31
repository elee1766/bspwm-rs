use super::{
    ArgCursor, CommandArgument, CommandEffect, CommandHandler, Coordinates, CycleDirection,
    Response, SelectorFollowArguments, fail, invalid_argument, io, parse_command_argument,
    parse_cycle_direction, parse_layout, parse_selector_follow_arguments, parse_terminal, text,
};
use crate::types::Layout;

command_set! {
    domain: b"desktop";
    enum DesktopCommand<'a> {
        Rename {
            name: &'a [u8] = raw,
        } => [b"-n", b"--rename"],
        Layout {
            argument: CommandArgument<'a> = custom(parse_command_argument),
        } => [b"-l", b"--layout"],
        Activate {
            selector: Option<&'a [u8]> = optional,
        } => [b"-a", b"--activate"],
        ToMonitor {
            arguments: SelectorFollowArguments<'a> = custom(parse_selector_follow_arguments),
        } => [b"-m", b"--to-monitor"],
        Swap {
            arguments: SelectorFollowArguments<'a> = custom(parse_selector_follow_arguments),
        } => [b"-s", b"--swap"],
        Bubble {
            argument: CommandArgument<'a> = custom(parse_command_argument),
        } => [b"-b", b"--bubble"],
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
    pub(super) fn handle_desktop(
        &mut self,
        args: &[&[u8]],
        rsp: &mut dyn Response,
    ) -> io::Result<()> {
        let Some((reference, mut target, index)) =
            self.domain_preamble(args, b"desktop", Self::resolve_desktop, rsp)?
        else {
            return Ok(());
        };
        let mut cursor = ArgCursor::new(&args[index..]);
        while let Some(command) = DesktopCommand::next(&mut cursor, rsp)? {
            match command {
                DesktopCommand::Rename { name } => {
                    let (Some(desktop), Some(name)) = (target.desktop, text(name)) else {
                        return fail(rsp, b"");
                    };
                    let previous = self.state.world.desktop(desktop).name.clone();
                    self.state.world.rename_desktop(desktop, name);
                    // The stored name is length-bounded, so a rename to an
                    // over-long variant of the current one is not a change.
                    let current = self.state.world.desktop(desktop).name.clone();
                    if current != previous
                        && let Some(monitor) = self.state.world.desktop_monitor(desktop)
                    {
                        self.broadcast(
                            crate::types::SubscriberMask::DESKTOP_RENAME,
                            format!(
                                "desktop_rename 0x{:08X} 0x{:08X} {previous} {current}\n",
                                self.state.world.monitor(monitor).external_id,
                                self.state.world.desktop(desktop).external_id,
                            ),
                        );
                        self.report_effect();
                    }
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                }
                DesktopCommand::Layout { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let desktop = require!(target.desktop, rsp);
                    let monitor = require!(target.monitor, rsp);
                    let previous = self.state.world.desktop(desktop).layout;
                    let layout = match text(argument) {
                        Some("next" | "prev") => {
                            match self.state.world.desktop(desktop).user_layout {
                                Layout::Tiled => Layout::Monocle,
                                Layout::Monocle => Layout::Tiled,
                            }
                        }
                        Some(value) => match parse_layout(value) {
                            Some(value) => value,
                            None => return invalid_argument(rsp, b"desktop", command, argument),
                        },
                        None => return invalid_argument(rsp, b"desktop", command, argument),
                    };
                    if !self.state.world.set_layout(
                        desktop,
                        layout,
                        true,
                        self.state.settings.single_monocle,
                    ) {
                        return fail(rsp, b"");
                    }
                    self.layout_effect(monitor, desktop, previous);
                }
                DesktopCommand::Activate { selector } => {
                    let mut destination = target;
                    if let Some(argument) = selector {
                        let Some(loc) = Self::selector_failure(
                            self.resolve_desktop(argument, reference),
                            b"desktop -a",
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        destination = loc;
                    }
                    let (Some(_), Some(desktop)) = (destination.monitor, destination.desktop)
                    else {
                        return fail(rsp, b"");
                    };
                    destination.node = self.state.world.desktop(desktop).tree.focus;
                    if !self.focus_location(destination, true) {
                        return fail(rsp, b"");
                    }
                }
                DesktopCommand::ToMonitor { arguments } => {
                    let SelectorFollowArguments {
                        selector: argument,
                        follow,
                    } = arguments;
                    let Some(destination) = Self::selector_failure(
                        self.resolve_monitor(argument, reference),
                        b"desktop -m",
                        argument,
                        rsp,
                    )?
                    else {
                        return Ok(());
                    };
                    let (Some(desktop), Some(monitor)) = (target.desktop, destination.monitor)
                    else {
                        return fail(rsp, b"");
                    };
                    let source = require!(self.state.world.desktop_monitor(desktop), rsp);
                    let was_active =
                        self.state.world.monitor(source).active_desktop == Some(desktop);
                    let was_focused =
                        self.state.world.focused_monitor == Some(source) && was_active;
                    let source_rectangle = self.state.world.monitor(source).rectangle;
                    let destination_rectangle = self.state.world.monitor(monitor).rectangle;
                    let sticky_count = if was_active {
                        self.state
                            .world
                            .desktop(desktop)
                            .tree
                            .root
                            .map_or(0, |root| self.state.world.tree.sticky_count(root))
                    } else {
                        0
                    };
                    if self.state.world.monitor(source).desktops.len() < 2
                        || !self.state.world.transfer_desktop(desktop, monitor)
                    {
                        return fail(rsp, b"");
                    }
                    if let Some(root) = self.state.world.desktop(desktop).tree.root {
                        self.adapt_subtree_geometry(root, source_rectangle, destination_rectangle);
                    }
                    self.state.world.monitor_mut(source).sticky_count = self
                        .state
                        .world
                        .monitor(source)
                        .sticky_count
                        .saturating_sub(sticky_count);
                    self.state.world.monitor_mut(monitor).sticky_count = self
                        .state
                        .world
                        .monitor(monitor)
                        .sticky_count
                        .saturating_add(sticky_count);
                    if sticky_count > 0
                        && let Some(fallback) = self.state.world.monitor(source).active_desktop
                        && let Some(root) = self.state.world.desktop(desktop).tree.root
                    {
                        let nodes = self.sticky_roots(root);
                        let sticky_still = self.state.sticky_still;
                        self.state.sticky_still = false;
                        for node in nodes {
                            let anchor = self.state.world.desktop(fallback).tree.focus;
                            let _ = self.transfer_node_complete(
                                Coordinates::node(monitor, desktop, node),
                                Coordinates::in_desktop(source, fallback, anchor),
                                false,
                            );
                        }
                        self.state.sticky_still = sticky_still;
                    }
                    if !follow || !was_active || !was_focused {
                        self.state
                            .pending_effects
                            .push(CommandEffect::SetDesktopVisibility {
                                desktop,
                                visible: self.state.world.monitor(monitor).active_desktop
                                    == Some(desktop),
                                preserve_sticky: true,
                            });
                    }
                    self.state.history.remove_desktop(desktop);
                    if was_active {
                        if follow && was_focused {
                            let _ = self.focus_location(
                                Coordinates::in_desktop(
                                    monitor,
                                    desktop,
                                    self.state.world.desktop(desktop).tree.focus,
                                ),
                                false,
                            );
                        } else if let Some(fallback) =
                            self.state.world.monitor(source).active_desktop
                        {
                            let _ = self.focus_location(
                                Coordinates::in_desktop(
                                    source,
                                    fallback,
                                    self.state.world.desktop(fallback).tree.focus,
                                ),
                                !was_focused,
                            );
                        }
                    }
                    // `transfer_desktop` reassigns the source monitor's active
                    // desktop, but a missing one must not take the daemon down.
                    match self.state.world.monitor(source).active_desktop {
                        Some(active) => {
                            self.structural_effects(&[(source, active), (monitor, desktop)]);
                        }
                        None => self.structural_effects(&[(monitor, desktop)]),
                    }
                    self.broadcast(
                        crate::types::SubscriberMask::DESKTOP_TRANSFER,
                        format!(
                            "desktop_transfer 0x{:08X} 0x{:08X} 0x{:08X}\n",
                            self.state.world.monitor(source).external_id,
                            self.state.world.desktop(desktop).external_id,
                            self.state.world.monitor(monitor).external_id,
                        ),
                    );
                    self.report_effect();
                    target.monitor = Some(monitor);
                }
                DesktopCommand::Swap { arguments } => {
                    let SelectorFollowArguments {
                        selector: argument,
                        follow,
                    } = arguments;
                    let Some(destination) = Self::selector_failure(
                        self.resolve_desktop(argument, reference),
                        b"desktop -s",
                        argument,
                        rsp,
                    )?
                    else {
                        return Ok(());
                    };
                    let (Some(first), Some(second)) = (target.desktop, destination.desktop) else {
                        return fail(rsp, b"");
                    };
                    let (Some(first_monitor), Some(second_monitor)) =
                        (target.monitor, destination.monitor)
                    else {
                        return fail(rsp, b"");
                    };
                    let first_active =
                        self.state.world.monitor(first_monitor).active_desktop == Some(first);
                    let second_active =
                        self.state.world.monitor(second_monitor).active_desktop == Some(second);
                    let first_focused =
                        self.state.world.focused_monitor == Some(first_monitor) && first_active;
                    let second_focused =
                        self.state.world.focused_monitor == Some(second_monitor) && second_active;
                    let first_rectangle = self.state.world.monitor(first_monitor).rectangle;
                    let second_rectangle = self.state.world.monitor(second_monitor).rectangle;
                    let first_stickies = if first_active {
                        self.state
                            .world
                            .desktop(first)
                            .tree
                            .root
                            .map_or_else(Vec::new, |root| self.sticky_roots(root))
                    } else {
                        Vec::new()
                    };
                    let second_stickies = if second_active {
                        self.state
                            .world
                            .desktop(second)
                            .tree
                            .root
                            .map_or_else(Vec::new, |root| self.sticky_roots(root))
                    } else {
                        Vec::new()
                    };
                    if !self.state.world.swap_desktops(first, second) {
                        return fail(rsp, b"");
                    }
                    if first_monitor != second_monitor {
                        if let Some(root) = self.state.world.desktop(first).tree.root {
                            self.adapt_subtree_geometry(root, first_rectangle, second_rectangle);
                        }
                        if let Some(root) = self.state.world.desktop(second).tree.root {
                            self.adapt_subtree_geometry(root, second_rectangle, first_rectangle);
                        }
                        self.structural_effects(&[
                            (first_monitor, second),
                            (second_monitor, first),
                        ]);
                        self.relocate_swapped_stickies(
                            &first_stickies,
                            second_monitor,
                            first,
                            first_monitor,
                            second,
                        );
                        self.relocate_swapped_stickies(
                            &second_stickies,
                            first_monitor,
                            second,
                            second_monitor,
                            first,
                        );
                    }
                    if first_active != second_active {
                        self.state.pending_effects.extend([
                            CommandEffect::SetDesktopVisibility {
                                desktop: first,
                                visible: second_active || (follow && first_focused),
                                preserve_sticky: false,
                            },
                            CommandEffect::SetDesktopVisibility {
                                desktop: second,
                                visible: first_active || (follow && second_focused),
                                preserve_sticky: false,
                            },
                        ]);
                    }
                    self.state.history.remove_desktop(first);
                    self.state.history.remove_desktop(second);
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    if first_focused {
                        let (monitor, desktop) = if follow || first_monitor == second_monitor {
                            (second_monitor, first)
                        } else {
                            (first_monitor, second)
                        };
                        let _ = self.focus_location(
                            Coordinates::in_desktop(
                                monitor,
                                desktop,
                                self.state.world.desktop(desktop).tree.focus,
                            ),
                            false,
                        );
                    } else if second_focused {
                        let (monitor, desktop) = if follow || first_monitor == second_monitor {
                            (first_monitor, second)
                        } else {
                            (second_monitor, first)
                        };
                        let _ = self.focus_location(
                            Coordinates::in_desktop(
                                monitor,
                                desktop,
                                self.state.world.desktop(desktop).tree.focus,
                            ),
                            false,
                        );
                    }
                    self.state
                        .pending_effects
                        .push(CommandEffect::RefreshBorders);
                    self.broadcast(
                        crate::types::SubscriberMask::DESKTOP_SWAP,
                        format!(
                            "desktop_swap 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X}\n",
                            self.state.world.monitor(first_monitor).external_id,
                            self.state.world.desktop(first).external_id,
                            self.state.world.monitor(second_monitor).external_id,
                            self.state.world.desktop(second).external_id,
                        ),
                    );
                    self.report_effect();
                    target = destination;
                }
                DesktopCommand::Bubble { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let (Some(desktop), Some(direction)) = (
                        target.desktop,
                        text(argument).and_then(parse_cycle_direction),
                    ) else {
                        return invalid_argument(rsp, b"desktop", command, argument);
                    };
                    if !self
                        .state
                        .world
                        .bubble_desktop(desktop, direction == CycleDirection::Next)
                    {
                        return fail(rsp, b"");
                    }
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                }
                DesktopCommand::Remove { terminal: () } => {
                    let desktop = require!(target.desktop, rsp);
                    let monitor = require!(self.state.world.desktop_monitor(desktop), rsp);
                    let monitor_external = self.state.world.monitor(monitor).external_id;
                    let desktop_external = self.state.world.desktop(desktop).external_id;
                    let desktops = &self.state.world.monitor(monitor).desktops;
                    let Some(position) =
                        desktops.iter().position(|candidate| *candidate == desktop)
                    else {
                        return fail(rsp, b"");
                    };
                    let fallback = if position == 0 {
                        desktops.get(1).copied()
                    } else {
                        desktops.get(position - 1).copied()
                    };
                    if let (Some(root), Some(fallback)) =
                        (self.state.world.desktop(desktop).tree.root, fallback)
                    {
                        let anchor = self.state.world.desktop(fallback).tree.focus;
                        if self
                            .transfer_node_complete(
                                Coordinates::node(monitor, desktop, root),
                                Coordinates::in_desktop(monitor, fallback, anchor),
                                false,
                            )
                            .is_err()
                        {
                            return fail(rsp, b"");
                        }
                    }
                    let Some(removal) = self
                        .state
                        .world
                        .remove_desktop(desktop, self.state.settings.split_ratio)
                    else {
                        return fail(rsp, b"");
                    };
                    self.purge_removal(&removal);
                    if let Some(fallback) = self.state.world.monitor(monitor).active_desktop {
                        self.structural_effects(&[(monitor, fallback)]);
                    } else {
                        self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    }
                    self.broadcast(
                        crate::types::SubscriberMask::DESKTOP_REMOVE,
                        format!(
                            "desktop_remove 0x{monitor_external:08X} 0x{desktop_external:08X}\n"
                        ),
                    );
                    self.report_effect();
                    return Ok(());
                }
                DesktopCommand::Focus { selector } => {
                    let mut destination = target;
                    if let Some(argument) = selector {
                        let Some(location) = Self::selector_failure(
                            self.resolve_desktop(argument, reference),
                            b"desktop -f",
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        destination = location;
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
