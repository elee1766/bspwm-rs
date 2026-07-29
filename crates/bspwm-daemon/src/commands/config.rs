use std::io;

use super::effects::queue_layout_effect;
use super::{
    ArgCursor, CommandEffect, CommandHandler, Coordinates, Response, fail, fail_parts, not_enough,
    text, unknown_option,
};
use crate::parse::{
    is_hex_color, parse_automatic_scheme, parse_bool, parse_button_index, parse_child_polarity,
    parse_honor_size_hints_mode, parse_pointer_action, parse_pointer_modifier,
    parse_state_transition, parse_tightness,
};
use crate::settings::Settings;
use crate::state::DaemonState;
use crate::types::{
    AutomaticScheme, ButtonIndex, ChildPolarity, HonorSizeHintsMode, Layout, PointerModifier,
    StateTransitions, Tightness,
};

enum ConfigScopeKind {
    Monitor,
    Desktop,
    Node,
}

struct ConfigScope<'a> {
    kind: ConfigScopeKind,
    descriptor: &'a [u8],
}

enum ConfigScopeError<'a> {
    MissingDescriptor(&'a [u8]),
    UnknownOption(&'a [u8]),
}

impl<'a> ConfigScope<'a> {
    fn next(cursor: &mut ArgCursor<'_, 'a>) -> Result<Option<Self>, ConfigScopeError<'a>> {
        let Some(option) = cursor.peek().filter(|argument| argument.starts_with(b"-")) else {
            return Ok(None);
        };
        cursor.next();
        let kind = match option {
            b"-m" | b"--monitor" => ConfigScopeKind::Monitor,
            b"-d" | b"--desktop" => ConfigScopeKind::Desktop,
            b"-n" | b"--node" => ConfigScopeKind::Node,
            _ => return Err(ConfigScopeError::UnknownOption(option)),
        };
        let Some(descriptor) = cursor.next() else {
            return Err(ConfigScopeError::MissingDescriptor(option));
        };
        Ok(Some(Self { kind, descriptor }))
    }
}

enum ConfigRequest<'a> {
    Get { name: &'a [u8] },
    Set { name: &'a [u8], value: &'a [u8] },
}

impl CommandHandler<'_> {
    pub(super) fn handle_config(
        &mut self,
        args: &[&[u8]],
        rsp: &mut dyn Response,
    ) -> io::Result<()> {
        if args.is_empty() {
            return fail(rsp, b"config: Missing arguments.\n");
        }
        let reference = self.reference();
        let mut target = Coordinates::default();
        let mut cursor = ArgCursor::new(args);
        loop {
            let scope = match ConfigScope::next(&mut cursor) {
                Ok(Some(scope)) => scope,
                Ok(None) => break,
                Err(ConfigScopeError::MissingDescriptor(option)) => {
                    return not_enough(rsp, b"config", option);
                }
                Err(ConfigScopeError::UnknownOption(option)) => {
                    return unknown_option(rsp, b"config", option);
                }
            };
            let source = match scope.kind {
                ConfigScopeKind::Monitor => b"config -m".as_slice(),
                ConfigScopeKind::Desktop => b"config -d".as_slice(),
                ConfigScopeKind::Node => b"config -n".as_slice(),
            };
            let result = match scope.kind {
                ConfigScopeKind::Monitor => self.resolve_monitor(scope.descriptor, reference),
                ConfigScopeKind::Desktop => self.resolve_desktop(scope.descriptor, reference),
                ConfigScopeKind::Node => self.resolve_node(scope.descriptor, reference),
            };
            let Some(location) = Self::selector_failure(result, source, scope.descriptor, rsp)?
            else {
                return Ok(());
            };
            target = location;
        }
        let request = match cursor.remaining() {
            [name] => ConfigRequest::Get { name },
            [name, value] => ConfigRequest::Set { name, value },
            arguments => {
                return fail_parts(
                    rsp,
                    &[
                        b"config: Was expecting 1 or 2 arguments, received ",
                        arguments.len().to_string().as_bytes(),
                        b".\n",
                    ],
                );
            }
        };
        match request {
            ConfigRequest::Get { name } => self.get_setting(target, name, rsp),
            ConfigRequest::Set { name, value } => self.set_setting(target, name, value, rsp),
        }
    }

    fn get_setting(
        &self,
        target: Coordinates,
        name: &[u8],
        rsp: &mut dyn Response,
    ) -> io::Result<()> {
        let Some(name) = text(name) else {
            return unknown_setting(rsp, name);
        };
        let settings = &self.state.settings;
        let value = match name {
            "top_padding" | "right_padding" | "bottom_padding" | "left_padding" => {
                scoped_padding(self.state, target, name).to_string()
            }
            "window_gap" => scoped_window_gap(self.state, target).to_string(),
            "border_width" => {
                if target.node.is_some() {
                    scoped_client_value(self.state, target, |client| {
                        client.border_width.to_string()
                    })
                    .unwrap_or_default()
                } else {
                    target.desktop.map_or_else(
                        || {
                            target.monitor.map_or_else(
                                || settings.border_width.to_string(),
                                |monitor| {
                                    self.state.world.monitor(monitor).border_width.to_string()
                                },
                            )
                        },
                        |desktop| self.state.world.desktop(desktop).border_width.to_string(),
                    )
                }
            }
            "honor_size_hints" => {
                if target.monitor.is_none() {
                    settings.honor_size_hints.protocol_name().into()
                } else {
                    scoped_client_value(self.state, target, |client| {
                        client.honor_size_hints.protocol_name().into()
                    })
                    .unwrap_or_default()
                }
            }
            _ => match pointer_action_index(name) {
                Some(index) => settings.pointer_actions[index].protocol_name().into(),
                None => match get_table_setting(settings, name) {
                    Some(value) => value,
                    None => return unknown_setting(rsp, name.as_bytes()),
                },
            },
        };
        writeln!(rsp, "{value}")
    }

    fn set_setting(
        &mut self,
        target: Coordinates,
        name: &[u8],
        value: &[u8],
        rsp: &mut dyn Response,
    ) -> io::Result<()> {
        let (Some(name), Some(value)) = (text(name), text(value)) else {
            return invalid_setting(rsp, name, value);
        };
        macro_rules! parsed {
            ($parser:expr) => {
                match $parser(value) {
                    Some(value) => value,
                    None => return invalid_setting(rsp, name.as_bytes(), value.as_bytes()),
                }
            };
        }
        match name {
            "border_width" => {
                set_scoped_border_width(self.state, target, parsed!(parse_integer::<u32>));
            }
            "honor_size_hints" => {
                set_scoped_honor_size_hints(
                    self.state,
                    target,
                    parsed!(parse_honor_size_hints_mode),
                );
            }
            "window_gap" => {
                set_scoped_window_gap(self.state, target, parsed!(parse_integer::<i32>));
            }
            "top_padding" | "right_padding" | "bottom_padding" | "left_padding" => {
                set_scoped_padding(self.state, target, name, parsed!(parse_integer::<i32>));
            }
            _ => {
                let settings = &mut self.state.settings;
                let applied = match pointer_action_index(name) {
                    Some(index) => {
                        settings.pointer_actions[index] = parsed!(parse_pointer_action);
                        true
                    }
                    None => match set_table_setting(settings, name, value) {
                        Some(applied) => applied,
                        None => return unknown_setting(rsp, name.as_bytes()),
                    },
                };
                if !applied {
                    return invalid_setting(rsp, name.as_bytes(), value.as_bytes());
                }
                propagate_global_setting(self.state, name);
                queue_config_effects(self.state, name);
                return Ok(());
            }
        }
        queue_config_effects(self.state, name);
        Ok(())
    }
}

/// Rows of `(protocol name, settings field, parser, printer)`, driving both
/// `config <name>` and `config <name> <value>` from a single table.
macro_rules! settings_table {
    (
        $(($name:literal, $($field:ident).+, $parse:expr, $print:expr),)*
        bools: $($flag:ident),+ $(,)?
    ) => {
        fn get_table_setting(settings: &Settings, name: &str) -> Option<String> {
            match name {
                $($name => Some($print(&settings.$($field).+)),)*
                $(stringify!($flag) => Some(bool_string(settings.$flag).to_owned()),)+
                _ => None,
            }
        }

        /// `None` when the setting is unknown, `Some(false)` when the value is invalid.
        fn set_table_setting(settings: &mut Settings, name: &str, value: &str) -> Option<bool> {
            match name {
                $($name => Some(match $parse(value) {
                    Some(parsed) => {
                        settings.$($field).+ = parsed;
                        true
                    }
                    None => false,
                }),)*
                $(stringify!($flag) => Some(match parse_bool(value) {
                    Some(parsed) => {
                        settings.$flag = parsed;
                        true
                    }
                    None => false,
                }),)+
                _ => None,
            }
        }
    };
}

settings_table! {
    ("external_rules_command", external_rules_command, |value: &str| Some(value.to_owned()), String::clone),
    ("status_prefix", status_prefix, |value: &str| Some(value.to_owned()), String::clone),
    ("normal_border_color", normal_border_color, parse_color, String::clone),
    ("active_border_color", active_border_color, parse_color, String::clone),
    ("focused_border_color", focused_border_color, parse_color, String::clone),
    ("presel_feedback_color", presel_feedback_color, parse_color, String::clone),
    ("top_monocle_padding", monocle_padding.top, parse_integer::<i32>, i32::to_string),
    ("right_monocle_padding", monocle_padding.right, parse_integer::<i32>, i32::to_string),
    ("bottom_monocle_padding", monocle_padding.bottom, parse_integer::<i32>, i32::to_string),
    ("left_monocle_padding", monocle_padding.left, parse_integer::<i32>, i32::to_string),
    ("split_ratio", split_ratio, parse_split_ratio, |value: &f64| format!("{value:.6}")),
    ("initial_polarity", initial_polarity, parse_child_polarity, ChildPolarity::to_string),
    ("automatic_scheme", automatic_scheme, parse_automatic_scheme, AutomaticScheme::to_string),
    ("directional_focus_tightness", directional_focus_tightness, parse_tightness, Tightness::to_string),
    ("pointer_modifier", pointer_modifier, parse_pointer_modifier, PointerModifier::to_string),
    ("pointer_motion_interval", pointer_motion_interval, parse_integer::<u32>, u32::to_string),
    ("mapping_events_count", mapping_events_count, parse_integer::<i8>, i8::to_string),
    ("click_to_focus", click_to_focus, parse_button_index, ButtonIndex::to_string),
    ("ignore_ewmh_fullscreen", ignore_ewmh_fullscreen, parse_state_transition, StateTransitions::to_string),
    bools:
        removal_adjustment,
        presel_feedback,
        borderless_monocle,
        gapless_monocle,
        single_monocle,
        borderless_singleton,
        borderless_csd,
        focus_follows_pointer,
        pointer_follows_focus,
        pointer_follows_monitor,
        pointer_resize_sync,
        swallow_first_click,
        enable_ewmh_ping,
        enable_ewmh_allowed_actions,
        ignore_ewmh_focus,
        ignore_ewmh_struts,
        center_pseudo_tiled,
        remove_disabled_monitors,
        remove_unplugged_monitors,
        merge_overlapping_monitors,
}

fn parse_color(value: &str) -> Option<String> {
    is_hex_color(value).then(|| value.to_owned())
}

fn parse_integer<T: std::str::FromStr>(value: &str) -> Option<T> {
    value.parse().ok()
}

fn parse_split_ratio(value: &str) -> Option<f64> {
    parse_integer::<f64>(value).filter(|ratio| (0.0..1.0).contains(ratio))
}

/// Index of `pointer_action1`..`pointer_action3`, without indexing past the name.
fn pointer_action_index(name: &str) -> Option<usize> {
    match name.strip_prefix("pointer_action") {
        Some("1") => Some(0),
        Some("2") => Some(1),
        Some("3") => Some(2),
        _ => None,
    }
}

fn queue_config_effects(state: &mut DaemonState, name: &str) {
    if matches!(
        name,
        "split_ratio" | "pointer_motion_interval" | "pointer_resize_sync"
    ) {
        return;
    }
    if name == "enable_ewmh_allowed_actions" {
        state
            .pending_effects
            .push(CommandEffect::RefreshEwmhAllowedActions);
        return;
    }
    if name == "enable_ewmh_ping" {
        return;
    }
    if name == "ignore_ewmh_fullscreen" && state.settings.enable_ewmh_allowed_actions {
        state
            .pending_effects
            .push(CommandEffect::RefreshEwmhAllowedActions);
    }
    if matches!(
        name,
        "pointer_modifier"
            | "pointer_action1"
            | "pointer_action2"
            | "pointer_action3"
            | "click_to_focus"
    ) {
        state.pending_effects.push(CommandEffect::RegrabButtons);
    }
    if matches!(
        name,
        "normal_border_color"
            | "active_border_color"
            | "focused_border_color"
            | "presel_feedback_color"
    ) {
        state.pending_effects.push(CommandEffect::RefreshColors);
    }
    if matches!(
        name,
        "remove_disabled_monitors" | "remove_unplugged_monitors" | "merge_overlapping_monitors"
    ) && match name {
        "remove_disabled_monitors" => state.settings.remove_disabled_monitors,
        "remove_unplugged_monitors" => state.settings.remove_unplugged_monitors,
        _ => state.settings.merge_overlapping_monitors,
    } {
        state.pending_effects.push(CommandEffect::RefreshMonitors);
    }
    if name == "focus_follows_pointer" {
        state
            .pending_effects
            .push(CommandEffect::RefreshFocusFollowsPointer);
        return;
    }
    let arrangements: Vec<_> = state
        .world
        .desktops()
        .map(|(monitor, desktop)| CommandEffect::Arrange { monitor, desktop })
        .collect();
    state.pending_effects.extend(arrangements);
    if matches!(
        name,
        "top_padding" | "right_padding" | "bottom_padding" | "left_padding"
    ) {
        state.pending_effects.push(CommandEffect::SyncEwmh);
    }
}

fn propagate_global_setting(state: &mut DaemonState, name: &str) {
    if name != "single_monocle" {
        return;
    }
    for (monitor, desktop) in state.world.desktops().collect::<Vec<_>>() {
        let value = state.world.desktop(desktop);
        let previous = value.layout;
        let layout = if state.settings.single_monocle
            && value
                .tree
                .root
                .map_or(0, |root| state.world.tree.tiled_count(root, true))
                <= 1
        {
            Layout::Monocle
        } else {
            value.user_layout
        };
        state
            .world
            .set_layout(desktop, layout, false, state.settings.single_monocle);
        queue_layout_effect(state, monitor, desktop, previous);
    }
}

fn target_roots(state: &DaemonState, target: Coordinates) -> Vec<crate::tree::NodeId> {
    if let Some(node) = target.node {
        return vec![node];
    }
    if let Some(desktop) = target.desktop {
        return state.world.desktop(desktop).tree.root.into_iter().collect();
    }
    if let Some(monitor) = target.monitor {
        return state
            .world
            .monitor(monitor)
            .desktops
            .iter()
            .filter_map(|desktop| state.world.desktop(*desktop).tree.root)
            .collect();
    }
    state.world.roots().map(|(_, _, root)| root).collect()
}

fn client_leaves(state: &DaemonState, target: Coordinates) -> Vec<crate::tree::NodeId> {
    target_roots(state, target)
        .into_iter()
        .flat_map(|root| state.world.tree.leaves(root))
        .filter(|node| state.world.tree.node(*node).client.is_some())
        .collect()
}

fn scoped_client_value(
    state: &DaemonState,
    target: Coordinates,
    value: impl FnOnce(&crate::tree::Client) -> String,
) -> Option<String> {
    let node = client_leaves(state, target).into_iter().next()?;
    Some(value(
        state
            .world
            .tree
            .node(node)
            .client
            .as_ref()
            .expect("client leaf"),
    ))
}

fn set_scoped_border_width(state: &mut DaemonState, target: Coordinates, value: u32) {
    if let Some(node) = target.node {
        for node in client_leaves(
            state,
            Coordinates {
                node: Some(node),
                ..target
            },
        ) {
            state
                .world
                .tree
                .node_mut(node)
                .client
                .as_mut()
                .expect("client leaf")
                .border_width = value;
        }
        return;
    }
    if let Some(desktop) = target.desktop {
        state.world.desktop_mut(desktop).border_width = value;
    } else if let Some(monitor) = target.monitor {
        state.world.monitor_mut(monitor).border_width = value;
        for desktop in state.world.monitor(monitor).desktops.clone() {
            state.world.desktop_mut(desktop).border_width = value;
        }
    } else {
        state.settings.border_width = value;
        for monitor in state.world.monitor_order().to_vec() {
            state.world.monitor_mut(monitor).border_width = value;
            for desktop in state.world.monitor(monitor).desktops.clone() {
                state.world.desktop_mut(desktop).border_width = value;
            }
        }
    }
    for node in client_leaves(state, target) {
        state
            .world
            .tree
            .node_mut(node)
            .client
            .as_mut()
            .expect("client leaf")
            .border_width = value;
    }
}

fn set_scoped_honor_size_hints(
    state: &mut DaemonState,
    target: Coordinates,
    value: HonorSizeHintsMode,
) {
    if target.monitor.is_none() {
        state.settings.honor_size_hints = value;
    }
    for node in client_leaves(state, target) {
        state
            .world
            .tree
            .node_mut(node)
            .client
            .as_mut()
            .expect("client leaf")
            .honor_size_hints = value;
    }
}

fn scoped_window_gap(state: &DaemonState, target: Coordinates) -> i32 {
    target.desktop.map_or_else(
        || {
            target.monitor.map_or(state.settings.window_gap, |monitor| {
                state.world.monitor(monitor).window_gap
            })
        },
        |desktop| state.world.desktop(desktop).window_gap,
    )
}

fn set_scoped_window_gap(state: &mut DaemonState, target: Coordinates, value: i32) {
    if let Some(desktop) = target.desktop {
        state.world.desktop_mut(desktop).window_gap = value;
    } else if let Some(monitor) = target.monitor {
        state.world.monitor_mut(monitor).window_gap = value;
        for desktop in state.world.monitor(monitor).desktops.clone() {
            state.world.desktop_mut(desktop).window_gap = value;
        }
    } else {
        state.settings.window_gap = value;
        for monitor in state.world.monitor_order().to_vec() {
            state.world.monitor_mut(monitor).window_gap = value;
            for desktop in state.world.monitor(monitor).desktops.clone() {
                state.world.desktop_mut(desktop).window_gap = value;
            }
        }
    }
}

fn scoped_padding(state: &DaemonState, target: Coordinates, name: &str) -> i32 {
    let padding = target.desktop.map_or_else(
        || {
            target.monitor.map_or(state.settings.padding, |monitor| {
                state.world.monitor(monitor).padding
            })
        },
        |desktop| state.world.desktop(desktop).padding,
    );
    match name {
        "top_padding" => padding.top,
        "right_padding" => padding.right,
        "bottom_padding" => padding.bottom,
        _ => padding.left,
    }
}

fn set_scoped_padding(state: &mut DaemonState, target: Coordinates, name: &str, value: i32) {
    let padding = if let Some(desktop) = target.desktop {
        &mut state.world.desktop_mut(desktop).padding
    } else if let Some(monitor) = target.monitor {
        &mut state.world.monitor_mut(monitor).padding
    } else {
        match name {
            "top_padding" => state.settings.padding.top = value,
            "right_padding" => state.settings.padding.right = value,
            "bottom_padding" => state.settings.padding.bottom = value,
            _ => state.settings.padding.left = value,
        }
        for monitor in state.world.monitor_order().to_vec() {
            set_padding_side(&mut state.world.monitor_mut(monitor).padding, name, value);
        }
        return;
    };
    set_padding_side(padding, name, value);
}

fn set_padding_side(padding: &mut crate::types::Padding, name: &str, value: i32) {
    match name {
        "top_padding" => padding.top = value,
        "right_padding" => padding.right = value,
        "bottom_padding" => padding.bottom = value,
        _ => padding.left = value,
    }
}

fn invalid_setting(rsp: &mut dyn Response, name: &[u8], value: &[u8]) -> io::Result<()> {
    fail_parts(
        rsp,
        &[b"config: ", name, b": Invalid value: '", value, b"'.\n"],
    )
}

fn unknown_setting(rsp: &mut dyn Response, name: &[u8]) -> io::Result<()> {
    fail_parts(rsp, &[b"config: Unknown setting: '", name, b"'.\n"])
}

const fn bool_string(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every accepted spelling, exactly as upstream advertises them.
    const TABLE_CASES: &[(&str, &str)] = &[
        ("external_rules_command", "/bin/true"),
        ("status_prefix", "X"),
        ("normal_border_color", "#123456"),
        ("active_border_color", "#123456"),
        ("focused_border_color", "#123456"),
        ("presel_feedback_color", "#123456"),
        ("top_monocle_padding", "-3"),
        ("right_monocle_padding", "4"),
        ("bottom_monocle_padding", "5"),
        ("left_monocle_padding", "6"),
        ("split_ratio", "0.250000"),
        ("initial_polarity", "first_child"),
        ("automatic_scheme", "spiral"),
        ("directional_focus_tightness", "low"),
        ("pointer_modifier", "mod4"),
        ("pointer_motion_interval", "20"),
        ("pointer_resize_sync", "true"),
        ("mapping_events_count", "-1"),
        ("click_to_focus", "button3"),
        ("ignore_ewmh_fullscreen", "enter,exit"),
        ("removal_adjustment", "false"),
        ("presel_feedback", "false"),
        ("borderless_monocle", "true"),
        ("gapless_monocle", "true"),
        ("single_monocle", "true"),
        ("borderless_singleton", "true"),
        ("borderless_csd", "true"),
        ("focus_follows_pointer", "true"),
        ("pointer_follows_focus", "true"),
        ("pointer_follows_monitor", "true"),
        ("swallow_first_click", "true"),
        ("enable_ewmh_ping", "true"),
        ("enable_ewmh_allowed_actions", "true"),
        ("ignore_ewmh_focus", "true"),
        ("ignore_ewmh_struts", "true"),
        ("center_pseudo_tiled", "false"),
        ("remove_disabled_monitors", "true"),
        ("remove_unplugged_monitors", "true"),
        ("merge_overlapping_monitors", "true"),
    ];

    #[test]
    fn table_settings_round_trip_through_both_directions() {
        for &(name, value) in TABLE_CASES {
            let mut settings = Settings::default();
            assert_eq!(
                set_table_setting(&mut settings, name, value),
                Some(true),
                "{name} set"
            );
            assert_eq!(
                get_table_setting(&settings, name).as_deref(),
                Some(value),
                "{name} get"
            );
        }
    }

    #[test]
    fn table_rejects_invalid_values_without_mutating() {
        let mut settings = Settings::default();
        for &(name, _) in TABLE_CASES {
            if matches!(name, "external_rules_command" | "status_prefix") {
                continue;
            }
            assert_eq!(
                set_table_setting(&mut settings, name, "\u{fffd}bogus"),
                Some(false),
                "{name} invalid"
            );
        }
        assert_eq!(settings, Settings::default());
        assert_eq!(set_table_setting(&mut settings, "border_width", "1"), None);
        assert_eq!(get_table_setting(&settings, "pointer_action1"), None);
    }

    #[test]
    fn pointer_action_names_never_index_out_of_bounds() {
        assert_eq!(pointer_action_index("pointer_action1"), Some(0));
        assert_eq!(pointer_action_index("pointer_action3"), Some(2));
        for name in [
            "pointer_action",
            "pointer_action0",
            "pointer_action4",
            "pointer_action11",
            "pointer",
            "",
        ] {
            assert_eq!(pointer_action_index(name), None, "{name}");
        }
    }

    #[test]
    fn split_ratio_keeps_its_half_open_range() {
        assert_eq!(parse_split_ratio("0"), Some(0.0));
        assert_eq!(parse_split_ratio("1"), None);
        assert_eq!(parse_split_ratio("-0.1"), None);
        assert_eq!(parse_split_ratio("0.5"), Some(0.5));
    }
}
