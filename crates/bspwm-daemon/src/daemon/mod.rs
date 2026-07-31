#![allow(clippy::missing_errors_doc)]

//! Concrete state, command, event, and X11 integration for the daemon runtime.

mod action;
mod effects;
mod events;
mod feedback;
mod manage;
mod monitors;
mod persist;
mod status;
#[cfg(test)]
mod test_support;

pub use action::XAction;
pub use events::XEventContext;
pub use manage::ClientInitial;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::RawFd;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use xcb::{Xid, XidNew, sync, x};

use crate::commands::CommandHandler;
use crate::events::{MappingFilterState, PointerFilterState};
use crate::ewmh;
use crate::messages::{Domain, MessageHandler, Response, Subscription};
use crate::monitor;
use crate::restore;
use crate::runtime::{InheritedFds, RuntimeApp, RuntimeError, UnixResponse};
use crate::state::DaemonState;
use crate::subscribe::{Subscribers, print_report};
use crate::tree::{Client, Node, NodeId, Tree};
use crate::types::{Point, PointerAction, Rectangle, ResizeHandle};
use crate::window;
use crate::world::{DesktopId, MonitorId, World};
use crate::x11::X11;

use manage::PendingRule;
use persist::SubscriberOutput;

/// Where a managed window lives: its monitor, desktop, and tree node.
type WindowLocation = (MonitorId, DesktopId, NodeId);

/// Every managed window, keyed by its X window identifier.
type WindowIndex = HashMap<u32, WindowLocation>;

fn timestamp_is_later(candidate: x::Timestamp, reference: x::Timestamp) -> bool {
    candidate.wrapping_sub(reference).cast_signed() > 0
}

/// The application owned by [`crate::runtime::Runtime`].
#[derive(Debug)]
pub struct DaemonApp {
    pub state: DaemonState,
    subscribers: Subscribers<SubscriberOutput>,
    lock_masks: crate::pointer::LockMasks,
    mapping_filter: MappingFilterState,
    pointer_filter: PointerFilterState,
    pointer_grab: Option<PointerGrab>,
    #[doc(hidden)]
    pub motion_recorder: Option<MotionRecorder>,
    pending_rules: Vec<PendingRule>,
    reaping_rules: Vec<crate::rule::ExternalRuleProcess>,
    restored_subscribers: Vec<restore::RestoredSubscriber>,
    startup: crate::startup::StartupTracker,
    last_user_time: Option<x::Timestamp>,
    user_time_windows: HashMap<u32, u32>,
    sync_request_clients: HashMap<u32, sync::Counter>,
    pending_ewmh_pings: HashMap<u32, PendingEwmhPing>,
    #[doc(hidden)]
    pub mapped_feedbacks: HashSet<u32>,
    /// Lazily rebuilt `window -> location` index behind [`DaemonApp::managed_window`].
    ///
    /// Every accessor that can hand out a mutable borrow of the world clears
    /// it, so a stale entry cannot outlive the mutation that invalidated it.
    window_index: RefCell<Option<WindowIndex>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PointerGrab {
    monitor: MonitorId,
    desktop: DesktopId,
    node: NodeId,
    action: PointerAction,
    handle: ResizeHandle,
    last_position: Point,
    last_motion_time: x::Timestamp,
    origin: PointerGrabOrigin,
    sync_resize: Option<SyncResize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SyncResize {
    counter: sync::Counter,
    alarm: sync::Alarm,
    value: i64,
    in_flight: bool,
    pending: Option<(Rectangle, x::Timestamp)>,
    deadline: Instant,
}

const SYNC_RESIZE_TIMEOUT: Duration = Duration::from_millis(250);
const EWMH_PING_TIMEOUT: Duration = Duration::from_secs(5);

const fn sync_i64(value: sync::Int64) -> i64 {
    ((value.hi as i64) << 32) | value.lo as i64
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const fn sync_int64(value: i64) -> sync::Int64 {
    sync::Int64 {
        hi: (value >> 32) as i32,
        lo: value as u32,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointerGrabOrigin {
    Binding,
    Ewmh { button: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingEwmhPing {
    timestamp: x::Timestamp,
    deadline: Instant,
    expiration_observed: bool,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MotionRecorder {
    window: u32,
    enabled: bool,
}

impl Default for DaemonApp {
    fn default() -> Self {
        Self::new(DaemonState::default())
    }
}

impl Drop for DaemonApp {
    fn drop(&mut self) {
        self.subscribers.clear();
    }
}

impl DaemonApp {
    fn close_client(
        &mut self,
        x11: &X11,
        node: NodeId,
        timestamp: x::Timestamp,
    ) -> Result<window::CloseMethod, RuntimeError> {
        let window_id = self.xid(node);
        let protocols = self.client(node).icccm;
        let method = window::close_cached(x11, x::Window::new(window_id), protocols.delete_window)?;
        if method == window::CloseMethod::WmDeleteWindow
            && self.state.settings.enable_ewmh_ping
            && protocols.ping
        {
            window::send_ping(x11, x::Window::new(window_id), timestamp)?;
            self.pending_ewmh_pings.insert(
                window_id,
                PendingEwmhPing {
                    timestamp,
                    deadline: Instant::now() + EWMH_PING_TIMEOUT,
                    expiration_observed: false,
                },
            );
        }
        Ok(method)
    }

    fn poll_ewmh_ping_timeouts(&mut self) -> bool {
        if !self.state.settings.enable_ewmh_ping {
            let changed = !self.pending_ewmh_pings.is_empty();
            self.pending_ewmh_pings.clear();
            return changed;
        }
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut changed = false;
        for (&window, ping) in &mut self.pending_ewmh_pings {
            if now < ping.deadline {
                continue;
            }
            if ping.expiration_observed {
                expired.push(window);
            } else {
                ping.expiration_observed = true;
                changed = true;
            }
        }
        for window in expired {
            self.pending_ewmh_pings.remove(&window);
            log::warn!("_NET_WM_PING timeout for window 0x{window:08X}");
            changed = true;
        }
        changed
    }

    fn acknowledge_ewmh_ping(&mut self, window: u32, timestamp: x::Timestamp) -> bool {
        if self
            .pending_ewmh_pings
            .get(&window)
            .is_some_and(|ping| ping.timestamp == timestamp)
        {
            self.pending_ewmh_pings.remove(&window);
            true
        } else {
            false
        }
    }

    fn poll_sync_resize_timeout(&mut self, x11: &X11) -> Result<bool, RuntimeError> {
        let Some(mut grab) = self.pointer_grab else {
            return Ok(false);
        };
        let Some(resize) = grab.sync_resize.as_ref() else {
            return Ok(false);
        };
        if !resize.in_flight || Instant::now() < resize.deadline {
            return Ok(false);
        }
        // Timeout: destroy the alarm and fall back to immediate resize.
        let _ = x11.send_and_check_request(&sync::DestroyAlarm {
            alarm: resize.alarm,
        });
        let pending = resize.pending;
        grab.sync_resize = None;
        if self.pointer_grab_is_live()
            && let Some((rectangle, _)) = pending
        {
            window::move_resize(x11, x::Window::new(self.xid(grab.node)), rectangle)?;
        }
        self.pointer_grab = Some(grab);
        Ok(true)
    }

    #[must_use]
    pub fn new(mut state: DaemonState) -> Self {
        state.running = true;
        let mapping_events_count = state.settings.mapping_events_count;
        Self {
            state,
            subscribers: Subscribers::default(),
            lock_masks: crate::pointer::LockMasks::default(),
            mapping_filter: MappingFilterState::new(mapping_events_count),
            pointer_filter: PointerFilterState::default(),
            pointer_grab: None,
            motion_recorder: None,
            pending_rules: Vec::new(),
            reaping_rules: Vec::new(),
            restored_subscribers: Vec::new(),
            startup: crate::startup::StartupTracker::default(),
            last_user_time: None,
            user_time_windows: HashMap::new(),
            sync_request_clients: HashMap::new(),
            pending_ewmh_pings: HashMap::new(),
            mapped_feedbacks: HashSet::new(),
            window_index: RefCell::new(None),
        }
    }

    /// Drops the cached `window -> location` index.
    ///
    /// Called by every accessor that lends out a mutable borrow of the world,
    /// so the index cannot survive a structural change. The borrow returned by
    /// those accessors also keeps [`DaemonApp::managed_window`] unreachable
    /// until the mutation finishes.
    fn invalidate_window_index(&mut self) {
        *self.window_index.get_mut() = None;
    }

    #[must_use]
    fn world(&self) -> &World {
        &self.state.world
    }

    fn world_mut(&mut self) -> &mut World {
        self.invalidate_window_index();
        &mut self.state.world
    }

    #[must_use]
    fn tree(&self) -> &Tree {
        &self.state.world.tree
    }

    fn tree_mut(&mut self) -> &mut Tree {
        self.invalidate_window_index();
        &mut self.state.world.tree
    }

    #[must_use]
    fn node(&self, node: NodeId) -> &Node {
        self.state.world.tree.node(node)
    }

    fn node_mut(&mut self, node: NodeId) -> &mut Node {
        self.invalidate_window_index();
        self.state.world.tree.node_mut(node)
    }

    /// The X window backing `node`.
    #[must_use]
    fn xid(&self, node: NodeId) -> u32 {
        self.node(node).external_id
    }

    /// Whether the in-flight pointer grab still names live state.
    ///
    /// A drag outlives its subject when the desktop or monitor under it goes
    /// away -- `bspc monitor --remove`, or an X randr unplug -- which frees the
    /// dragged node with them. The grab itself is kept so the eventual
    /// `ButtonRelease` still ungrabs the X pointer; only the work that would
    /// resolve its ids is skipped.
    #[must_use]
    fn pointer_grab_is_live(&self) -> bool {
        self.pointer_grab.is_some_and(|grab| {
            self.state.world.tree.is_live(grab.node)
                && self.state.world.get_desktop(grab.desktop).is_some()
                && self.state.world.get_monitor(grab.monitor).is_some()
        })
    }

    /// The client of `node`, if it holds one; receptacles and branches do not.
    #[must_use]
    fn client_of(&self, node: NodeId) -> Option<&Client> {
        self.node(node).client.as_ref()
    }

    /// The client of a node the caller already knows holds one.
    #[must_use]
    fn client(&self, node: NodeId) -> &Client {
        self.client_of(node)
            .expect("node was expected to hold a client")
    }

    /// The mutable client of a node the caller already knows holds one.
    ///
    /// A `&mut Client` cannot move a node between desktops nor change its
    /// window, so this deliberately keeps the window index intact.
    fn client_mut(&mut self, node: NodeId) -> &mut Client {
        self.state
            .world
            .tree
            .node_mut(node)
            .client
            .as_mut()
            .expect("node was expected to hold a client")
    }

    #[must_use]
    fn monitor_xid(&self, monitor: MonitorId) -> u32 {
        self.world().monitor(monitor).external_id
    }

    #[must_use]
    fn desktop_xid(&self, desktop: DesktopId) -> u32 {
        self.world().desktop(desktop).external_id
    }

    /// A command handler over the daemon state.
    ///
    /// Commands relocate nodes, so obtaining one drops the window index; the
    /// exclusive borrow it holds keeps the index unreachable until it is gone.
    fn command(&mut self) -> CommandHandler<'_> {
        self.invalidate_window_index();
        CommandHandler::new(&mut self.state)
    }

    /// Points the X input focus at `node`, or clears it when there is nothing
    /// focusable, and reports the node that actually took the focus.
    ///
    /// A focused node need not own a client: receptacles are focusable leaves,
    /// so the client must be checked rather than assumed.
    fn apply_focus(&self, x11: &X11, node: Option<NodeId>) -> Result<Option<NodeId>, RuntimeError> {
        let focused = node.filter(|node| self.node(*node).client.is_some());
        if let Some(node) = focused {
            let client = self.client(node);
            window::focus_client(
                x11,
                x::Window::new(self.xid(node)),
                client.icccm.input_hint,
                client.icccm.take_focus,
            )?;
        } else {
            window::clear_focus(x11)?;
        }
        Ok(focused)
    }

    /// Locates the managed client owning `window`.
    ///
    /// Backed by a lazily built index, because nearly every X event resolves a
    /// window and the naive walk is `O(monitors x desktops x tree)`.
    #[doc(hidden)]
    #[must_use]
    pub fn managed_window(&self, window: u32) -> Option<WindowLocation> {
        let found = {
            let mut index = self.window_index.borrow_mut();
            index
                .get_or_insert_with(|| self.build_window_index())
                .get(&window)
                .copied()
        };
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            found,
            self.scan_managed_window(window),
            "window index disagrees with a full scan for 0x{window:08X}"
        );
        found
    }

    fn build_window_index(&self) -> WindowIndex {
        let mut index = HashMap::new();
        for (monitor, desktop, root) in self.world().roots() {
            for node in self.tree().preorder(root) {
                if self.node(node).client.is_some() {
                    index
                        .entry(self.xid(node))
                        .or_insert((monitor, desktop, node));
                }
            }
        }
        index
    }

    /// The pre-index lookup, kept as the debug oracle for the index.
    #[cfg(debug_assertions)]
    fn scan_managed_window(&self, window: u32) -> Option<WindowLocation> {
        self.world().roots().find_map(|(monitor, desktop, root)| {
            let node = self.tree().find_by_external_id(root, window)?;
            self.node(node)
                .client
                .is_some()
                .then_some((monitor, desktop, node))
        })
    }

    fn update_ewmh(&self, x11: &X11) -> Result<(), RuntimeError> {
        ewmh::update_number_of_desktops(x11, self.world())?;
        ewmh::update_desktop_layout(x11, self.world())?;
        ewmh::update_desktop_names(x11, self.world())?;
        ewmh::update_desktop_geometry(x11)?;
        ewmh::update_desktop_viewports(x11, self.world())?;
        ewmh::update_workareas(x11, self.world())?;
        ewmh::update_current_desktop(x11, self.world())?;
        ewmh::update_client_desktops(x11, self.world())?;
        ewmh::update_client_list(x11, self.world())?;
        ewmh::update_client_stacking_list(x11, &self.state.stacking_order)?;
        if self.state.settings.enable_ewmh_allowed_actions {
            self.refresh_ewmh_allowed_actions(x11)?;
        }
        Ok(ewmh::update_active_window(x11, self.world())?)
    }

    fn client_nodes(&self, root: NodeId) -> Vec<NodeId> {
        self.tree()
            .preorder(root)
            .filter(|node| self.node(*node).client.is_some())
            .collect()
    }

    /// Every managed client window, in `all_client_nodes` order.
    fn all_client_windows(&self) -> Vec<x::Window> {
        self.all_client_nodes()
            .into_iter()
            .map(|node| x::Window::new(self.xid(node)))
            .collect()
    }

    /// Monitor identifiers paired with their rectangles, in monitor order.
    fn monitor_rectangles(&self) -> Vec<(MonitorId, Rectangle)> {
        self.world()
            .monitor_order()
            .iter()
            .map(|id| (*id, self.world().monitor(*id).rectangle))
            .collect()
    }

    fn all_client_nodes(&self) -> Vec<NodeId> {
        self.world()
            .roots()
            .flat_map(|(_, _, root)| self.client_nodes(root))
            .collect()
    }
}

impl MessageHandler for DaemonApp {
    fn dispatch(
        &mut self,
        domain: Domain,
        args: &[&[u8]],
        response: &mut dyn Response,
    ) -> io::Result<Option<Subscription>> {
        let config = (domain == Domain::Config).then(|| {
            (
                print_report(self.world(), &self.state.settings),
                self.state.settings.mapping_events_count,
            )
        });

        let subscription = self.command().dispatch(domain, args, response)?;

        if let Some((report, mapping_events_count)) = config {
            if self.state.settings.mapping_events_count != mapping_events_count {
                self.mapping_filter =
                    MappingFilterState::new(self.state.settings.mapping_events_count);
            }
            if report != print_report(self.world(), &self.state.settings) {
                self.broadcast_report();
            }
        }

        Ok(subscription)
    }
}

impl RuntimeApp for DaemonApp {
    fn state(&self) -> &DaemonState {
        &self.state
    }

    fn setup(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        self.lock_masks = crate::pointer::LockMasks::query(x11)?;
        if x11.extensions().sync.is_some() {
            let _ = x11.request(&sync::Initialize {
                desired_major_version: 3,
                desired_minor_version: 1,
            })?;
        }
        self.mapping_filter = MappingFilterState::new(self.state.settings.mapping_events_count);
        if self.motion_recorder.is_none() {
            let window: x::Window = x11.connection().generate_id();
            x11.send_and_check_request(&x::CreateWindow {
                depth: 0,
                wid: window,
                parent: x11.root(),
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                border_width: 0,
                class: x::WindowClass::InputOnly,
                visual: x::COPY_FROM_PARENT,
                value_list: &[x::Cw::EventMask(
                    x::EventMask::STRUCTURE_NOTIFY | x::EventMask::POINTER_MOTION,
                )],
            })?;
            window::set_property(
                x11,
                window,
                x11.atoms().wm_class,
                x::ATOM_STRING,
                b"motion_recorder\0Bspwm\0",
            )?;
            self.motion_recorder = Some(MotionRecorder {
                window: window.resource_id(),
                enabled: false,
            });
        }
        if !self.world().monitor_order().is_empty() {
            return normalize_initial_focus(x11);
        }
        let query = monitor::query_monitor_info(x11);
        self.reconcile_monitor_query(x11, &query)?;
        if query.source == monitor::MonitorInfoSource::Randr
            && let Some(extension) = x11.extensions().randr
        {
            monitor::select_randr_screen_change(x11)?;
            self.state.randr_base = extension.first_event;
        }
        normalize_initial_focus(x11)
    }

    fn restore_state(&mut self, path: &Path, x11: &X11) -> Result<(), RuntimeError> {
        self.load_state(path, x11, true)
    }

    fn restore_inherited_subscribers(
        &mut self,
        fds: &mut InheritedFds,
    ) -> Result<(), RuntimeError> {
        DaemonApp::restore_inherited_subscribers(self, fds)
    }

    fn run_config(&mut self, path: &Path, run_level: u8) -> Result<(), RuntimeError> {
        use std::process::Stdio;
        let mut cmd = Command::new(path);
        cmd.arg(run_level.to_string());
        // Redirect child stderr to BSPWM_CHILD_LOG if set, keeping the
        // daemon's own stderr clean for backtraces and log output.
        if let Ok(log_path) = std::env::var("BSPWM_CHILD_LOG")
            && let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
        {
            cmd.stderr(Stdio::from(file));
        }
        cmd.spawn()?;
        Ok(())
    }

    fn handle_event(
        &mut self,
        event: xcb::Result<xcb::Event>,
        x11: &X11,
    ) -> Result<(), RuntimeError> {
        let mut context = XEventContext { app: self, x11 };
        let result = crate::events::handle_event(&mut context, event)
            .map(|_| ())
            .map_err(|error| match error {
                crate::events::DispatchError::Connection(error) => error.into(),
                crate::events::DispatchError::Handler(error) => error,
            });
        // Handlers restructure trees directly, so no id may survive the event
        // that freed the node behind it.
        self.state.forget_retired_nodes();
        result
    }

    fn execute_pending_effects(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        DaemonApp::execute_pending_effects(self, x11)
    }

    fn poll(&mut self, x11: &X11) -> Result<bool, RuntimeError> {
        let rules = self.poll_pending_rules(x11)?;
        let resize = self.poll_sync_resize_timeout(x11)?;
        let pings = self.poll_ewmh_ping_timeouts();
        Ok(rules || resize || pings)
    }

    fn running(&self) -> bool {
        self.state.running
    }

    fn exit_status(&self) -> i32 {
        self.state.exit_status
    }

    fn set_exit_status(&mut self, status: i32) {
        self.state.exit_status = status;
    }

    fn request_stop(&mut self) {
        self.state.running = false;
    }

    fn cleanup(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        self.terminate_pending_rules();
        self.pending_ewmh_pings.clear();
        if let Some(grab) = self.pointer_grab.take() {
            if let Some(resize) = grab.sync_resize {
                let _ = x11.send_and_check_request(&sync::DestroyAlarm {
                    alarm: resize.alarm,
                });
            }
            crate::pointer::ungrab_pointer(x11)?;
        }
        if let Some(recorder) = self.motion_recorder.take() {
            x11.send_and_check_request(&x::DestroyWindow {
                window: x::Window::new(recorder.window),
            })?;
        }
        for feedback in self.tree().feedback_windows() {
            window::destroy(x11, x::Window::new(feedback))?;
        }
        self.mapped_feedbacks.clear();
        for id in self.world().monitor_order().to_vec() {
            if let Some(root) = self.world_mut().monitor_mut(id).root_id.take() {
                monitor::destroy_monitor_root(x11, root)?;
            }
        }
        if !self.state.restart {
            self.subscribers.clear();
        }
        Ok(())
    }

    fn restarting(&self) -> bool {
        self.state.restart
    }

    fn write_restart_state(&mut self, path: &Path) -> Result<Vec<RawFd>, RuntimeError> {
        DaemonApp::write_restart_state(self, path)
    }

    fn restart_failed(&mut self) {
        self.state.restart = false;
        self.subscribers.clear();
    }

    fn retain_response(
        &mut self,
        response: UnixResponse,
        subscription: Subscription,
    ) -> Result<(), RuntimeError> {
        DaemonApp::retain_response(self, response, subscription)
    }

    fn prune_dead_subscribers(&mut self) {
        DaemonApp::prune_dead_subscribers(self);
    }
}

fn normalize_initial_focus(x11: &X11) -> Result<(), RuntimeError> {
    let focus = x11.request(&x::GetInputFocus {})?.focus().resource_id();
    if focus <= 1 {
        window::clear_focus(x11)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};

    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::daemon::test_support::{TestResponse, app_with_desktop, test_path};

    #[test]
    fn delegates_pure_commands_to_command_handler() {
        let (mut app, _, _) = app_with_desktop();
        let mut response = TestResponse::default();
        app.dispatch(Domain::Config, &[b"window_gap", b"17"], &mut response)
            .unwrap();
        assert_eq!(app.state.settings.window_gap, 17);
        app.dispatch(Domain::Config, &[b"window_gap"], &mut response)
            .unwrap();
        assert_eq!(response.0, b"17\n");
    }

    #[test]
    fn mapping_event_budget_updates_with_runtime_configuration() {
        let (mut app, _, _) = app_with_desktop();
        let mut response = TestResponse::default();
        assert_eq!(app.mapping_filter.pending(), 1);
        app.dispatch(
            Domain::Config,
            &[b"mapping_events_count", b"-1"],
            &mut response,
        )
        .unwrap();
        assert_eq!(app.mapping_filter.pending(), -1);
    }

    #[test]
    fn config_runs_asynchronously_with_positional_run_level() {
        let script = test_path("config");
        let output = test_path("config-output");
        let release = test_path("config-release");
        let done = test_path("config-done");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\ni=0\nwhile [ ! -e '{}' ] && [ \"$i\" -lt 100 ]; do\n  sleep 0.01\n  i=$((i + 1))\ndone\n[ -e '{}' ] || exit 42\n: > '{}'\nexit 23\n",
                output.display(),
                release.display(),
                release.display(),
                done.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

        let start = Instant::now();
        DaemonApp::default().run_config(&script, 1).unwrap();
        assert!(start.elapsed() < Duration::from_millis(500));

        let deadline = Instant::now() + Duration::from_secs(2);
        while !output.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read_to_string(&output).unwrap(), "1");
        fs::write(&release, b"").unwrap();
        while !done.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(done.exists());

        fs::remove_file(script).unwrap();
        fs::remove_file(output).unwrap();
        fs::remove_file(release).unwrap();
        fs::remove_file(done).unwrap();
    }

    #[test]
    fn ewmh_ping_acknowledgement_and_timeout_are_one_shot() {
        let mut app = DaemonApp::default();
        app.state.settings.enable_ewmh_ping = true;
        app.pending_ewmh_pings.insert(
            42,
            PendingEwmhPing {
                timestamp: 7,
                deadline: Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .unwrap(),
                expiration_observed: false,
            },
        );
        assert!(!app.acknowledge_ewmh_ping(42, 8));
        assert!(app.poll_ewmh_ping_timeouts());
        assert!(app.pending_ewmh_pings.contains_key(&42));
        assert!(app.acknowledge_ewmh_ping(42, 7));
        assert!(!app.poll_ewmh_ping_timeouts());

        app.pending_ewmh_pings.insert(
            43,
            PendingEwmhPing {
                timestamp: 9,
                deadline: Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .unwrap(),
                expiration_observed: false,
            },
        );
        assert!(app.poll_ewmh_ping_timeouts());
        assert!(app.poll_ewmh_ping_timeouts());
        assert!(!app.pending_ewmh_pings.contains_key(&43));
        assert!(!app.poll_ewmh_ping_timeouts());
    }
}
