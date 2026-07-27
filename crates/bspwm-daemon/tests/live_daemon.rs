//! Daemon behaviour that needs a real X server.
//!
//! These stay `#[ignore]`d: they are driven by `tests/run` against Xephyr, not
//! by `cargo test`. They live outside `src/daemon.rs` so the unit test module
//! there does not have to pull in X plumbing for tests it never runs.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use xcb::{Xid, XidNew, x};

use bspwm::daemon::{ClientInitial, DaemonApp, XEventContext};
use bspwm::events::EventHandler;
use bspwm::messages::{Domain, MessageHandler, Response};
use bspwm::rule::{Rule, RuleConsequence, make_rule_consequence};
use bspwm::runtime::RuntimeApp;
use bspwm::settings::Settings;
use bspwm::state::{CommandEffect, DaemonState};
use bspwm::tree::{NodeId, SizeHints};
use bspwm::types::{ClientState, Direction, Rectangle, StackLayer};
use bspwm::world::{DesktopId, MonitorId};
use bspwm::x11::X11;
use bspwm::{monitor, restore, window};

#[derive(Default)]
struct TestResponse(Vec<u8>);

impl Write for TestResponse {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Response for TestResponse {
    fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

fn test_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "bspwm-rs-live-daemon-{name}-{}-{}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}

fn app_with_desktop() -> (DaemonApp, MonitorId, DesktopId) {
    let settings = Settings::default();
    let mut state = DaemonState::default();
    let monitor =
        state
            .world
            .create_monitor(1, Some("monitor"), Rectangle::new(0, 0, 100, 80), &settings);
    let desktop = state.world.create_desktop(2, Some("I"), &settings);
    assert!(state.world.add_desktop(monitor, desktop));
    (DaemonApp::new(state), monitor, desktop)
}

fn manage_window(app: &mut DaemonApp, window: u32) -> (MonitorId, DesktopId, NodeId) {
    app.manage_window_with_initial(
        window,
        &RuleConsequence::default(),
        Rectangle::new(0, 0, 1, 1),
        SizeHints::default(),
        ClientInitial::default(),
        !window,
    )
    .unwrap()
}

fn manage_window_with(
    app: &mut DaemonApp,
    window: u32,
    consequence: &RuleConsequence,
    initial_rectangle: Rectangle,
    size_hints: SizeHints,
    internal_xid: u32,
) -> Option<(MonitorId, DesktopId, NodeId)> {
    app.manage_window_with_initial(
        window,
        consequence,
        initial_rectangle,
        size_hints,
        ClientInitial::default(),
        internal_xid,
    )
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn executes_action_plan_on_live_x_server() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    DaemonApp::execute_plan(&x11, &[]).expect("execute empty plan");
}

#[test]
#[ignore = "requires a live X server with the Shape extension selected by DISPLAY"]
fn live_presel_feedback_command_cancel_and_insertion_lifecycle() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let (mut app, monitor, desktop) = app_with_desktop();
    app.state.world.monitor_mut(monitor).rectangle = Rectangle::new(0, 0, 200, 120);
    let first: x::Window = x11.connection().generate_id();
    let second: x::Window = x11.connection().generate_id();
    create_live_window(&x11, first, false);
    create_live_window(&x11, second, false);
    let (_, _, node) = manage_window(&mut app, first.resource_id());
    app.arrange_desktop(&x11, monitor, desktop).unwrap();

    app.state
        .world
        .tree
        .set_presel_direction(node, Direction::East, 0.5);
    app.state
        .pending_effects
        .push(CommandEffect::SyncPreselFeedback {
            node,
            include_receptacle: false,
        });
    app.execute_pending_effects(&x11).unwrap();
    let feedback = app
        .state
        .world
        .tree
        .node(node)
        .presel
        .unwrap()
        .feedback
        .unwrap();
    assert!(window::exists(&x11, x::Window::new(feedback)));
    assert!(app.mapped_feedbacks.contains(&feedback));

    app.state.world.tree.cancel_presel(node);
    app.execute_pending_effects(&x11).unwrap();
    assert!(!window::exists(&x11, x::Window::new(feedback)));

    app.state
        .world
        .tree
        .set_presel_direction(node, Direction::West, 0.5);
    app.state
        .pending_effects
        .push(CommandEffect::SyncPreselFeedback {
            node,
            include_receptacle: false,
        });
    app.execute_pending_effects(&x11).unwrap();
    let consumed = app
        .state
        .world
        .tree
        .node(node)
        .presel
        .unwrap()
        .feedback
        .unwrap();
    manage_window(&mut app, second.resource_id());
    app.execute_pending_effects(&x11).unwrap();
    assert!(!window::exists(&x11, x::Window::new(consumed)));

    window::destroy(&x11, first).ok();
    window::destroy(&x11, second).ok();
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
#[allow(clippy::too_many_lines)]
fn live_command_effects_configure_properties_ewmh_and_focus_correction() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let (mut app, monitor, desktop) = app_with_desktop();
    app.state.world.monitor_mut(monitor).rectangle = Rectangle::new(0, 0, 300, 200);
    let first: x::Window = x11.connection().generate_id();
    let second: x::Window = x11.connection().generate_id();
    create_live_window(&x11, first, false);
    create_live_window(&x11, second, false);
    let mut consequence = make_rule_consequence();
    consequence.state = Some(ClientState::Floating);
    consequence.rect = Some(Rectangle::new(10, 10, 40, 30));
    let first_node = manage_window_with(
        &mut app,
        first.resource_id(),
        &consequence,
        Rectangle::new(10, 10, 40, 30),
        SizeHints::default(),
        x11.connection().generate_id::<x::Window>().resource_id(),
    )
    .unwrap()
    .2;
    let second_node = manage_window_with(
        &mut app,
        second.resource_id(),
        &consequence,
        Rectangle::new(80, 10, 40, 30),
        SizeHints::default(),
        x11.connection().generate_id::<x::Window>().resource_id(),
    )
    .unwrap()
    .2;
    window::map(&x11, first).unwrap();
    window::map(&x11, second).unwrap();

    let descriptor = format!("0x{:08X}", first.resource_id());
    app.dispatch(
        Domain::Node,
        &[descriptor.as_bytes(), b"--move", b"5", b"6"],
        &mut TestResponse::default(),
    )
    .unwrap();
    app.execute_pending_effects(&x11).unwrap();
    assert_eq!(
        window::geometry(&x11, first).unwrap().rectangle,
        Rectangle::new(15, 16, 40, 30)
    );

    let configure = x::ConfigureRequestEvent::new(
        x::StackMode::Above,
        x11.root(),
        first,
        x::Window::none(),
        20,
        21,
        55,
        44,
        0,
        x::ConfigWindowMask::X
            | x::ConfigWindowMask::Y
            | x::ConfigWindowMask::WIDTH
            | x::ConfigWindowMask::HEIGHT,
    );
    XEventContext {
        app: &mut app,
        x11: &x11,
    }
    .configure_request(&configure)
    .unwrap();
    assert_eq!(
        window::geometry(&x11, first).unwrap().rectangle,
        Rectangle::new(19, 20, 55, 44)
    );

    window::set_property(
        &x11,
        first,
        x::ATOM_WM_HINTS,
        x::ATOM_WM_HINTS,
        &[1_u32 << 8],
    )
    .unwrap();
    let property = x::PropertyNotifyEvent::new(
        first,
        x::ATOM_WM_HINTS,
        x::CURRENT_TIME,
        x::Property::NewValue,
    );
    XEventContext {
        app: &mut app,
        x11: &x11,
    }
    .property_notify(&property)
    .unwrap();
    assert!(
        app.state
            .world
            .tree
            .node(first_node)
            .client
            .as_ref()
            .unwrap()
            .urgent
    );

    let fullscreen = x::ClientMessageEvent::new(
        first,
        x11.atoms().net_wm_state,
        x::ClientMessageData::Data32([
            1,
            x11.atoms().net_wm_state_fullscreen.resource_id(),
            0,
            0,
            0,
        ]),
    );
    XEventContext {
        app: &mut app,
        x11: &x11,
    }
    .client_message(&fullscreen)
    .unwrap();
    assert_eq!(
        app.state
            .world
            .tree
            .node(first_node)
            .client
            .as_ref()
            .unwrap()
            .state,
        ClientState::Fullscreen
    );

    app.state.world.desktop_mut(desktop).tree.focus = Some(first_node);
    window::focus(&x11, second).unwrap();
    let stolen = x::FocusInEvent::new(x::NotifyDetail::Nonlinear, second, x::NotifyMode::Normal);
    XEventContext {
        app: &mut app,
        x11: &x11,
    }
    .focus_in(&stolen)
    .unwrap();
    assert_eq!(x11.request(&x::GetInputFocus {}).unwrap().focus(), first);

    window::set_property(
        &x11,
        first,
        x11.atoms().wm_protocols,
        x::ATOM_ATOM,
        &[x11.atoms().wm_delete_window],
    )
    .unwrap();
    app.state
        .pending_effects
        .push(CommandEffect::Close { node: first_node });
    app.execute_pending_effects(&x11).unwrap();
    app.state
        .pending_effects
        .push(CommandEffect::Kill { node: second_node });
    app.execute_pending_effects(&x11).unwrap();
    x11.send_and_check_request(&x::DestroyWindow { window: first })
        .ok();
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn live_monitor_reconciliation_updates_adds_transfers_removes_and_cleans_roots() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let mut app = DaemonApp::default();
    let info = |output, name: &str, rectangle| monitor::MonitorInfo {
        output: Some(output),
        name: name.into(),
        rectangle,
        connected: true,
        enabled: true,
    };
    let first_query = monitor::MonitorQuery {
        monitors: vec![info(1, "first", Rectangle::new(0, 0, 100, 100))],
        primary_output: None,
        source: monitor::MonitorInfoSource::Randr,
    };
    app.reconcile_monitor_query(&x11, &first_query).unwrap();
    let first = app.state.world.monitor_order()[0];
    let first_root = app.state.world.monitor(first).root_id.unwrap();
    assert_eq!(app.state.world.monitor(first).randr_id, 1);
    assert_eq!(app.state.world.monitor(first).desktops.len(), 1);
    assert_eq!(app.state.world.primary_monitor, None);

    let client_window: x::Window = x11.connection().generate_id();
    create_live_window(&x11, client_window, false);
    let mut consequence = make_rule_consequence();
    consequence.state = Some(ClientState::Floating);
    consequence.rect = Some(Rectangle::new(10, 10, 20, 20));
    let node = manage_window_with(
        &mut app,
        client_window.resource_id(),
        &consequence,
        Rectangle::new(10, 10, 20, 20),
        SizeHints::default(),
        x11.connection().generate_id::<x::Window>().resource_id(),
    )
    .unwrap()
    .2;
    let second_query = monitor::MonitorQuery {
        monitors: vec![
            info(1, "first", Rectangle::new(0, 0, 200, 100)),
            info(2, "second", Rectangle::new(200, 0, 100, 100)),
        ],
        primary_output: Some(2),
        source: monitor::MonitorInfoSource::Randr,
    };
    app.reconcile_monitor_query(&x11, &second_query).unwrap();
    assert_eq!(
        app.state
            .world
            .tree
            .node(node)
            .client
            .as_ref()
            .unwrap()
            .floating_rectangle,
        Rectangle::new(22, 10, 20, 20)
    );
    let second = app
        .state
        .world
        .monitor_order()
        .iter()
        .copied()
        .find(|id| app.state.world.monitor(*id).randr_id == 2)
        .unwrap();
    assert_eq!(app.state.world.primary_monitor, Some(second));

    app.state.settings.remove_unplugged_monitors = true;
    let removal_query = monitor::MonitorQuery {
        monitors: vec![info(2, "second", Rectangle::new(200, 0, 100, 100))],
        primary_output: Some(2),
        source: monitor::MonitorInfoSource::Randr,
    };
    app.reconcile_monitor_query(&x11, &removal_query).unwrap();
    assert_eq!(app.state.world.monitor_order(), &[second]);
    assert_eq!(
        app.state
            .world
            .desktop_monitor(app.state.world.node_desktop(node).unwrap()),
        Some(second)
    );
    assert!(window::geometry(&x11, x::Window::new(first_root)).is_err());

    let remaining_root = app.state.world.monitor(second).root_id.unwrap();
    app.cleanup(&x11).unwrap();
    assert!(window::geometry(&x11, x::Window::new(remaining_root)).is_err());
    x11.send_and_check_request(&x::DestroyWindow {
        window: client_window,
    })
    .unwrap();
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn live_root_configure_updates_screen_dimensions_without_resizing_monitor() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let (mut app, monitor, _) = app_with_desktop();
    let rectangle = app.state.world.monitor(monitor).rectangle;
    let event = x::ConfigureNotifyEvent::new(
        x11.root(),
        x11.root(),
        x::Window::none(),
        0,
        0,
        777,
        555,
        0,
        false,
    );
    XEventContext {
        app: &mut app,
        x11: &x11,
    }
    .configure_notify(&event)
    .unwrap();
    assert_eq!((x11.geometry().width, x11.geometry().height), (777, 555));
    assert_eq!(app.state.world.monitor(monitor).rectangle, rectangle);
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn live_schedule_applies_class_type_and_user_rules() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let (mut app, _, _) = app_with_desktop();
    app.state.rules.add_rule(Rule::from_cause(
        "DialogApp:*:*",
        "layer=above border=off",
        true,
    ));
    app.state
        .rules
        .add_rule(Rule::from_cause("ToolApp:*:*", "manage=off", false));

    let dialog: x::Window = x11.connection().generate_id();
    create_live_window(&x11, dialog, false);
    set_live_class(&x11, dialog, b"dialog\0DialogApp\0");
    window::set_property(
        &x11,
        dialog,
        x11.atoms().net_wm_window_type,
        x::ATOM_ATOM,
        &[x11.atoms().net_wm_window_type_dialog],
    )
    .unwrap();
    let (_, _, node) = app
        .schedule_window(&x11, dialog.resource_id())
        .unwrap()
        .unwrap();
    let client = app.state.world.tree.node(node).client.as_ref().unwrap();
    assert_eq!(client.class_name, "DialogApp");
    assert_eq!(client.instance_name, "dialog");
    assert_eq!(client.state, ClientState::Floating);
    assert_eq!(client.layer, StackLayer::Above);
    assert_eq!(client.border_width, 0);
    assert_eq!(app.state.rules.len(), 1);
    assert_eq!(
        window::get_property::<u32>(&x11, dialog, x11.atoms().net_wm_state, x::ATOM_ATOM).unwrap(),
        [x11.atoms().net_wm_state_above.resource_id()]
    );

    let tool: x::Window = x11.connection().generate_id();
    create_live_window(&x11, tool, false);
    set_live_class(&x11, tool, b"tool\0ToolApp\0");
    window::set_property(
        &x11,
        tool,
        x11.atoms().net_wm_window_type,
        x::ATOM_ATOM,
        &[x11.atoms().net_wm_window_type_toolbar],
    )
    .unwrap();
    assert!(
        app.schedule_window(&x11, tool.resource_id())
            .unwrap()
            .is_none()
    );
    assert!(app.managed_window(tool.resource_id()).is_none());
    assert_eq!(
        x11.request(&x::GetWindowAttributes { window: tool })
            .unwrap()
            .map_state(),
        x::MapState::Viewable
    );

    let override_redirect: x::Window = x11.connection().generate_id();
    create_live_window(&x11, override_redirect, true);
    assert!(
        app.schedule_window(&x11, override_redirect.resource_id())
            .unwrap()
            .is_none()
    );
    assert!(
        app.managed_window(override_redirect.resource_id())
            .is_none()
    );

    for window in [dialog, tool, override_redirect] {
        x11.send_and_check_request(&x::DestroyWindow { window })
            .unwrap();
    }
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn live_adopt_orphans_manages_root_children_with_net_wm_desktop() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let (mut app, _, _) = app_with_desktop();
    let orphan: x::Window = x11.connection().generate_id();
    create_live_window(&x11, orphan, false);
    window::set_property(
        &x11,
        orphan,
        x11.atoms().net_wm_desktop,
        x::ATOM_CARDINAL,
        &[0_u32],
    )
    .unwrap();

    app.state.pending_effects.push(CommandEffect::AdoptOrphans);
    app.execute_pending_effects(&x11).unwrap();

    assert!(app.managed_window(orphan.resource_id()).is_some());
    x11.send_and_check_request(&x::DestroyWindow { window: orphan })
        .unwrap();
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn live_load_state_reconstructs_x_resources_and_client_runtime_state() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let (mut app, _, _) = app_with_desktop();
    RuntimeApp::setup(&mut app, &x11).expect("set up daemon runtime");
    let client: x::Window = x11.connection().generate_id();
    create_live_window(&x11, client, false);
    window::set_property(
        &x11,
        client,
        x11.atoms().wm_protocols,
        x::ATOM_ATOM,
        &[x11.atoms().wm_delete_window],
    )
    .unwrap();
    manage_window(&mut app, client.resource_id());
    let old_monitor_xid = app
        .state
        .world
        .monitor(app.state.world.monitor_order()[0])
        .external_id;
    let path = test_path("live-load-state");
    std::fs::write(&path, bspwm::query::query_state(&app.state)).unwrap();

    app.state.pending_effects.push(CommandEffect::LoadState {
        restored: Box::new(
            restore::restore_state(
                &std::fs::read_to_string(&path).unwrap(),
                &app.state.settings,
            )
            .unwrap(),
        ),
    });
    app.execute_pending_effects(&x11).unwrap();

    let monitor = app.state.world.monitor_order()[0];
    assert_ne!(
        app.state.world.monitor(monitor).external_id,
        old_monitor_xid
    );
    let restored = app.managed_window(client.resource_id()).unwrap().2;
    assert_eq!(
        app.state.world.tree.node(restored).external_id,
        client.resource_id()
    );
    assert!(
        app.state
            .world
            .tree
            .node(restored)
            .client
            .as_ref()
            .unwrap()
            .icccm
            .delete_window
    );
    assert!(app.state.world.monitor(monitor).root_id.is_some());

    RuntimeApp::cleanup(&mut app, &x11).unwrap();
    x11.send_and_check_request(&x::DestroyWindow { window: client })
        .unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
#[ignore = "requires a live X server selected by DISPLAY"]
fn live_pointer_runtime_state_obeys_app_lifecycle() {
    let x11 = X11::connect(None).expect("connect to DISPLAY");
    let mut app = DaemonApp::default();
    RuntimeApp::setup(&mut app, &x11).expect("set up pointer runtime");
    assert!(app.motion_recorder.is_some());
    RuntimeApp::cleanup(&mut app, &x11).expect("clean up pointer runtime");
    assert!(app.motion_recorder.is_none());
}

fn create_live_window(x11: &X11, window: x::Window, override_redirect: bool) {
    let values = override_redirect
        .then_some(x::Cw::OverrideRedirect(true))
        .into_iter()
        .collect::<Vec<_>>();
    x11.send_and_check_request(&x::CreateWindow {
        depth: 0,
        wid: window,
        parent: x11.root(),
        x: 0,
        y: 0,
        width: 40,
        height: 30,
        border_width: 0,
        class: x::WindowClass::InputOutput,
        visual: x::COPY_FROM_PARENT,
        value_list: &values,
    })
    .unwrap();
}

fn set_live_class(x11: &X11, window: x::Window, class: &[u8]) {
    window::set_property(x11, window, x11.atoms().wm_class, x::ATOM_STRING, class).unwrap();
}
