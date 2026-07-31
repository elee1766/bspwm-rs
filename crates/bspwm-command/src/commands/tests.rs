use super::*;
use crate::messages::{Response, process_message};
use crate::query::find_by_id;
use crate::tree::Client;
use crate::types::{HonorSizeHintsMode, Rectangle};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

#[derive(Default)]
struct TestResponse {
    bytes: Vec<u8>,
    closed: bool,
}

impl Write for TestResponse {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Response for TestResponse {
    fn close(&mut self) -> io::Result<()> {
        self.closed = true;
        Ok(())
    }
}

fn fixture() -> DaemonState {
    let mut state = DaemonState::default();
    let monitor = state.world.create_monitor(
        0x10,
        Some("main"),
        Rectangle::new(0, 0, 800, 600),
        &state.settings,
    );
    let first = state.world.create_desktop(0x20, Some("I"), &state.settings);
    let second = state
        .world
        .create_desktop(0x21, Some("II"), &state.settings);
    state.world.add_desktop(monitor, first);
    state.world.add_desktop(monitor, second);
    let root = state.world.tree.add_node(0x30, 0.4);
    let left = state.world.tree.add_node(0x31, 0.5);
    let right = state.world.tree.add_node(0x32, 0.5);
    state.world.tree.set_children(root, left, right);
    state.world.tree.node_mut(left).client = Some(Client::from_settings(&state.settings));
    state.world.desktop_mut(first).tree.root = Some(root);
    state.world.desktop_mut(first).tree.focus = Some(left);
    state.clients_count = 1;
    state.history.add(
        crate::history::Coordinates {
            monitor,
            desktop: first,
            node: Some(left),
        },
        true,
    );
    {
        struct Noop;
        impl bspwm_xstack::StackBackend for Noop {
            type Error = ();
            fn stack_above(&mut self, _: u32, _: u32) -> Result<(), ()> {
                Ok(())
            }
            fn stack_below(&mut self, _: u32, _: u32) -> Result<(), ()> {
                Ok(())
            }
        }
        let xid = state.world.tree.node(left).external_id;
        let level = crate::stack::stack_level(state.world.tree.node(left).client.as_ref().unwrap());
        let _ = state.stacking_order.insert(&mut Noop, xid, level);
    }
    state
}

/// The client a node holds; every caller here builds the node with one.
fn client(state: &DaemonState, node: crate::tree::NodeId) -> &Client {
    state
        .world
        .tree
        .node(node)
        .client
        .as_ref()
        .expect("node holds a client")
}

fn set_tiled_rectangle(state: &mut DaemonState, node: crate::tree::NodeId, rectangle: Rectangle) {
    state
        .world
        .tree
        .node_mut(node)
        .client
        .as_mut()
        .expect("node holds a client")
        .tiled_rectangle = rectangle;
}

fn descriptor_fixture() -> DaemonState {
    let mut state = fixture();
    let main = state.world.focused_monitor.unwrap();
    let first = state.world.monitor(main).desktops[0];
    let second = state.world.monitor(main).desktops[1];
    let left = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    let right = find_by_id(&state.world, 0x32).unwrap().node.unwrap();

    set_tiled_rectangle(&mut state, left, Rectangle::new(0, 0, 100, 100));
    state.world.tree.node_mut(right).client = Some(Client::from_settings(&state.settings));
    set_tiled_rectangle(&mut state, right, Rectangle::new(200, 0, 100, 100));
    state.clients_count += 1;

    let second_node = state.world.tree.add_node(0x40, 0.5);
    state.world.tree.node_mut(second_node).client = Some(Client::from_settings(&state.settings));
    set_tiled_rectangle(&mut state, second_node, Rectangle::new(0, 0, 80, 80));
    state.world.desktop_mut(second).tree.root = Some(second_node);
    state.world.desktop_mut(second).tree.focus = Some(second_node);
    state.clients_count += 1;

    let gap = state.world.create_monitor(
        0x11,
        Some("gap"),
        Rectangle::new(800, 0, 800, 600),
        &state.settings,
    );
    let empty = state
        .world
        .create_desktop(0x22, Some("empty"), &state.settings);
    assert!(state.world.add_desktop(gap, empty));

    let far = state.world.create_monitor(
        0x12,
        Some("right"),
        Rectangle::new(1600, 0, 800, 600),
        &state.settings,
    );
    let third = state
        .world
        .create_desktop(0x23, Some("III"), &state.settings);
    assert!(state.world.add_desktop(far, third));
    let third_node = state.world.tree.add_node(0x50, 0.5);
    state.world.tree.node_mut(third_node).client = Some(Client::from_settings(&state.settings));
    set_tiled_rectangle(&mut state, third_node, Rectangle::new(1600, 0, 100, 100));
    state.world.desktop_mut(third).tree.root = Some(third_node);
    state.world.desktop_mut(third).tree.focus = Some(third_node);
    state.clients_count += 1;

    for (monitor, desktop, node) in [
        (main, first, left),
        (main, second, second_node),
        (far, third, third_node),
    ] {
        state.history.add(
            crate::history::Coordinates {
                monitor,
                desktop,
                node: Some(node),
            },
            true,
        );
    }
    state
}

fn run(state: &mut DaemonState, args: &[&[u8]]) -> Vec<u8> {
    let mut response = TestResponse::default();
    process_message(args, 0, &mut CommandHandler::new(state), &mut response).unwrap();
    assert!(response.closed);
    response.bytes
}

#[test]
fn query_ids_names_and_tree_cover_each_domain() {
    let mut state = fixture();
    assert_eq!(run(&mut state, &[b"query", b"-M"]), b"0x00000010\n");
    assert_eq!(run(&mut state, &[b"query", b"-D", b"--names"]), b"I\nII\n");
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-d", b"I"]),
        b"0x00000030\n0x00000031\n0x00000032\n"
    );
    let tree = run(&mut state, &[b"query", b"-T", b"-n", b"0x31"]);
    assert!(tree.starts_with(b"{\"id\":49,"));
    assert!(tree.ends_with(b"\n"));
}

#[test]
fn descriptors_cover_global_qualified_cycle_direction_and_paths() {
    let mut state = descriptor_fixture();
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"-d", b"^3"]),
        b"0x00000022\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"-d", b"right:^1"]),
        b"0x00000023\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"query", b"-D", b"-d", b"next.active.occupied"]
        ),
        b"0x00000023\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"query", b"-D", b"-d", b"III#prev.active.occupied"]
        ),
        b"0x00000020\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-M", b"-m", b"east.occupied"]),
        b"0x00000012\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"query", b"-M", b"-m", b"right#prev.occupied"]
        ),
        b"0x00000010\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"next.window"]),
        b"0x00000032\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"east.window"]),
        b"0x00000032\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"any"]),
        b"0x00000030\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"smallest"]),
        b"0x00000040\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"@/2"]),
        b"0x00000032\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"@right:^1:/"]),
        b"0x00000050\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"query", b"-N", b"-n", b"0x32#first_ancestor"]
        ),
        b"0x00000030\n"
    );
}

#[test]
fn history_and_optional_query_references_match_upstream_roles() {
    let mut state = descriptor_fixture();
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b"newest"]),
        b"0x00000050\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"-d", b"last"]),
        b"0x00000023\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-M", b"-m", b"newest"]),
        b"0x00000012\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"query", b"-N", b"0x32", b"-n", b".ancestor_of"]
        ),
        b"0x00000030\n0x00000032\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"III", b"-d", b".local"]),
        b"0x00000023\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"query", b"-M", b"right", b"-m", b"prev.occupied"]
        ),
        b"0x00000010\n"
    );
}

#[test]
fn descriptor_diagnostics_distinguish_malformed_from_unmatched() {
    let mut state = descriptor_fixture();
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"-d", b"missing"]),
        b"\x07query -d: Invalid descriptor found in 'missing'.\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-M", b"-m", b"^99"]),
        b"\x07query -m: Invalid descriptor found in '^99'.\n"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"-d", b"%missing"]),
        b"\x07"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-D", b"-d", b"I.!active"]),
        b"\x07"
    );
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"missing"]),
        b"\x07query -N: Invalid descriptor found in 'missing'.\n"
    );
    assert_eq!(run(&mut state, &[b"query", b"-N", b"0xDEAD"]), b"\x07");
}

#[test]
fn wm_dump_load_is_deferred_for_the_x_aware_daemon() {
    let mut state = fixture();
    let dump = run(&mut state, &[b"wm", b"--dump-state"]);
    assert!(dump.starts_with(b"{\"focusedMonitorId\":16,"));
    assert!(run(&mut state, &[b"wm", b"--get-status"]).starts_with(b"WMmain"));
    assert!(run(&mut state, &[b"wm", b"-h", b"off"]).is_empty());
    assert!(!state.record_history);
    let path = std::env::temp_dir().join(format!(
        "bspwm-rs-load-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, dump.strip_suffix(b"\n").unwrap()).unwrap();
    assert!(run(&mut state, &[b"wm", b"-l", path.as_os_str().as_bytes()]).is_empty());
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        state.pending_effects.last(),
        Some(CommandEffect::LoadState { .. })
    ));
    assert_eq!(state.clients_count, 1);
    assert!(!state.history.recording);
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn wm_typed_parser_preserves_diagnostics_and_incremental_execution() {
    let mut state = fixture();

    assert_eq!(run(&mut state, &[b"wm"]), b"\x07wm: Missing commands.\n");
    assert_eq!(
        run(&mut state, &[b"wm", b"-h"]),
        b"\x07wm -h: Not enough arguments.\n"
    );
    assert_eq!(
        run(&mut state, &[b"wm", b"--record-history", b"maybe"]),
        b"\x07wm --record-history: Invalid argument: 'maybe'.\n"
    );
    assert_eq!(
        run(&mut state, &[b"wm", b"--unknown\xff"]),
        b"\x07wm: Unknown command: '--unknown\xff'.\n"
    );

    state.set_record_history(true);
    assert_eq!(
        run(
            &mut state,
            &[b"wm", b"--record-history", b"off", b"--unknown"]
        ),
        b"\x07wm: Unknown command: '--unknown'.\n"
    );
    assert!(!state.record_history);

    // Upstream reports the rectangle token when either add-monitor value is invalid.
    assert_eq!(
        run(
            &mut state,
            &[b"wm", b"--add-monitor", b"bad\xff", b"1x1+0+0"]
        ),
        b"\x07wm --add-monitor: Invalid argument: '1x1+0+0'.\n"
    );
}

#[test]
fn typed_command_sets_preserve_argument_precedence_and_empty_world_failures() {
    let mut state = fixture();
    assert_eq!(
        run(&mut state, &[b"node", b"--move", b"bad"]),
        b"\x07node --move: Not enough arguments.\n"
    );
    assert_eq!(
        run(&mut state, &[b"node", b"--resize", b"bad", b"1"]),
        b"\x07node --resize: Not enough arguments.\n"
    );
    assert_eq!(
        run(&mut state, &[b"node", b"--close", b"--focus"]),
        b"\x07node --close: Trailing commands.\n"
    );
    assert_eq!(
        run(&mut state, &[b"rule", b"--add", b"bad\xff", b"state=tiled"]),
        b"\x07rule --add: Not enough arguments.\n"
    );

    let mut empty = DaemonState::default();
    assert_eq!(run(&mut empty, &[b"desktop", b"--layout", b"bad"]), b"\x07");
    assert_eq!(
        run(&mut empty, &[b"desktop", b"--bubble", b"next"]),
        b"\x07desktop --bubble: Invalid argument: 'next'.\n"
    );
    assert_eq!(
        run(&mut empty, &[b"monitor", b"--rectangle", b"1x1+0+0"]),
        b"\x07monitor --rectangle: Invalid argument: '1x1+0+0'.\n"
    );
    for command in [
        b"--add-desktops".as_slice(),
        b"--reset-desktops".as_slice(),
        b"--reorder-desktops".as_slice(),
    ] {
        assert_eq!(run(&mut empty, &[b"monitor", command]), b"\x07");
    }

    let mut state = fixture();
    assert_eq!(
        run(&mut state, &[b"monitor", b"--add-desktops"]),
        b"\x07monitor --add-desktops: Not enough arguments.\n"
    );
}

#[test]
fn rule_add_list_and_all_removal_forms_preserve_aliases() {
    let mut state = fixture();
    run(
        &mut state,
        &[b"rule", b"--add", b"A::", b"focus=off", b"--one-shot"],
    );
    run(&mut state, &[b"rule", b"-a", b"B:*:*", b"state=floating"]);
    assert_eq!(
        run(&mut state, &[b"rule", b"--list"]),
        b"A:*:* -> focus=off\nB:*:* => state=floating\n"
    );
    run(&mut state, &[b"rule", b"-r", b"^1"]);
    run(&mut state, &[b"rule", b"--remove", b"tail"]);
    assert!(state.rules.is_empty());
}

#[test]
fn every_global_setting_round_trips() {
    let mut state = fixture();
    let cases: &[(&[u8], &[u8], &[u8])] = &[
        (b"external_rules_command", b"rule-helper", b"rule-helper\n"),
        (b"status_prefix", b"S", b"S\n"),
        (b"normal_border_color", b"#112233", b"#112233\n"),
        (b"active_border_color", b"#223344", b"#223344\n"),
        (b"focused_border_color", b"#334455", b"#334455\n"),
        (b"presel_feedback_color", b"#445566", b"#445566\n"),
        (b"top_padding", b"1", b"1\n"),
        (b"right_padding", b"2", b"2\n"),
        (b"bottom_padding", b"3", b"3\n"),
        (b"left_padding", b"4", b"4\n"),
        (b"top_monocle_padding", b"5", b"5\n"),
        (b"right_monocle_padding", b"6", b"6\n"),
        (b"bottom_monocle_padding", b"7", b"7\n"),
        (b"left_monocle_padding", b"8", b"8\n"),
        (b"window_gap", b"9", b"9\n"),
        (b"border_width", b"2", b"2\n"),
        (b"split_ratio", b"0.6", b"0.600000\n"),
        (b"initial_polarity", b"first_child", b"first_child\n"),
        (b"automatic_scheme", b"spiral", b"spiral\n"),
        (b"removal_adjustment", b"off", b"false\n"),
        (b"directional_focus_tightness", b"low", b"low\n"),
        (b"pointer_modifier", b"mod1", b"mod1\n"),
        (b"pointer_motion_interval", b"25", b"25\n"),
        (b"pointer_resize_sync", b"on", b"true\n"),
        (b"pointer_action1", b"focus", b"focus\n"),
        (b"pointer_action2", b"none", b"none\n"),
        (b"pointer_action3", b"move", b"move\n"),
        (b"mapping_events_count", b"2", b"2\n"),
        (b"presel_feedback", b"off", b"false\n"),
        (b"borderless_monocle", b"on", b"true\n"),
        (b"gapless_monocle", b"on", b"true\n"),
        (b"single_monocle", b"on", b"true\n"),
        (b"borderless_singleton", b"on", b"true\n"),
        (b"focus_follows_pointer", b"on", b"true\n"),
        (b"pointer_follows_focus", b"on", b"true\n"),
        (b"pointer_follows_monitor", b"on", b"true\n"),
        (b"click_to_focus", b"button2", b"button2\n"),
        (b"swallow_first_click", b"on", b"true\n"),
        (b"enable_ewmh_ping", b"on", b"true\n"),
        (b"enable_ewmh_allowed_actions", b"on", b"true\n"),
        (b"ignore_ewmh_focus", b"on", b"true\n"),
        (b"ignore_ewmh_struts", b"on", b"true\n"),
        (b"ignore_ewmh_fullscreen", b"enter,exit", b"enter,exit\n"),
        (b"put_dialogs_above", b"on", b"true\n"),
        (b"center_pseudo_tiled", b"off", b"false\n"),
        (b"honor_size_hints", b"floating", b"floating\n"),
        (b"remove_disabled_monitors", b"on", b"true\n"),
        (b"remove_unplugged_monitors", b"on", b"true\n"),
        (b"merge_overlapping_monitors", b"on", b"true\n"),
    ];
    for &(name, value, expected) in cases {
        assert!(run(&mut state, &[b"config", name, value]).is_empty());
        assert_eq!(run(&mut state, &[b"config", name]), expected, "{name:?}");
    }
}

#[test]
fn scoped_window_gap_reads_and_writes_follow_desktop_and_monitor_defaults() {
    let mut state = descriptor_fixture();
    let main = state.world.focused_monitor.unwrap();
    let first = state.world.monitor(main).desktops[0];
    let second = state.world.monitor(main).desktops[1];
    let right = locate_monitor(&state.world, "right")
        .unwrap()
        .monitor
        .unwrap();
    let third = state.world.monitor(right).desktops[0];

    assert!(
        run(
            &mut state,
            &[b"config", b"-m", b"main", b"window_gap", b"17"]
        )
        .is_empty()
    );
    assert_eq!(state.settings.window_gap, 6);
    assert_eq!(state.world.monitor(main).window_gap, 17);
    assert_eq!(state.world.desktop(first).window_gap, 17);
    assert_eq!(state.world.desktop(second).window_gap, 17);
    assert_eq!(state.world.monitor(right).window_gap, 6);
    assert_eq!(
        run(
            &mut state,
            &[b"config", b"--monitor", b"main", b"window_gap"]
        ),
        b"17\n"
    );

    assert!(
        run(
            &mut state,
            &[b"config", b"-n", b"0x50", b"window_gap", b"23"]
        )
        .is_empty()
    );
    assert_eq!(state.world.desktop(third).window_gap, 23);
    assert_eq!(state.world.monitor(right).window_gap, 6);
    assert_eq!(
        run(&mut state, &[b"config", b"-d", b"III", b"window_gap"]),
        b"23\n"
    );

    assert!(
        run(
            &mut state,
            &[
                b"config",
                b"-m",
                b"right",
                b"-d",
                b"I",
                b"window_gap",
                b"29"
            ]
        )
        .is_empty()
    );
    assert_eq!(state.world.desktop(first).window_gap, 29);
    assert_eq!(state.world.desktop(third).window_gap, 23);

    assert!(run(&mut state, &[b"config", b"window_gap", b"31"]).is_empty());
    assert_eq!(state.settings.window_gap, 31);
    for monitor in state.world.monitor_order() {
        assert_eq!(state.world.monitor(*monitor).window_gap, 31);
        for desktop in &state.world.monitor(*monitor).desktops {
            assert_eq!(state.world.desktop(*desktop).window_gap, 31);
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn scoped_border_width_propagates_to_the_selected_defaults_and_client_leaves() {
    let mut state = descriptor_fixture();
    let main = state.world.focused_monitor.unwrap();
    let first = state.world.monitor(main).desktops[0];
    let second = state.world.monitor(main).desktops[1];
    let left = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    let right_leaf = find_by_id(&state.world, 0x32).unwrap().node.unwrap();
    let second_node = find_by_id(&state.world, 0x40).unwrap().node.unwrap();
    let third_node = find_by_id(&state.world, 0x50).unwrap().node.unwrap();

    assert!(run(&mut state, &[b"config", b"-d", b"I", b"border_width", b"4"]).is_empty());
    assert_eq!(state.settings.border_width, 1);
    assert_eq!(state.world.monitor(main).border_width, 1);
    assert_eq!(state.world.desktop(first).border_width, 4);
    assert_eq!(state.world.desktop(second).border_width, 1);
    assert_eq!(client(&state, left).border_width, 4);
    assert_eq!(client(&state, right_leaf).border_width, 4);
    assert_eq!(client(&state, second_node).border_width, 1);

    assert!(
        run(
            &mut state,
            &[b"config", b"-n", b"0x30", b"border_width", b"7"]
        )
        .is_empty()
    );
    assert_eq!(state.world.desktop(first).border_width, 4);
    assert_eq!(client(&state, left).border_width, 7);
    assert_eq!(client(&state, right_leaf).border_width, 7);
    assert_eq!(
        run(&mut state, &[b"config", b"-n", b"0x30", b"border_width"]),
        b"7\n"
    );

    assert!(
        run(
            &mut state,
            &[b"config", b"-m", b"right", b"border_width", b"9"]
        )
        .is_empty()
    );
    assert_eq!(client(&state, third_node).border_width, 9);
    assert_eq!(run(&mut state, &[b"config", b"border_width"]), b"1\n");

    assert!(run(&mut state, &[b"config", b"border_width", b"2"]).is_empty());
    assert_eq!(state.settings.border_width, 2);
    for node in [left, right_leaf, second_node, third_node] {
        assert_eq!(client(&state, node).border_width, 2);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn scoped_honor_size_hints_changes_clients_without_creating_scope_defaults() {
    let mut state = descriptor_fixture();
    let left = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    let right_leaf = find_by_id(&state.world, 0x32).unwrap().node.unwrap();
    let second_node = find_by_id(&state.world, 0x40).unwrap().node.unwrap();
    let third_node = find_by_id(&state.world, 0x50).unwrap().node.unwrap();

    assert!(
        run(
            &mut state,
            &[
                b"config",
                b"--monitor",
                b"main",
                b"honor_size_hints",
                b"floating"
            ]
        )
        .is_empty()
    );
    assert_eq!(state.settings.honor_size_hints, HonorSizeHintsMode::No);
    for node in [left, right_leaf, second_node] {
        assert_eq!(
            client(&state, node).honor_size_hints,
            HonorSizeHintsMode::Floating
        );
    }
    assert_eq!(
        client(&state, third_node).honor_size_hints,
        HonorSizeHintsMode::No
    );
    assert_eq!(
        run(&mut state, &[b"config", b"-d", b"I", b"honor_size_hints"]),
        b"floating\n"
    );
    assert_eq!(
        run(
            &mut state,
            &[b"config", b"-d", b"empty", b"honor_size_hints"]
        ),
        b"\n"
    );
    assert_eq!(
        run(&mut state, &[b"config", b"honor_size_hints"]),
        b"false\n"
    );

    assert!(
        run(
            &mut state,
            &[b"config", b"-n", b"0x32", b"honor_size_hints", b"true"]
        )
        .is_empty()
    );
    assert_eq!(
        client(&state, right_leaf).honor_size_hints,
        HonorSizeHintsMode::Yes
    );
    assert_eq!(
        client(&state, left).honor_size_hints,
        HonorSizeHintsMode::Floating
    );

    assert!(run(&mut state, &[b"config", b"honor_size_hints", b"tiled"]).is_empty());
    assert_eq!(state.settings.honor_size_hints, HonorSizeHintsMode::Tiled);
    for node in [left, right_leaf, second_node, third_node] {
        assert_eq!(
            client(&state, node).honor_size_hints,
            HonorSizeHintsMode::Tiled
        );
    }
}

#[test]
fn scoped_padding_preserves_each_upstream_propagation_boundary() {
    let mut state = descriptor_fixture();
    let main = state.world.focused_monitor.unwrap();
    let first = state.world.monitor(main).desktops[0];
    let right = locate_monitor(&state.world, "right")
        .unwrap()
        .monitor
        .unwrap();
    let third = state.world.monitor(right).desktops[0];

    assert!(
        run(
            &mut state,
            &[b"config", b"-m", b"main", b"top_padding", b"10"]
        )
        .is_empty()
    );
    assert_eq!(state.world.monitor(main).padding.top, 10);
    assert_eq!(state.world.desktop(first).padding.top, 0);
    assert_eq!(state.settings.padding.top, 0);

    assert!(
        run(
            &mut state,
            &[b"config", b"-d", b"I", b"right_padding", b"11"]
        )
        .is_empty()
    );
    assert_eq!(state.world.desktop(first).padding.right, 11);
    assert_eq!(state.world.monitor(main).padding.right, 0);

    assert!(
        run(
            &mut state,
            &[b"config", b"-n", b"0x31", b"bottom_padding", b"12"]
        )
        .is_empty()
    );
    assert_eq!(state.world.desktop(first).padding.bottom, 12);
    assert_eq!(
        run(&mut state, &[b"config", b"-n", b"0x31", b"bottom_padding"]),
        b"12\n"
    );

    assert!(run(&mut state, &[b"config", b"left_padding", b"13"]).is_empty());
    assert_eq!(state.settings.padding.left, 13);
    assert_eq!(state.world.monitor(main).padding.left, 13);
    assert_eq!(state.world.monitor(right).padding.left, 13);
    assert_eq!(state.world.desktop(first).padding.left, 0);
    assert_eq!(state.world.desktop(third).padding.left, 0);

    assert!(
        run(
            &mut state,
            &[b"config", b"-m", b"right", b"top_monocle_padding", b"14"]
        )
        .is_empty()
    );
    assert_eq!(state.settings.monocle_padding.top, 14);
    assert_eq!(
        run(
            &mut state,
            &[b"config", b"-d", b"I", b"top_monocle_padding"]
        ),
        b"14\n"
    );
}

#[test]
fn config_selector_failures_and_argument_counts_match_other_domains() {
    let mut state = descriptor_fixture();
    assert_eq!(
        run(&mut state, &[b"config", b"-m"]),
        b"\x07config -m: Not enough arguments.\n"
    );
    assert_eq!(
        run(&mut state, &[b"config", b"--unknown"]),
        b"\x07config: Unknown option: '--unknown'.\n"
    );
    assert_eq!(
        run(&mut state, &[b"config", b"-d", b"missing", b"window_gap"]),
        b"\x07config -d: Invalid descriptor found in 'missing'.\n"
    );
    assert_eq!(
        run(&mut state, &[b"config", b"-n", b"0xDEAD", b"border_width"]),
        b"\x07"
    );
    assert_eq!(
        run(&mut state, &[b"config", b"-m", b"main", b"a", b"b", b"c"]),
        b"\x07config: Was expecting 1 or 2 arguments, received 3.\n"
    );
}

#[test]
fn config_records_required_x_effects_without_applying_them() {
    let mut state = descriptor_fixture();
    assert!(run(&mut state, &[b"config", b"-d", b"I", b"window_gap", b"8"]).is_empty());
    assert_eq!(
        state
            .pending_effects
            .iter()
            .filter(|effect| matches!(effect, CommandEffect::Arrange { .. }))
            .count(),
        4
    );

    state.pending_effects.clear();
    assert!(run(&mut state, &[b"config", b"pointer_modifier", b"mod1"]).is_empty());
    assert!(
        state
            .pending_effects
            .contains(&CommandEffect::RegrabButtons)
    );

    state.pending_effects.clear();
    assert!(run(&mut state, &[b"config", b"normal_border_color", b"#123456"]).is_empty());
    assert!(
        state
            .pending_effects
            .contains(&CommandEffect::RefreshColors)
    );

    state.pending_effects.clear();
    assert!(
        run(
            &mut state,
            &[b"config", b"remove_disabled_monitors", b"off"]
        )
        .is_empty()
    );
    assert!(
        !state
            .pending_effects
            .contains(&CommandEffect::RefreshMonitors)
    );
    state.pending_effects.clear();
    assert!(run(&mut state, &[b"config", b"remove_disabled_monitors", b"on"]).is_empty());
    assert!(
        state
            .pending_effects
            .contains(&CommandEffect::RefreshMonitors)
    );

    state.pending_effects.clear();
    assert!(run(&mut state, &[b"config", b"split_ratio", b"0.6"]).is_empty());
    assert!(state.pending_effects.is_empty());
}

#[test]
fn desktop_monitor_and_node_pure_mutations_work_with_selectors() {
    let mut state = fixture();
    assert!(run(&mut state, &[b"desktop", b"I", b"-n", b"one"]).is_empty());
    assert!(run(&mut state, &[b"desktop", b"one", b"-l", b"monocle"]).is_empty());
    assert!(run(&mut state, &[b"desktop", b"II", b"--focus"]).is_empty());
    assert!(
        run(
            &mut state,
            &[b"monitor", b"focused", b"--rename", b"display"]
        )
        .is_empty()
    );
    run(&mut state, &[b"node", b"0x30", b"--ratio", b"0.3"]);
    run(&mut state, &[b"node", b"0x30", b"--type", b"horizontal"]);
    run(&mut state, &[b"node", b"0x30", b"--flip", b"horizontal"]);
    run(&mut state, &[b"node", b"0x30", b"--rotate", b"90"]);
    run(&mut state, &[b"node", b"0x30", b"--equalize", b"--balance"]);
    assert_eq!(
        state
            .world
            .monitor(state.world.focused_monitor.unwrap())
            .name,
        "display"
    );
    let ratio = state
        .world
        .tree
        .node(find_by_id(&state.world, 0x30).unwrap().node.unwrap())
        .split_ratio;
    assert!((ratio - 0.5).abs() < f64::EPSILON);
}

#[test]
fn node_preselection_commands_update_queryable_pure_state() {
    let mut state = fixture();
    assert!(run(&mut state, &[b"node", b"0x30", b"--presel-dir", b"west"]).is_empty());
    assert!(run(&mut state, &[b"node", b"0x30", b"--presel-ratio", b"0.3"]).is_empty());
    let node = find_by_id(&state.world, 0x30).unwrap().node.unwrap();
    let presel = state.world.tree.node(node).presel.unwrap();
    assert_eq!(presel.split_dir, crate::types::Direction::West);
    assert!((presel.split_ratio - 0.3).abs() < f64::EPSILON);
    assert_eq!(
        run(&mut state, &[b"query", b"-N", b"-n", b".automatic"]),
        b"0x00000031\n0x00000032\n"
    );
    assert!(run(&mut state, &[b"node", b"0x30", b"-p", b"~west"]).is_empty());
    assert_eq!(state.world.tree.node(node).presel, None);
    assert!(run(&mut state, &[b"node", b"0x30", b"-p", b"east"]).is_empty());
    assert!(run(&mut state, &[b"node", b"0x30", b"-p", b"cancel"]).is_empty());
    assert_eq!(state.world.tree.node(node).presel, None);
    assert_eq!(
        run(&mut state, &[b"node", b"0x30", b"-o", b"1"]),
        b"\x07node -o: Invalid argument: '1'.\n"
    );
}

#[test]
fn vacancy_changes_publish_automatic_preselection_cancellation() {
    let mut state = fixture();
    assert!(run(&mut state, &[b"node", b"0x31", b"--presel-dir", b"east"]).is_empty());
    state.pending_effects.clear();

    assert!(run(&mut state, &[b"node", b"0x31", b"--state", b"floating"]).is_empty());
    assert!(state.pending_effects.iter().any(|effect| {
        matches!(
            effect,
            CommandEffect::Broadcast { mask, status, .. }
                if *mask == crate::types::SubscriberMask::NODE_PRESEL
                    && status.ends_with("0x00000031 cancel\n")
        )
    }));
}

#[test]
fn x_backed_commands_queue_effects_and_diagnostics_are_byte_preserving() {
    let mut state = fixture();
    assert!(run(&mut state, &[b"node", b"--focus"]).is_empty());
    assert!(state.pending_effects.iter().any(|effect| matches!(
        effect,
        CommandEffect::Focus {
            activate: false,
            ..
        }
    )));
    assert!(run(&mut state, &[b"monitor", b"--rectangle", b"1x1+0+0"]).is_empty());
    assert!(
        state
            .pending_effects
            .iter()
            .any(|effect| matches!(effect, CommandEffect::UpdateMonitorRoot { .. }))
    );
    assert!(run(&mut state, &[b"node", b"--state", b"floating"]).is_empty());
    assert!(run(&mut state, &[b"node", b"--move", b"70000", b"-70000"]).is_empty());
    assert!(
        run(
            &mut state,
            &[b"node", b"--resize", b"bottom_right", b"3", b"4"]
        )
        .is_empty()
    );
    assert!(state.pending_effects.iter().any(|effect| {
        matches!(
            effect,
            CommandEffect::MoveResize {
                rectangle: Rectangle {
                    x: 70_000,
                    y: -70_000,
                    ..
                },
                ..
            }
        )
    }));
    assert!(run(&mut state, &[b"node", b"--close"]).is_empty());
    assert!(
        state
            .pending_effects
            .iter()
            .any(|effect| matches!(effect, CommandEffect::Close { .. }))
    );
    assert_eq!(
        run(&mut state, &[b"query", b"--bad\xff"]),
        b"\x07query: Unknown option: '--bad\xff'.\n"
    );
}

#[test]
fn pure_command_families_mutate_valid_state_and_expose_x_effects() {
    let mut state = descriptor_fixture();
    assert!(run(&mut state, &[b"node", b"0x31", b"-g", b"marked=on"]).is_empty());
    assert!(run(&mut state, &[b"node", b"0x31", b"-l", b"above"]).is_empty());
    assert!(run(&mut state, &[b"node", b"0x31", b"-t", b"floating"]).is_empty());
    let node = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    let item = state.world.tree.node(node);
    assert!(item.marked && item.vacant);
    assert_eq!(client(&state, node).layer, crate::types::StackLayer::Above);
    assert_eq!(
        client(&state, node).state,
        crate::types::ClientState::Floating
    );

    assert!(run(&mut state, &[b"node", b"0x32", b"-d", b"II"]).is_empty());
    assert!(run(&mut state, &[b"desktop", b"II", b"-b", b"prev"]).is_empty());
    assert!(run(&mut state, &[b"monitor", b"main", b"-a", b"III"]).is_empty());
    assert!(run(&mut state, &[b"wm", b"-a", b"aux", b"320x200+2400+0"]).is_empty());
    assert!(state.pending_effects.contains(&CommandEffect::SyncEwmh));
    assert!(
        state
            .pending_effects
            .iter()
            .any(|effect| matches!(effect, CommandEffect::CreateMonitorRoot { .. }))
    );
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn node_follow_transfer_adapts_geometry_focuses_destination_and_records_effects() {
    let mut state = descriptor_fixture();
    let node = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    state
        .world
        .tree
        .node_mut(node)
        .client
        .as_mut()
        .expect("node holds a client")
        .floating_rectangle = Rectangle::new(100, 50, 200, 100);

    assert!(
        run(
            &mut state,
            &[b"node", b"0x31", b"--to-monitor", b"right", b"--follow"]
        )
        .is_empty()
    );

    let destination = locate_desktop(&state.world, "III")
        .unwrap()
        .desktop
        .unwrap();
    assert_eq!(state.world.node_desktop(node), Some(destination));
    assert_eq!(
        client(&state, node).floating_rectangle,
        Rectangle::new(1700, 50, 200, 100)
    );
    assert_eq!(
        state.world.focused_monitor,
        state.world.desktop_monitor(destination)
    );
    assert!(state.pending_effects.iter().any(|effect| matches!(
            effect,
            CommandEffect::Broadcast { mask, status, .. }
                if *mask == crate::types::SubscriberMask::NODE_TRANSFER
                    && status == "node_transfer 0x00000010 0x00000020 0x00000031 0x00000012 0x00000023 0x00000050\n"
        )));
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn node_transfer_preserves_geometry_already_on_destination_monitor() {
    let mut state = descriptor_fixture();
    let node = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    let rectangle = Rectangle::new(1700, 50, 200, 100);
    state
        .world
        .tree
        .node_mut(node)
        .client
        .as_mut()
        .expect("node holds a client")
        .floating_rectangle = rectangle;

    assert!(
        run(
            &mut state,
            &[b"node", b"0x31", b"--to-monitor", b"right", b"--follow"]
        )
        .is_empty()
    );

    assert_eq!(client(&state, node).floating_rectangle, rectangle);
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn sticky_focus_transfer_and_receptacle_kill_are_structural() {
    let mut state = descriptor_fixture();
    let sticky = find_by_id(&state.world, 0x31).unwrap().node.unwrap();
    let monitor = state.world.focused_monitor.unwrap();
    let second = locate_desktop(&state.world, "II").unwrap().desktop.unwrap();

    assert!(run(&mut state, &[b"node", b"0x31", b"--flag", b"sticky=on"]).is_empty());
    assert!(run(&mut state, &[b"desktop", b"II", b"--focus"]).is_empty());
    assert_eq!(state.world.node_desktop(sticky), Some(second));
    assert_eq!(state.world.monitor(monitor).sticky_count, 1);

    let mut state = fixture();
    let receptacle = find_by_id(&state.world, 0x32).unwrap().node.unwrap();
    assert!(run(&mut state, &[b"node", b"0x32", b"--kill"]).is_empty());
    assert_eq!(state.world.node_desktop(receptacle), None);
    assert!(
        !state
            .pending_effects
            .iter()
            .any(|effect| matches!(effect, CommandEffect::Kill { node } if *node == receptacle))
    );
    assert!(state.pending_effects.iter().any(|effect| matches!(
        effect,
        CommandEffect::Broadcast { mask, .. }
            if *mask == crate::types::SubscriberMask::NODE_REMOVE
    )));
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn adopt_orphans_is_an_x_aware_deferred_effect() {
    let mut state = fixture();
    assert!(run(&mut state, &[b"wm", b"--adopt-orphans"]).is_empty());
    assert!(state.pending_effects.contains(&CommandEffect::AdoptOrphans));
}

#[test]
fn suppressed_focus_policy_is_a_parameter_and_never_touches_persisted_settings() {
    /// Focuses the empty second desktop, so that focusing the first one back
    /// resolves a node and would warp the pointer under the default policy.
    fn parked() -> DaemonState {
        let mut state = fixture();
        state.settings.pointer_follows_focus = true;
        state.settings.pointer_follows_monitor = true;
        assert!(run(&mut state, &[b"desktop", b"II", b"--focus"]).is_empty());
        state.pending_effects.clear();
        state
    }

    let mut state = parked();
    let monitor = state.world.focused_monitor.unwrap();
    let first = locate_desktop(&state.world, "I").unwrap().desktop.unwrap();
    let policy = crate::state::FocusPolicy::suppressed(&state, true, true);
    assert!(CommandHandler::new(&mut state).focus_location_with(
        Coordinates::in_desktop(monitor, first, None),
        false,
        policy,
    ));

    // The suppression rides on the effects, not on a temporary write.
    assert!(state.settings.pointer_follows_focus);
    assert!(state.settings.pointer_follows_monitor);
    assert!(state.auto_raise);
    assert!(
        !state
            .pending_effects
            .iter()
            .any(|effect| matches!(effect, CommandEffect::WarpPointer { .. }))
    );
    assert!(state.pending_effects.iter().any(|effect| matches!(
        effect,
        CommandEffect::Focus {
            auto_raise: false,
            ..
        }
    )));

    let mut state = parked();
    assert!(run(&mut state, &[b"desktop", b"I", b"--focus"]).is_empty());
    assert!(
        state
            .pending_effects
            .iter()
            .any(|effect| matches!(effect, CommandEffect::WarpPointer { .. })),
        "an unsuppressed focus still honours pointer_follows_focus"
    );
    assert!(state.pending_effects.iter().any(|effect| matches!(
        effect,
        CommandEffect::Focus {
            auto_raise: true,
            ..
        }
    )));
}

#[test]
fn renames_queue_their_status_records_and_ignore_no_op_renames() {
    let rename_record = |state: &DaemonState, mask| {
        state
            .pending_effects
            .iter()
            .find_map(|effect| match effect {
                CommandEffect::Broadcast {
                    mask: found,
                    status,
                    ..
                } if *found == mask => Some(status.clone()),
                _ => None,
            })
    };

    let mut state = fixture();
    assert!(run(&mut state, &[b"desktop", b"I", b"--rename", b"web"]).is_empty());
    assert_eq!(
        rename_record(&state, crate::types::SubscriberMask::DESKTOP_RENAME).as_deref(),
        Some("desktop_rename 0x00000010 0x00000020 I web\n"),
    );
    assert!(run(&mut state, &[b"monitor", b"main", b"--rename", b"aux"]).is_empty());
    assert_eq!(
        rename_record(&state, crate::types::SubscriberMask::MONITOR_RENAME).as_deref(),
        Some("monitor_rename 0x00000010 main aux\n"),
    );

    let mut state = fixture();
    assert!(run(&mut state, &[b"desktop", b"I", b"--rename", b"I"]).is_empty());
    assert!(run(&mut state, &[b"monitor", b"main", b"--rename", b"main"]).is_empty());
    assert!(
        rename_record(&state, crate::types::SubscriberMask::DESKTOP_RENAME).is_none()
            && rename_record(&state, crate::types::SubscriberMask::MONITOR_RENAME).is_none(),
        "a rename to the current name emits nothing, as the removed diff did"
    );
}

/// The monitor and desktop arenas were `Vec`s that only ever grew: adding and
/// removing a desktop, or a whole monitor, left the entry behind forever, and
/// `next_external_id` then scanned that garbage on every insert. Both arenas
/// have to return to the size they started at.
#[test]
fn desktop_and_monitor_churn_returns_both_arenas_to_their_starting_size() {
    let mut state = fixture();
    let monitors = state.world.monitor_count();
    let desktops = state.world.desktop_count();
    let nodes = state.world.tree.len();
    let next_id = state.world.next_external_id();

    for _ in 0..40 {
        assert!(
            run(
                &mut state,
                &[b"monitor", b"main", b"--add-desktops", b"tmp"]
            )
            .is_empty()
        );
        assert_eq!(state.world.desktop_count(), desktops + 1);
        assert!(run(&mut state, &[b"desktop", b"tmp", b"--remove"]).is_empty());
        assert_eq!(
            state.world.desktop_count(),
            desktops,
            "desktop arena grew across an add/remove cycle"
        );
        assert_eq!(state.validate(), Ok(()));
    }

    for _ in 0..40 {
        assert!(
            run(
                &mut state,
                &[b"wm", b"--add-monitor", b"tmp", b"800x600+800+0"],
            )
            .is_empty()
        );
        assert_eq!(state.world.monitor_count(), monitors + 1);
        assert!(run(&mut state, &[b"monitor", b"tmp", b"--remove"]).is_empty());
        assert_eq!(
            state.world.monitor_count(),
            monitors,
            "monitor arena grew across an add/remove cycle"
        );
        assert_eq!(
            state.world.desktop_count(),
            desktops,
            "removing a monitor has to free its desktops too"
        );
        assert_eq!(state.validate(), Ok(()));
    }

    assert_eq!(state.world.tree.len(), nodes);
    // Freed ids are handed out again, so the allocator does not drift either.
    assert_eq!(state.world.next_external_id(), next_id);
}
