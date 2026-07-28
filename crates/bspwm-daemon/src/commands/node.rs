use super::{
    ArgCursor, BoolDeclaration, CommandArgument, CommandEffect, CommandHandler, CommandParseError,
    Coordinates, NodeFlag, Response, SelectorFollowArguments, SplitType, fail, invalid_argument,
    io, node_state_status, parse_bool_declaration, parse_circulate_direction, parse_client_state,
    parse_command_argument, parse_degree, parse_direction, parse_flip, parse_optional_selector,
    parse_resize_handle, parse_selector_follow_arguments, parse_split_type, parse_stack_layer,
    parse_terminal, text,
};
use crate::types::ResizeHandle;

fn parse_i32_argument(argument: &[u8]) -> Option<i32> {
    text(argument)?.parse().ok()
}

fn parse_resize_handle_argument(argument: &[u8]) -> Option<ResizeHandle> {
    text(argument).and_then(parse_resize_handle)
}

struct MoveArguments {
    dx: i32,
    dy: i32,
}

fn parse_move_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<MoveArguments, CommandParseError<'a>> {
    let dx = cursor.required(command)?;
    let dy = cursor.required(command)?;
    let Some(dx) = parse_i32_argument(dx) else {
        return Err(CommandParseError::InvalidArgument { command, value: dx });
    };
    let Some(dy) = parse_i32_argument(dy) else {
        return Err(CommandParseError::InvalidArgument { command, value: dy });
    };
    Ok(MoveArguments { dx, dy })
}

struct ResizeArguments {
    handle: ResizeHandle,
    dx: i32,
    dy: i32,
}

fn parse_resize_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<ResizeArguments, CommandParseError<'a>> {
    let handle = cursor.required(command)?;
    let dx = cursor.required(command)?;
    let dy = cursor.required(command)?;
    let Some(handle) = parse_resize_handle_argument(handle) else {
        return Err(CommandParseError::InvalidArgument {
            command,
            value: handle,
        });
    };
    let Some(dx) = parse_i32_argument(dx) else {
        return Err(CommandParseError::InvalidArgument { command, value: dx });
    };
    let Some(dy) = parse_i32_argument(dy) else {
        return Err(CommandParseError::InvalidArgument { command, value: dy });
    };
    Ok(ResizeArguments { handle, dx, dy })
}

enum TransferTarget {
    Desktop,
    Monitor,
    Node,
}

struct NodeTransferArguments<'a> {
    target: TransferTarget,
    selector: &'a [u8],
    follow: bool,
}

fn parse_node_transfer_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<NodeTransferArguments<'a>, CommandParseError<'a>> {
    let target = if matches!(command, b"-d" | b"--to-desktop") {
        TransferTarget::Desktop
    } else if matches!(command, b"-m" | b"--to-monitor") {
        TransferTarget::Monitor
    } else {
        TransferTarget::Node
    };
    let SelectorFollowArguments { selector, follow } =
        parse_selector_follow_arguments(cursor, command)?;
    Ok(NodeTransferArguments {
        target,
        selector,
        follow,
    })
}

struct NodeFocusArguments<'a> {
    activate: bool,
    selector: Option<&'a [u8]>,
}

fn parse_node_focus_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> NodeFocusArguments<'a> {
    NodeFocusArguments {
        activate: matches!(command, b"-a" | b"--activate"),
        selector: parse_optional_selector(cursor),
    }
}

struct NodeTermination {
    close: bool,
}

fn parse_node_termination<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<NodeTermination, CommandParseError<'a>> {
    parse_terminal(cursor, command)?;
    Ok(NodeTermination {
        close: matches!(command, b"-c" | b"--close"),
    })
}

command_set! {
    domain: b"node";
    enum NodeCommand<'a> {
        Rotate { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-R", b"--rotate"],
        Flip { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-F", b"--flip"],
        Equalize => [b"-E", b"--equalize"],
        Balance => [b"-B", b"--balance"],
        SplitType { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-y", b"--type"],
        SplitRatio { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-r", b"--ratio"],
        PreselDirection { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-p", b"--presel-dir"],
        PreselRatio { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-o", b"--presel-ratio"],
        Layer { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-l", b"--layer"],
        State { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-t", b"--state"],
        Flag { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-g", b"--flag"],
        Circulate { argument: CommandArgument<'a> = custom(parse_command_argument) }
            => [b"-C", b"--circulate"],
        InsertReceptacle => [b"-i", b"--insert-receptacle"],
        Swap { arguments: SelectorFollowArguments<'a> = custom(parse_selector_follow_arguments) }
            => [b"-s", b"--swap"],
        Transfer { arguments: NodeTransferArguments<'a> = custom(parse_node_transfer_arguments) }
            => [b"-d", b"--to-desktop", b"-m", b"--to-monitor", b"-n", b"--to-node"],
        Focus { arguments: NodeFocusArguments<'a> = map(parse_node_focus_arguments) }
            => [b"-f", b"--focus", b"-a", b"--activate"],
        Move { arguments: MoveArguments = custom(parse_move_arguments) }
            => [b"-v", b"--move"],
        Resize { arguments: ResizeArguments = custom(parse_resize_arguments) }
            => [b"-z", b"--resize"],
        Terminate { termination: NodeTermination = custom(parse_node_termination) }
            => [b"-c", b"--close", b"-k", b"--kill"],
    }
}

impl CommandHandler<'_> {
    #[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
    pub(super) fn handle_node(&mut self, args: &[&[u8]], rsp: &mut dyn Response) -> io::Result<()> {
        let Some((reference, mut target, index)) =
            self.domain_preamble(args, b"node", Self::resolve_node, rsp)?
        else {
            return Ok(());
        };
        let mut cursor = ArgCursor::new(&args[index..]);
        while let Some(command) = NodeCommand::next(&mut cursor, rsp)? {
            match command {
                NodeCommand::Rotate { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let node = require!(target.node, rsp);
                    let Some(degree) = text(argument).and_then(parse_degree) else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    self.state.world.tree.rotate(node, degree);
                    self.arrange_target(target);
                }
                NodeCommand::Flip { argument } => {
                    let CommandArgument {
                        value: argument, ..
                    } = argument;
                    let node = require!(target.node, rsp);
                    let value = require!(text(argument).and_then(parse_flip), rsp);
                    self.state.world.tree.flip(node, value);
                    self.arrange_target(target);
                }
                NodeCommand::Equalize => {
                    let node = require!(target.node, rsp);
                    self.state
                        .world
                        .tree
                        .equalize(node, self.state.settings.split_ratio);
                    self.arrange_target(target);
                }
                NodeCommand::Balance => {
                    let node = require!(target.node, rsp);
                    self.state.world.tree.balance(node);
                    self.arrange_target(target);
                }
                NodeCommand::SplitType { argument } => {
                    let CommandArgument {
                        value: argument, ..
                    } = argument;
                    let node = require!(target.node, rsp);
                    let split_type = match text(argument) {
                        Some("next" | "prev") => {
                            match self.state.world.tree.node(node).split_type {
                                SplitType::Horizontal => SplitType::Vertical,
                                SplitType::Vertical => SplitType::Horizontal,
                            }
                        }
                        Some(value) => match parse_split_type(value) {
                            Some(value) => value,
                            None => return fail(rsp, b""),
                        },
                        None => return fail(rsp, b""),
                    };
                    self.state.world.tree.node_mut(node).split_type = split_type;
                    self.state.world.tree.update_constraints(node);
                    self.state.world.tree.rebuild_constraints_towards_root(node);
                    self.arrange_target(target);
                }
                NodeCommand::SplitRatio { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let node = require!(target.node, rsp);
                    let Some(value) = text(argument).and_then(|value| value.parse::<f64>().ok())
                    else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    let old = self.state.world.tree.node(node).split_ratio;
                    let ratio = if argument.starts_with(b"+") || argument.starts_with(b"-") {
                        if (-1.0..1.0).contains(&value) {
                            old + value
                        } else {
                            let item = self.state.world.tree.node(node);
                            let extent = match item.split_type {
                                SplitType::Horizontal => item.rectangle.height,
                                SplitType::Vertical => item.rectangle.width,
                            };
                            if extent == 0 {
                                return fail(rsp, b"");
                            }
                            (f64::from(extent) * old + value) / f64::from(extent)
                        }
                    } else {
                        value
                    };
                    if !(0.0..1.0).contains(&ratio) {
                        return fail(rsp, b"");
                    }
                    self.state.world.tree.node_mut(node).split_ratio = ratio;
                    self.arrange_target(target);
                }
                NodeCommand::PreselDirection { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let Some(node) = target
                        .node
                        .filter(|node| !self.state.world.tree.node(*node).vacant)
                    else {
                        return fail(rsp, b"");
                    };
                    let status = if argument == b"cancel" {
                        self.state
                            .world
                            .tree
                            .cancel_presel(node)
                            .map(|_| "cancel".to_owned())
                    } else {
                        let (alternate, direction) = argument
                            .strip_prefix(b"~")
                            .map_or((false, argument), |value| (true, value));
                        let Some(direction) = text(direction).and_then(parse_direction) else {
                            return invalid_argument(rsp, b"node", command, argument);
                        };
                        if alternate
                            && self
                                .state
                                .world
                                .tree
                                .node(node)
                                .presel
                                .is_some_and(|presel| presel.split_dir == direction)
                        {
                            self.state
                                .world
                                .tree
                                .cancel_presel(node)
                                .map(|_| "cancel".to_owned())
                        } else {
                            self.state.world.tree.set_presel_direction(
                                node,
                                direction,
                                self.state.settings.split_ratio,
                            );
                            Some(format!("dir {}", direction.protocol_name()))
                        }
                    };
                    if let (Some(status), Some(monitor), Some(desktop)) =
                        (status, target.monitor, target.desktop)
                    {
                        self.broadcast(
                            crate::types::SubscriberMask::NODE_PRESEL,
                            format!(
                                "node_presel 0x{:08X} 0x{:08X} 0x{:08X} {status}\n",
                                self.state.world.monitor(monitor).external_id,
                                self.state.world.desktop(desktop).external_id,
                                self.state.world.tree.node(node).external_id,
                            ),
                        );
                    }
                    self.state
                        .pending_effects
                        .push(CommandEffect::SyncPreselFeedback {
                            node,
                            include_receptacle: false,
                        });
                }
                NodeCommand::PreselRatio { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let Some(node) = target
                        .node
                        .filter(|node| !self.state.world.tree.node(*node).vacant)
                    else {
                        return fail(rsp, b"");
                    };
                    let Some(ratio) = text(argument).and_then(|value| value.parse::<f64>().ok())
                    else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    if !(0.0..1.0).contains(&ratio) {
                        return invalid_argument(rsp, b"node", command, argument);
                    }
                    self.state.world.tree.set_presel_ratio(
                        node,
                        ratio,
                        self.state.settings.split_ratio,
                    );
                    if let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop) {
                        self.broadcast(
                            crate::types::SubscriberMask::NODE_PRESEL,
                            format!(
                                "node_presel 0x{:08X} 0x{:08X} 0x{:08X} ratio {ratio:.6}\n",
                                self.state.world.monitor(monitor).external_id,
                                self.state.world.desktop(desktop).external_id,
                                self.state.world.tree.node(node).external_id,
                            ),
                        );
                    }
                    self.state
                        .pending_effects
                        .push(CommandEffect::SyncPreselFeedback {
                            node,
                            include_receptacle: true,
                        });
                }
                NodeCommand::Layer { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let (Some(node), Some(layer)) =
                        (target.node, text(argument).and_then(parse_stack_layer))
                    else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    if !self.state.world.tree.set_layer(node, layer) {
                        return fail(rsp, b"");
                    }
                    self.state.pending_effects.extend([
                        CommandEffect::Restack {
                            node,
                            auto_raise: self.state.auto_raise,
                        },
                        CommandEffect::SyncWindowState { node },
                        CommandEffect::SyncEwmh,
                    ]);
                    if let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop) {
                        if self.state.world.desktop(desktop).tree.focus == Some(node) {
                            self.neutralize_occluding_windows(
                                monitor,
                                desktop,
                                node,
                                self.state.auto_raise,
                            );
                        }
                        let layer = layer.protocol_name();
                        self.broadcast(
                            crate::types::SubscriberMask::NODE_LAYER,
                            format!(
                                "node_layer 0x{:08X} 0x{:08X} 0x{:08X} {layer}\n",
                                self.state.world.monitor(monitor).external_id,
                                self.state.world.desktop(desktop).external_id,
                                self.state.world.tree.node(node).external_id,
                            ),
                        );
                    }
                }
                NodeCommand::State { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let (Some(node), Some(value)) = (target.node, text(argument)) else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    let (alternate, name) = value
                        .strip_prefix('~')
                        .map_or((false, value), |name| (true, name));
                    let client = require!(self.state.world.tree.node(node).client.as_ref(), rsp);
                    let old_state = client.state;
                    let state = if alternate && name.is_empty() {
                        client.last_state
                    } else {
                        let Some(requested) = parse_client_state(name) else {
                            return invalid_argument(rsp, b"node", command, argument);
                        };
                        if alternate && client.state == requested {
                            client.last_state
                        } else {
                            requested
                        }
                    };
                    // Everything below needs both, so demand them once instead
                    // of testing `target.monitor` and `target.desktop` thrice.
                    let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop) else {
                        return fail(rsp, b"");
                    };
                    let layout = self.state.world.desktop(desktop).layout;
                    if !self.set_node_state(monitor, desktop, node, state) {
                        return fail(rsp, b"");
                    }
                    self.arrange_effect(monitor, desktop);
                    self.state.pending_effects.extend([
                        CommandEffect::Restack {
                            node,
                            auto_raise: self.state.auto_raise,
                        },
                        CommandEffect::SyncWindowState { node },
                        CommandEffect::SyncEwmh,
                    ]);
                    for (reported, enabled) in [(old_state, false), (state, true)] {
                        self.broadcast(
                            crate::types::SubscriberMask::NODE_STATE,
                            node_state_status(
                                &self.state.world,
                                monitor,
                                desktop,
                                node,
                                reported,
                                enabled,
                            ),
                        );
                    }
                    if self.state.world.monitor(monitor).active_desktop == Some(desktop)
                        && self.state.world.desktop(desktop).tree.focus == Some(node)
                    {
                        self.report_effect();
                    }
                    self.layout_effect(monitor, desktop, layout);
                }
                NodeCommand::Flag { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let (Some(node), Some(declaration)) =
                        (target.node, text(argument).and_then(parse_bool_declaration))
                    else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    let (key, requested) = match declaration {
                        BoolDeclaration::Toggle { key } => (key, None),
                        BoolDeclaration::Set { key, value } => (key, Some(value)),
                    };
                    let flag = match key {
                        Some("hidden") => NodeFlag::Hidden,
                        Some("sticky") => NodeFlag::Sticky,
                        Some("private") => NodeFlag::Private,
                        Some("locked") => NodeFlag::Locked,
                        Some("marked") => NodeFlag::Marked,
                        _ => return invalid_argument(rsp, b"node", command, argument),
                    };
                    let value =
                        requested.unwrap_or_else(|| !self.state.world.tree.flag(node, flag));
                    // Read against the desktop the node is on now. The sticky
                    // branch below can rebind `target.desktop`, but `held_focus`
                    // is only consumed by the hidden branch, and one `-g` sets
                    // exactly one flag, so the two are mutually exclusive.
                    let held_focus = target.desktop.is_some_and(|desktop| {
                        self.state
                            .world
                            .desktop(desktop)
                            .tree
                            .focus
                            .is_some_and(|focus| self.state.world.tree.is_descendant(focus, node))
                    });
                    if flag == NodeFlag::Sticky && value {
                        let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop)
                        else {
                            return fail(rsp, b"");
                        };
                        if let Some(active) = self.state.world.monitor(monitor).active_desktop
                            && active != desktop
                        {
                            let anchor = self.state.world.desktop(active).tree.focus;
                            if self
                                .transfer_node_complete(
                                    target,
                                    Coordinates::in_desktop(monitor, active, anchor),
                                    false,
                                )
                                .is_err()
                            {
                                return fail(rsp, b"");
                            }
                            target.desktop = Some(active);
                        }
                    }
                    let presels = (flag == NodeFlag::Hidden)
                        .then(|| target.desktop.map(|desktop| self.presel_snapshot(desktop)))
                        .flatten();
                    if self.state.world.tree.set_flag(node, flag, value) {
                        if let (Some(monitor), Some(desktop), Some(presels)) =
                            (target.monitor, target.desktop, presels)
                        {
                            self.broadcast_cancelled_presels(monitor, desktop, presels);
                        }
                        if flag == NodeFlag::Sticky
                            && let Some(monitor) = target.monitor
                        {
                            let count = &mut self.state.world.monitor_mut(monitor).sticky_count;
                            *count = if value {
                                count.saturating_add(1)
                            } else {
                                count.saturating_sub(1)
                            };
                        }
                        if flag == NodeFlag::Hidden {
                            self.state
                                .pending_effects
                                .push(CommandEffect::SetWindowVisibility {
                                    node,
                                    visible: !value,
                                });
                            if let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop)
                            {
                                self.arrange_effect(monitor, desktop);
                                if value && held_focus {
                                    self.state.history.remove_node(
                                        &self.state.world.tree,
                                        node,
                                        true,
                                    );
                                    let repaired =
                                        self.state.world.desktop(desktop).tree.root.and_then(
                                            |root| self.state.world.tree.first_focusable_leaf(root),
                                        );
                                    self.state.world.desktop_mut(desktop).tree.focus = repaired;
                                    let activate = self.state.world.focused_monitor
                                        != Some(monitor)
                                        || self.state.world.monitor(monitor).active_desktop
                                            != Some(desktop);
                                    let _ = self.focus_location(
                                        Coordinates::in_desktop(monitor, desktop, repaired),
                                        activate,
                                    );
                                }
                            }
                        }
                        self.state.pending_effects.push(CommandEffect::SyncEwmh);
                        self.state
                            .pending_effects
                            .push(CommandEffect::SyncWindowState { node });
                        let name = flag.protocol_name();
                        if let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop) {
                            self.broadcast(
                                crate::types::SubscriberMask::NODE_FLAG,
                                format!(
                                    "node_flag 0x{:08X} 0x{:08X} 0x{:08X} {name} {}\n",
                                    self.state.world.monitor(monitor).external_id,
                                    self.state.world.desktop(desktop).external_id,
                                    self.state.world.tree.node(node).external_id,
                                    if value { "on" } else { "off" },
                                ),
                            );
                            if self.state.world.focused_monitor == Some(monitor)
                                && self.state.world.monitor(monitor).active_desktop == Some(desktop)
                                && self.state.world.desktop(desktop).tree.focus == Some(node)
                            {
                                self.report_effect();
                            }
                        }
                    }
                }
                NodeCommand::Circulate { argument } => {
                    let CommandArgument {
                        command,
                        value: argument,
                    } = argument;
                    let (Some(node), Some(desktop), Some(direction)) = (
                        target.node,
                        target.desktop,
                        text(argument).and_then(parse_circulate_direction),
                    ) else {
                        return invalid_argument(rsp, b"node", command, argument);
                    };
                    let mut tree_state = self.state.world.desktop(desktop).tree;
                    self.state
                        .world
                        .tree
                        .circulate(&mut tree_state, node, direction);
                    self.state.world.desktop_mut(desktop).tree = tree_state;
                    if let Some(monitor) = target.monitor {
                        self.arrange_effect(monitor, desktop);
                    }
                }
                NodeCommand::InsertReceptacle => {
                    let desktop = require!(target.desktop, rsp);
                    let Ok(receptacle) = self.state.world.insert_receptacle(
                        desktop,
                        target.node,
                        self.state.settings.split_ratio,
                    ) else {
                        return fail(rsp, b"");
                    };
                    if let Some(monitor) = target.monitor {
                        self.broadcast(
                            crate::types::SubscriberMask::NODE_ADD,
                            format!(
                                "node_add 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X}\n",
                                self.state.world.monitor(monitor).external_id,
                                self.state.world.desktop(desktop).external_id,
                                target.node.map_or(0, |node| self
                                    .state
                                    .world
                                    .tree
                                    .node(node)
                                    .external_id),
                                self.state.world.tree.node(receptacle).external_id,
                            ),
                        );
                        if self.state.world.desktop(desktop).tree.root == Some(receptacle) {
                            self.report_effect();
                        }
                        let previous = self.state.world.desktop(desktop).layout;
                        let leave_single_monocle =
                            self.state.settings.single_monocle
                                && previous == crate::types::Layout::Monocle
                                && self.state.world.desktop(desktop).tree.root.is_some_and(
                                    |root| self.state.world.tree.tiled_count(root, true) > 1,
                                );
                        if leave_single_monocle {
                            let user_layout = self.state.world.desktop(desktop).user_layout;
                            if self.state.world.set_layout(
                                desktop,
                                user_layout,
                                false,
                                self.state.settings.single_monocle,
                            ) {
                                self.layout_effect(monitor, desktop, previous);
                            } else {
                                self.arrange_effect(monitor, desktop);
                            }
                        } else {
                            self.arrange_effect(monitor, desktop);
                        }
                    }
                }
                NodeCommand::Swap { arguments } => {
                    let SelectorFollowArguments {
                        selector: argument,
                        follow,
                    } = arguments;
                    let Some(destination) = Self::selector_failure(
                        self.resolve_node(argument, reference),
                        b"node -s",
                        argument,
                        rsp,
                    )?
                    else {
                        return Ok(());
                    };
                    let (Some(first), Some(second)) = (target.node, destination.node) else {
                        return fail(rsp, b"");
                    };
                    if target.desktop != destination.desktop
                        && (self.state.world.tree.sticky_count(first) > 0
                            || self.state.world.tree.sticky_count(second) > 0)
                    {
                        return fail(rsp, b"");
                    }
                    // Both ends are used whole from here on, so resolve them
                    // once rather than unwrapping a node's implied monitor.
                    let (Some(source_monitor), Some(source_desktop)) =
                        (target.monitor, target.desktop)
                    else {
                        return fail(rsp, b"");
                    };
                    let (Some(destination_monitor), Some(destination_desktop)) =
                        (destination.monitor, destination.desktop)
                    else {
                        return fail(rsp, b"");
                    };
                    let source_rectangle = self.state.world.monitor(source_monitor).rectangle;
                    let destination_rectangle =
                        self.state.world.monitor(destination_monitor).rectangle;
                    let holds_focus = |handler: &Self, desktop, node| {
                        handler
                            .state
                            .world
                            .desktop(desktop)
                            .tree
                            .focus
                            .is_some_and(|focus| {
                                handler.state.world.tree.is_descendant(focus, node)
                            })
                    };
                    let first_held_focus = holds_focus(self, source_desktop, first);
                    let second_held_focus = holds_focus(self, destination_desktop, second);
                    if source_desktop != destination_desktop {
                        self.state
                            .history
                            .remove_node(&self.state.world.tree, first, true);
                        self.state
                            .history
                            .remove_node(&self.state.world.tree, second, true);
                    }
                    if self.state.world.swap_nodes(first, second).is_err() {
                        return fail(rsp, b"");
                    }
                    if source_monitor != destination_monitor {
                        self.adapt_subtree_geometry(first, source_rectangle, destination_rectangle);
                        self.adapt_subtree_geometry(
                            second,
                            destination_rectangle,
                            source_rectangle,
                        );
                    }
                    if source_desktop != destination_desktop {
                        let source_active = self.state.world.monitor(source_monitor).active_desktop
                            == Some(source_desktop);
                        let destination_active =
                            self.state.world.monitor(destination_monitor).active_desktop
                                == Some(destination_desktop);
                        if source_active != destination_active {
                            self.state.pending_effects.extend([
                                CommandEffect::SetWindowVisibility {
                                    node: first,
                                    visible: destination_active,
                                },
                                CommandEffect::SetWindowVisibility {
                                    node: second,
                                    visible: source_active,
                                },
                            ]);
                        }
                    }
                    let source_location = (source_monitor, source_desktop);
                    let destination_location = (destination_monitor, destination_desktop);
                    if source_location == destination_location {
                        self.structural_effects(&[source_location]);
                    } else {
                        self.structural_effects(&[source_location, destination_location]);
                    }
                    self.state.pending_effects.extend([
                        CommandEffect::Restack {
                            node: first,
                            auto_raise: self.state.auto_raise,
                        },
                        CommandEffect::Restack {
                            node: second,
                            auto_raise: self.state.auto_raise,
                        },
                    ]);
                    if source_desktop == destination_desktop
                        && self.state.settings.pointer_follows_focus
                        && (first_held_focus || second_held_focus)
                        && let Some(focus) = self.state.world.desktop(source_desktop).tree.focus
                    {
                        self.state.pending_effects.push(CommandEffect::WarpPointer {
                            rectangle: self.state.world.tree.node(focus).rectangle,
                        });
                    }
                    if follow
                        && source_desktop != destination_desktop
                        && self.state.world.desktop(source_desktop).tree.focus == Some(second)
                    {
                        let _ = self.focus_location(
                            Coordinates {
                                node: Some(first),
                                ..destination
                            },
                            false,
                        );
                    }
                    self.broadcast(
                        crate::types::SubscriberMask::NODE_SWAP,
                        format!(
                            "node_swap 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X}\n",
                            self.state.world.monitor(source_monitor).external_id,
                            self.state.world.desktop(source_desktop).external_id,
                            self.state.world.tree.node(first).external_id,
                            self.state.world.monitor(destination_monitor).external_id,
                            self.state.world.desktop(destination_desktop).external_id,
                            self.state.world.tree.node(second).external_id,
                        ),
                    );
                    target = destination;
                }
                NodeCommand::Transfer { arguments } => {
                    let NodeTransferArguments {
                        target: transfer_target,
                        selector: argument,
                        follow,
                    } = arguments;
                    let destination = if matches!(transfer_target, TransferTarget::Monitor) {
                        let Some(mut location) = Self::selector_failure(
                            self.resolve_monitor(argument, reference),
                            b"node -m",
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
                        location
                    } else if matches!(transfer_target, TransferTarget::Desktop) {
                        let Some(mut location) = Self::selector_failure(
                            self.resolve_desktop(argument, reference),
                            b"node -d",
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        location.node = location
                            .desktop
                            .and_then(|desktop| self.state.world.desktop(desktop).tree.focus);
                        location
                    } else {
                        let Some(location) = Self::selector_failure(
                            self.resolve_node(argument, reference),
                            b"node -n",
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        location
                    };
                    let (Some(node), Some(_)) = (target.node, destination.desktop) else {
                        return fail(rsp, b"");
                    };
                    let Ok(_moved) = self.transfer_node_complete(target, destination, follow)
                    else {
                        return fail(rsp, b"");
                    };
                    target = Coordinates {
                        node: Some(node),
                        ..destination
                    };
                }
                NodeCommand::Focus { arguments } => {
                    let NodeFocusArguments { activate, selector } = arguments;
                    let mut destination = target;
                    if let Some(argument) = selector {
                        let source = if activate {
                            b"node -a".as_slice()
                        } else {
                            b"node -f".as_slice()
                        };
                        let Some(location) = Self::selector_failure(
                            self.resolve_node(argument, reference),
                            source,
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        destination = location;
                    }
                    if destination.node.is_none() || !self.focus_location(destination, activate) {
                        return fail(rsp, b"");
                    }
                    target = destination;
                }
                NodeCommand::Move { arguments } => {
                    let MoveArguments { dx, dy } = arguments;
                    let node = require!(target.node, rsp);
                    let client = require!(self.state.world.tree.node(node).client.as_ref(), rsp);
                    if client.state.is_tiled() {
                        return fail(rsp, b"");
                    }
                    let rectangle =
                        crate::pointer::plan_floating_move(client.floating_rectangle, dx, dy);
                    self.state
                        .world
                        .tree
                        .node_mut(node)
                        .client
                        .as_mut()
                        .unwrap()
                        .floating_rectangle = rectangle;
                    self.state
                        .pending_effects
                        .push(CommandEffect::MoveResize { node, rectangle });
                }
                NodeCommand::Resize { arguments } => {
                    let ResizeArguments { handle, dx, dy } = arguments;
                    if !self.resize_node(target, handle, dx, dy) {
                        return fail(rsp, b"");
                    }
                }
                NodeCommand::Terminate { termination } => {
                    let close = termination.close;
                    let node = require!(target.node, rsp);
                    if close && self.state.world.tree.locked_count(node) > 0 {
                        return fail(rsp, b"");
                    }
                    if !close
                        && self.state.world.tree.node(node).client.is_none()
                        && self.state.world.tree.is_leaf(node)
                    {
                        let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop)
                        else {
                            return fail(rsp, b"");
                        };
                        self.state
                            .history
                            .remove_node(&self.state.world.tree, node, true);
                        self.state
                            .stacking_order
                            .remove_subtree(&self.state.world.tree, node);
                        self.state.world.tree.cancel_presel(node);
                        // Read before the free: the record names the receptacle
                        // that is about to stop existing.
                        let node_external_id = self.state.world.tree.node(node).external_id;
                        let mut tree = self.state.world.desktop(desktop).tree;
                        let Ok(unlink_result) = self.state.world.tree.unlink(&mut tree, node)
                        else {
                            return fail(rsp, b"");
                        };
                        if self.state.settings.removal_adjustment {
                            self.state.world.tree.apply_removal_adjustment(
                                &unlink_result,
                                self.state.settings.automatic_scheme,
                            );
                        }
                        self.state.world.tree.destroy_subtree(node);
                        self.state.forget_retired_nodes();
                        if tree.focus.is_none() {
                            tree.focus = tree
                                .root
                                .and_then(|root| self.state.world.tree.first_focusable_leaf(root));
                        }
                        self.state.world.desktop_mut(desktop).tree = tree;
                        self.structural_effects(&[(monitor, desktop)]);
                        self.broadcast(
                            crate::types::SubscriberMask::NODE_REMOVE,
                            format!(
                                "node_remove 0x{:08X} 0x{:08X} 0x{node_external_id:08X}\n",
                                self.state.world.monitor(monitor).external_id,
                                self.state.world.desktop(desktop).external_id,
                            ),
                        );
                        self.report_effect();
                        return Ok(());
                    }
                    self.state.pending_effects.push(if close {
                        CommandEffect::Close { node }
                    } else {
                        CommandEffect::Kill { node }
                    });
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
