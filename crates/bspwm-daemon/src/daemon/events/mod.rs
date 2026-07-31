//! The X event dispatch context and the handlers built on it.

mod pointer;
mod window;

use std::time::Instant;

use xcb::{Xid, XidNew, randr, sync, x};

use super::status::node_geometry_status;
use super::{
    DaemonApp, PointerGrab, SYNC_RESIZE_TIMEOUT, SyncResize, sync_i64, sync_int64,
    timestamp_is_later,
};
use crate::events::EventHandler;
use crate::monitor;
use crate::runtime::RuntimeError;
use crate::state::{CommandEffect, FocusPolicy};
use crate::tree::{Client, Node, NodeId};
use crate::types::{ClientState, Point, Rectangle, SubscriberMask, WmFlags};
use crate::world::{DesktopId, MonitorId, World};
use crate::x11::X11;

#[doc(hidden)]
pub struct XEventContext<'a> {
    pub app: &'a mut DaemonApp,
    pub x11: &'a X11,
}

impl XEventContext<'_> {
    fn begin_sync_resize(&self, node: NodeId) -> Option<SyncResize> {
        if !self.app.state.settings.pointer_resize_sync || self.x11.extensions().sync.is_none() {
            return None;
        }
        let counter = *self.app.sync_request_clients.get(&self.xid(node))?;
        let current = self
            .x11
            .request(&sync::QueryCounter { counter })
            .ok()?
            .counter_value();
        let value = sync_i64(current).wrapping_add(1);

        // Create an alarm that fires when the counter reaches our target value.
        let alarm: sync::Alarm = self.x11.connection().generate_id();
        let target = sync_int64(value);
        self.x11
            .send_and_check_request(&sync::CreateAlarm {
                id: alarm,
                value_list: &[
                    sync::Ca::Counter(counter),
                    sync::Ca::ValueType(sync::Valuetype::Absolute),
                    sync::Ca::Value(target),
                    sync::Ca::TestType(sync::Testtype::PositiveComparison),
                    sync::Ca::Delta(sync::Int64 { hi: 0, lo: 0 }),
                    sync::Ca::Events(1),
                ],
            })
            .ok()?;

        Some(SyncResize {
            counter,
            alarm,
            value,
            in_flight: false,
            pending: None,
            deadline: Instant::now() + SYNC_RESIZE_TIMEOUT,
        })
    }

    fn send_sync_resize(
        &self,
        grab: &mut PointerGrab,
        rectangle: Rectangle,
        timestamp: x::Timestamp,
    ) {
        let window = x::Window::new(self.xid(grab.node));
        let Some(resize) = grab.sync_resize.as_mut() else {
            crate::window::queue_move_resize(self.x11, window, rectangle);
            return;
        };
        if resize.in_flight {
            resize.pending = Some((rectangle, timestamp));
            return;
        }
        // Update the alarm target for the new value.
        let target = sync_int64(resize.value);
        let _ = self.x11.send_and_check_request(&sync::ChangeAlarm {
            id: resize.alarm,
            value_list: &[sync::Ca::Value(target)],
        });
        crate::window::queue_sync_request(self.x11, window, timestamp, resize.value);
        crate::window::queue_move_resize(self.x11, window, rectangle);
        resize.in_flight = true;
        resize.deadline = Instant::now() + SYNC_RESIZE_TIMEOUT;
    }

    fn finish_sync_resize(&self, grab: &mut PointerGrab) -> Result<(), RuntimeError> {
        let Some(mut resize) = grab.sync_resize.take() else {
            return Ok(());
        };
        if let Some((rectangle, timestamp)) = resize.pending.take() {
            resize.value = resize.value.wrapping_add(1);
            let window = x::Window::new(self.xid(grab.node));
            crate::window::queue_sync_request(self.x11, window, timestamp, resize.value);
            crate::window::queue_move_resize(self.x11, window, rectangle);
        }
        // Clean up the alarm.
        let _ = self.x11.send_and_check_request(&sync::DestroyAlarm {
            alarm: resize.alarm,
        });
        self.x11.flush()?;
        Ok(())
    }

    fn finish_pointer_grab(&mut self) -> Result<(), RuntimeError> {
        let live = self.app.pointer_grab_is_live();
        let Some(mut grab) = self.app.pointer_grab.take() else {
            return Ok(());
        };
        if live {
            self.finish_sync_resize(&mut grab)?;
        }
        crate::pointer::ungrab_pointer(self.x11)?;
        if !live {
            return Ok(());
        }
        self.pointer_status(grab, "end");
        let tiled_change = self.client_of(grab.node).is_some_and(|client| {
            grab.action == crate::types::PointerAction::Move && client.state.is_tiled()
                || matches!(
                    grab.action,
                    crate::types::PointerAction::ResizeSide
                        | crate::types::PointerAction::ResizeCorner
                ) && client.state == ClientState::Tiled
        });
        if tiled_change {
            self.publish_desktop_geometry(grab.monitor, grab.desktop);
        } else {
            self.publish_geometry(grab.monitor, grab.desktop, grab.node);
        }
        Ok(())
    }

    #[must_use]
    fn world(&self) -> &World {
        self.app.world()
    }

    fn world_mut(&mut self) -> &mut World {
        self.app.world_mut()
    }

    #[must_use]
    fn tree(&self) -> &crate::tree::Tree {
        self.app.tree()
    }

    fn tree_mut(&mut self) -> &mut crate::tree::Tree {
        self.app.tree_mut()
    }

    #[must_use]
    fn node(&self, node: NodeId) -> &Node {
        self.app.node(node)
    }

    fn node_mut(&mut self, node: NodeId) -> &mut Node {
        self.app.node_mut(node)
    }

    #[must_use]
    fn xid(&self, node: NodeId) -> u32 {
        self.app.xid(node)
    }

    #[must_use]
    fn client_of(&self, node: NodeId) -> Option<&Client> {
        self.app.client_of(node)
    }

    #[must_use]
    fn client(&self, node: NodeId) -> &Client {
        self.app.client(node)
    }

    fn client_mut(&mut self, node: NodeId) -> &mut Client {
        self.app.client_mut(node)
    }

    #[must_use]
    fn monitor_xid(&self, monitor: MonitorId) -> u32 {
        self.app.monitor_xid(monitor)
    }

    #[must_use]
    fn desktop_ids(&self, monitor: MonitorId, desktop: DesktopId) -> String {
        self.app.desktop_ids(monitor, desktop)
    }

    #[must_use]
    fn node_ids(&self, monitor: MonitorId, desktop: DesktopId, node: NodeId) -> String {
        self.app.node_ids(monitor, desktop, node)
    }

    fn publish(&mut self, mask: SubscriberMask, status: &str) {
        self.app.publish(mask, status);
    }

    fn arrange_and_publish(
        &mut self,
        location: Option<(MonitorId, DesktopId)>,
    ) -> Result<(), RuntimeError> {
        if let Some((monitor, desktop)) = location {
            self.app.arrange_desktop(self.x11, monitor, desktop)?;
        }
        self.app.execute_pending_effects(self.x11)?;
        self.app.update_ewmh(self.x11)
    }

    fn focus(
        &mut self,
        monitor: MonitorId,
        desktop: DesktopId,
        node: Option<NodeId>,
        activate: bool,
    ) -> Result<bool, RuntimeError> {
        let policy = FocusPolicy::configured(&self.app.state);
        self.focus_with(monitor, desktop, node, activate, policy)
    }

    /// [`Self::focus`] under an explicit focus policy.
    ///
    /// Pointer- and focus-driven corrections pass a policy with the knobs they
    /// must not honour cleared, instead of temporarily overwriting the
    /// persisted `pointer_follows_*` settings and `auto_raise` they come from.
    fn focus_with(
        &mut self,
        monitor: MonitorId,
        desktop: DesktopId,
        node: Option<NodeId>,
        activate: bool,
        policy: FocusPolicy,
    ) -> Result<bool, RuntimeError> {
        let changed = self.app.command().focus_location_with(
            crate::query::Coordinates {
                monitor: Some(monitor),
                desktop: Some(desktop),
                node,
            },
            activate,
            policy,
        );
        self.app.execute_pending_effects(self.x11)?;
        Ok(changed)
    }

    /// Focuses whatever a click at `position` landed on, reporting whether the
    /// focus actually moved.
    fn focus_clicked_window(
        &mut self,
        window: x::Window,
        position: Point,
        policy: FocusPolicy,
    ) -> Result<bool, RuntimeError> {
        if let Some((monitor, desktop, node)) = self.app.managed_window(window.resource_id()) {
            // Upstream compares against mon->desk->focus, i.e. the globally
            // focused node, not the clicked node's desktop focus. This ensures
            // cross-monitor clicks always trigger a focus change.
            let globally_focused = self
                .world()
                .focused_monitor
                .and_then(|m| self.world().monitor(m).active_desktop)
                .and_then(|d| self.world().desktop(d).tree.focus);
            if globally_focused == Some(node) {
                // Upstream stacks the already-focused node on click under FFP.
                if self.app.state.settings.focus_follows_pointer {
                    let mut backend = super::monitors::X11StackBackend::new(self.x11);
                    let result = self
                        .app
                        .state
                        .stacking_order
                        .raise_in_level(&mut backend, self.xid(node));
                    self.app.complete_stack_operation(backend, result)?;
                    if let Some(desktop_id) = self.app.world().node_desktop(node) {
                        self.app.sync_stacking_ewmh(self.x11, desktop_id)?;
                    }
                }
                return Ok(false);
            }
            return self.focus_with(monitor, desktop, Some(node), false, policy);
        }
        let Some(monitor) = self.monitor_at(position) else {
            return Ok(false);
        };
        let root = self.world().monitor(monitor).root_id;
        if self.world().focused_monitor == Some(monitor)
            || !(window.is_none() || root == Some(window.resource_id()))
        {
            return Ok(false);
        }
        let Some(desktop) = self.world().monitor(monitor).active_desktop else {
            return Ok(false);
        };
        let node = self.world().desktop(desktop).tree.focus;
        self.focus_with(monitor, desktop, node, false, policy)
    }

    fn monitor_at(&self, point: Point) -> Option<MonitorId> {
        monitor::monitor_from_point(&self.app.monitor_rectangles(), point)
    }

    fn cancel_grab_for_window(&mut self, window: u32) -> Result<(), RuntimeError> {
        let matches = self.app.pointer_grab.is_some_and(|grab| {
            self.app
                .state
                .world
                .tree
                .get(grab.node)
                .is_some_and(|node| node.external_id == window)
        });
        if matches {
            crate::pointer::ungrab_pointer(self.x11)?;
            self.app.pointer_grab = None;
        }
        Ok(())
    }

    fn query_pointer(&mut self) -> Result<(x::Window, Point), RuntimeError> {
        let enabled = self
            .app
            .motion_recorder
            .is_some_and(|recorder| recorder.enabled);
        if enabled {
            let recorder = self.app.motion_recorder.as_mut().unwrap();
            self.x11.send_and_check_request(&x::UnmapWindow {
                window: x::Window::new(recorder.window),
            })?;
            recorder.enabled = false;
        }
        let result = crate::pointer::query_pointer(self.x11)
            .map(|(window, point)| (self.resolve_pointer_window(window, point), point))
            .map_err(RuntimeError::from);
        if enabled {
            let recorder = self.app.motion_recorder.as_mut().unwrap();
            self.x11.send_and_check_request(&x::MapWindow {
                window: x::Window::new(recorder.window),
            })?;
            recorder.enabled = true;
        }
        result
    }

    fn resolve_pointer_window(&self, window: x::Window, point: Point) -> x::Window {
        if window.is_none() {
            if let Some(monitor) = self.monitor_at(point)
                && let Some(desktop) = self.world().monitor(monitor).active_desktop
                && let Some(root) = self.world().desktop(desktop).tree.root
            {
                let gap = self.world().desktop(desktop).window_gap;
                for leaf in self.tree().leaves(root) {
                    let value = self.node(leaf);
                    if value.client.is_some() {
                        continue;
                    }
                    let mut rectangle = value.rectangle;
                    rectangle.width -= gap;
                    rectangle.height -= gap;
                    if crate::geometry::is_inside(point, rectangle) {
                        return x::Window::new(value.external_id);
                    }
                }
            }
            return window;
        }

        let is_feedback = self
            .tree()
            .feedback_windows()
            .contains(&window.resource_id());
        for xid in self.app.state.stacking_order.windows().into_iter().rev() {
            let Some((_, _, node)) = self.app.managed_window(xid) else {
                continue;
            };
            let value = self.node(node);
            let Some(client) = value.client.as_ref() else {
                continue;
            };
            if !client.shown || value.hidden {
                continue;
            }
            let rectangle = if client.state == ClientState::Floating {
                client.floating_rectangle
            } else {
                client.tiled_rectangle
            };
            if crate::geometry::is_inside(point, rectangle) {
                if xid == window.resource_id() || is_feedback {
                    return x::Window::new(xid);
                }
                break;
            }
        }
        window
    }

    fn pointer_status(&mut self, grab: PointerGrab, phase: &str) {
        let status = format!(
            "pointer_action {} {} {phase}\n",
            self.node_ids(grab.monitor, grab.desktop, grab.node),
            grab.action
        );
        self.publish(SubscriberMask::POINTER_ACTION, &status);
    }

    fn publish_geometry(&mut self, monitor: MonitorId, desktop: DesktopId, node: NodeId) {
        let item = self.node(node);
        let Some(client) = item.client.as_ref() else {
            return;
        };
        let rectangle = if client.state == ClientState::Floating {
            client.floating_rectangle
        } else {
            client.tiled_rectangle
        };
        let status = node_geometry_status(
            self.monitor_xid(monitor),
            self.app.desktop_xid(desktop),
            item.external_id,
            rectangle,
        );
        self.publish(SubscriberMask::NODE_GEOMETRY, &status);
    }

    fn publish_desktop_geometry(&mut self, monitor: MonitorId, desktop: DesktopId) {
        let Some(root) = self.world().desktop(desktop).tree.root else {
            return;
        };
        let nodes = self.app.client_nodes(root);
        for node in nodes {
            if self
                .client_of(node)
                .is_some_and(|client| client.state.is_tiled())
            {
                self.publish_geometry(monitor, desktop, node);
            }
        }
    }

    fn transfer_grabbed_node(
        &mut self,
        grab: &mut PointerGrab,
        destination_monitor: MonitorId,
    ) -> Result<bool, RuntimeError> {
        if destination_monitor == grab.monitor {
            return Ok(false);
        }
        let Some(destination_desktop) = self.world().monitor(destination_monitor).active_desktop
        else {
            return Ok(false);
        };
        let source_rectangle = self.world().monitor(grab.monitor).rectangle;
        let destination_rectangle = self.world().monitor(destination_monitor).rectangle;
        let monitors = self.app.monitor_rectangles();
        let adapt_geometry = self.client_of(grab.node).is_none_or(|client| {
            monitor::monitor_from_client(&monitors, client.floating_rectangle)
                != Some(destination_monitor)
        });
        let anchor = self.world().desktop(destination_desktop).tree.focus;
        let split_ratio = self.app.state.settings.split_ratio;
        let Ok(moved) =
            self.world_mut()
                .transfer_node(grab.node, destination_desktop, anchor, split_ratio)
        else {
            return Ok(false);
        };
        if adapt_geometry && let Some(client) = self.node_mut(grab.node).client.as_mut() {
            client.floating_rectangle = crate::window::adapt_geometry(
                client.floating_rectangle,
                source_rectangle,
                destination_rectangle,
            );
        }
        let anchor_id = anchor.map_or(0, |node| self.xid(node));
        let status = format!(
            "node_transfer {} {}\n",
            self.node_ids(moved.source_monitor, moved.source_desktop, grab.node),
            self.app
                .node_ids_raw(destination_monitor, destination_desktop, anchor_id),
        );
        self.publish(SubscriberMask::NODE_TRANSFER, &status);
        self.app
            .arrange_desktop_quiet(self.x11, moved.source_monitor, moved.source_desktop)?;
        self.app
            .arrange_desktop_quiet(self.x11, destination_monitor, destination_desktop)?;
        grab.monitor = destination_monitor;
        grab.desktop = destination_desktop;
        self.app.update_ewmh(self.x11)?;
        Ok(true)
    }

    fn set_urgent(
        &mut self,
        monitor: MonitorId,
        desktop: DesktopId,
        node: NodeId,
        urgent: bool,
    ) -> Result<(), RuntimeError> {
        let focused = self.world().focused_monitor == Some(monitor)
            && self.world().monitor(monitor).active_desktop == Some(desktop)
            && self.world().desktop(desktop).tree.focus == Some(node);
        if urgent && focused {
            return Ok(());
        }
        let Some(client) = self.node_mut(node).client.as_mut() else {
            return Ok(());
        };
        client.urgent = urgent;
        self.app.sync_window_state(self.x11, node)?;
        self.publish_node_flag(monitor, desktop, node, "urgent", urgent, true);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn handle_wm_state(
        &mut self,
        monitor: MonitorId,
        desktop: DesktopId,
        node: NodeId,
        state: x::Atom,
        action: u32,
    ) -> Result<(), RuntimeError> {
        if state.is_none() || action > 2 {
            return Ok(());
        }
        let atoms = self.x11.atoms();
        let toggle = |current: bool| match action {
            0 => false,
            1 => true,
            _ => !current,
        };
        if state == atoms.net_wm_state_fullscreen {
            let current = self
                .client_of(node)
                .is_some_and(|client| client.state == crate::types::ClientState::Fullscreen);
            let enter = toggle(current);
            let transition = if enter {
                crate::types::StateTransitions::ENTER
            } else {
                crate::types::StateTransitions::EXIT
            };
            let ignored = self
                .app
                .state
                .settings
                .ignore_ewmh_fullscreen
                .contains(transition);
            if !ignored && enter != current {
                let next = if enter {
                    crate::types::ClientState::Fullscreen
                } else {
                    self.client(node).last_state
                };
                let old = self.client(node).state;
                let old_layout = self.world().desktop(desktop).layout;
                self.app
                    .command()
                    .set_node_state(monitor, desktop, node, next);
                self.app.state.pending_effects.extend([
                    CommandEffect::Restack {
                        node,
                        auto_raise: self.app.state.auto_raise,
                    },
                    CommandEffect::SyncWindowState { node },
                    CommandEffect::SyncEwmh,
                ]);
                for (state, enabled) in [(old, false), (next, true)] {
                    let state = state.protocol_name();
                    let status = format!(
                        "node_state {} {state} {}\n",
                        self.node_ids(monitor, desktop, node),
                        if enabled { "on" } else { "off" },
                    );
                    self.publish(SubscriberMask::NODE_STATE, &status);
                }
                let active_focus = self
                    .world()
                    .monitor(monitor)
                    .active_desktop
                    .and_then(|active| self.world().desktop(active).tree.focus);
                if active_focus == Some(node) {
                    self.app.broadcast_report();
                }
                let new_layout = self.world().desktop(desktop).layout;
                if new_layout != old_layout {
                    let layout = new_layout.protocol_name();
                    let ids = self.desktop_ids(monitor, desktop);
                    let status = format!("desktop_layout {ids} {layout}\n");
                    self.publish(SubscriberMask::DESKTOP_LAYOUT, &status);
                    if self.world().monitor(monitor).active_desktop == Some(desktop) {
                        self.app.broadcast_report();
                    }
                }
            }
            self.app
                .state
                .pending_effects
                .push(CommandEffect::Arrange { monitor, desktop });
        } else if state == atoms.net_wm_state_below || state == atoms.net_wm_state_above {
            let requested = if state == atoms.net_wm_state_below {
                crate::types::StackLayer::Below
            } else {
                crate::types::StackLayer::Above
            };
            let client = self.client(node);
            let next = if toggle(client.layer == requested) {
                requested
            } else if client.layer == requested {
                client.last_layer
            } else {
                client.layer
            };
            if self.tree_mut().set_layer(node, next) {
                self.app.state.pending_effects.extend([
                    CommandEffect::Restack {
                        node,
                        auto_raise: self.app.state.auto_raise,
                    },
                    CommandEffect::SyncWindowState { node },
                    CommandEffect::SyncEwmh,
                ]);
                let layer = next.protocol_name();
                let ids = self.node_ids(monitor, desktop, node);
                self.publish(
                    SubscriberMask::NODE_LAYER,
                    &format!("node_layer {ids} {layer}\n"),
                );
            }
        } else if state == atoms.net_wm_state_hidden {
            let value = toggle(self.node(node).hidden);
            if self
                .tree_mut()
                .set_flag(node, crate::tree::NodeFlag::Hidden, value)
            {
                self.app.state.pending_effects.extend([
                    CommandEffect::SetWindowVisibility {
                        node,
                        visible: !value,
                    },
                    CommandEffect::Arrange { monitor, desktop },
                    CommandEffect::SyncWindowState { node },
                ]);
                self.publish_node_flag(monitor, desktop, node, "hidden", value, false);
            }
        } else if state == atoms.net_wm_state_sticky {
            let value = toggle(self.node(node).sticky);
            if value
                && self.world().monitor(monitor).active_desktop != Some(desktop)
                && let Some(active) = self.world().monitor(monitor).active_desktop
                && self
                    .transfer_node((monitor, desktop, node), (monitor, active))
                    .is_ok()
            {
                self.app.execute_pending_effects(self.x11)?;
            }
            if self
                .tree_mut()
                .set_flag(node, crate::tree::NodeFlag::Sticky, value)
            {
                let count = &mut self.world_mut().monitor_mut(monitor).sticky_count;
                *count = if value {
                    count.saturating_add(1)
                } else {
                    count.saturating_sub(1)
                };
                self.app
                    .state
                    .pending_effects
                    .push(CommandEffect::SyncWindowState { node });
                let report = self
                    .world()
                    .monitor(monitor)
                    .active_desktop
                    .and_then(|active| self.world().desktop(active).tree.focus)
                    == Some(node);
                self.publish_node_flag(monitor, desktop, node, "sticky", value, report);
            }
        } else if state == atoms.net_wm_state_demands_attention {
            let current = self.client_of(node).is_some_and(|client| client.urgent);
            self.set_urgent(monitor, desktop, node, toggle(current))?;
        } else if let Some(flag) = match state {
            value if value == atoms.net_wm_state_modal => Some(WmFlags::MODAL),
            value if value == atoms.net_wm_state_maximized_vert => Some(WmFlags::MAXIMIZED_VERT),
            value if value == atoms.net_wm_state_maximized_horz => Some(WmFlags::MAXIMIZED_HORZ),
            value if value == atoms.net_wm_state_shaded => Some(WmFlags::SHADED),
            value if value == atoms.net_wm_state_skip_taskbar => Some(WmFlags::SKIP_TASKBAR),
            value if value == atoms.net_wm_state_skip_pager => Some(WmFlags::SKIP_PAGER),
            _ => None,
        } {
            let client = self.client_mut(node);
            client.wm_flags = match action {
                0 => client.wm_flags.difference(flag),
                1 => client.wm_flags.union(flag),
                _ => client.wm_flags ^ flag,
            };
            self.app.sync_window_state(self.x11, node)?;
        }
        self.app.execute_pending_effects(self.x11)
    }

    /// Moves `node` from `(monitor, desktop)` to `destination`, anchored on the
    /// destination's focus.
    fn transfer_node(
        &mut self,
        source: (MonitorId, DesktopId, NodeId),
        destination: (MonitorId, DesktopId),
    ) -> Result<(), crate::tree::StructuralError> {
        let (monitor, desktop, node) = source;
        let (destination_monitor, destination_desktop) = destination;
        let anchor = self.world().desktop(destination_desktop).tree.focus;
        self.app
            .command()
            .transfer_node_complete(
                crate::query::Coordinates {
                    monitor: Some(monitor),
                    desktop: Some(desktop),
                    node: Some(node),
                },
                crate::query::Coordinates {
                    monitor: Some(destination_monitor),
                    desktop: Some(destination_desktop),
                    node: anchor,
                },
                false,
            )
            .map(|_| ())
    }

    fn publish_node_flag(
        &mut self,
        monitor: MonitorId,
        desktop: DesktopId,
        node: NodeId,
        flag: &str,
        value: bool,
        report: bool,
    ) {
        let status = format!(
            "node_flag {} {flag} {}\n",
            self.node_ids(monitor, desktop, node),
            if value { "on" } else { "off" },
        );
        self.publish(SubscriberMask::NODE_FLAG, &status);
        if report {
            self.app.broadcast_report();
        }
    }
}

impl EventHandler for XEventContext<'_> {
    type Error = RuntimeError;

    fn map_request(&mut self, event: &x::MapRequestEvent) -> Result<(), Self::Error> {
        self.on_map_request(event)
    }

    fn destroy_notify(&mut self, event: &x::DestroyNotifyEvent) -> Result<(), Self::Error> {
        self.on_destroy_notify(event)
    }

    fn unmap_notify(&mut self, event: &x::UnmapNotifyEvent) -> Result<(), Self::Error> {
        self.on_unmap_notify(event)
    }

    fn configure_request(&mut self, event: &x::ConfigureRequestEvent) -> Result<(), Self::Error> {
        self.on_configure_request(event)
    }

    fn configure_notify(&mut self, event: &x::ConfigureNotifyEvent) -> Result<(), Self::Error> {
        self.on_configure_notify(event)
    }

    fn client_message(&mut self, event: &x::ClientMessageEvent) -> Result<(), Self::Error> {
        self.on_client_message(event)
    }

    fn property_notify(&mut self, event: &x::PropertyNotifyEvent) -> Result<(), Self::Error> {
        self.on_property_notify(event)
    }

    fn enter_notify(&mut self, event: &x::EnterNotifyEvent) -> Result<(), Self::Error> {
        self.on_enter_notify(event)
    }

    fn motion_notify(&mut self, event: &x::MotionNotifyEvent) -> Result<(), Self::Error> {
        self.on_motion_notify(event)
    }

    fn button_press(&mut self, event: &x::ButtonPressEvent) -> Result<(), Self::Error> {
        self.on_button_press(event)
    }

    fn button_release(&mut self, event: &x::ButtonReleaseEvent) -> Result<(), Self::Error> {
        self.on_button_release(event)
    }

    fn focus_in(&mut self, event: &x::FocusInEvent) -> Result<(), Self::Error> {
        self.on_focus_in(event)
    }

    fn mapping_notify(&mut self, event: &x::MappingNotifyEvent) -> Result<(), Self::Error> {
        if !self.app.mapping_filter.should_regrab(event.request()) {
            return Ok(());
        }
        self.app.regrab_client_buttons(self.x11)
    }

    fn randr_screen_change_notify(
        &mut self,
        _event: &randr::ScreenChangeNotifyEvent,
    ) -> Result<(), Self::Error> {
        self.app.reconcile_randr_monitors(self.x11)
    }

    fn randr_notify(&mut self, _event: &randr::NotifyEvent) -> Result<(), Self::Error> {
        self.app.reconcile_randr_monitors(self.x11)
    }

    fn sync_alarm_notify(&mut self, event: &sync::AlarmNotifyEvent) -> Result<(), Self::Error> {
        let Some(mut grab) = self.app.pointer_grab else {
            return Ok(());
        };
        let Some(mut resize) = grab.sync_resize else {
            return Ok(());
        };
        if event.alarm() != resize.alarm || !resize.in_flight {
            return Ok(());
        }
        // Counter has reached our target value -- app has repainted.
        resize.in_flight = false;
        resize.value = resize.value.wrapping_add(1);
        if let Some((rectangle, timestamp)) = resize.pending.take() {
            // Send the next coalesced geometry.
            let window = x::Window::new(self.xid(grab.node));
            // Update alarm target for the new value.
            let target = sync_int64(resize.value);
            let _ = self.x11.send_and_check_request(&sync::ChangeAlarm {
                id: resize.alarm,
                value_list: &[sync::Ca::Value(target)],
            });
            crate::window::queue_sync_request(self.x11, window, timestamp, resize.value);
            crate::window::queue_move_resize(self.x11, window, rectangle);
            resize.in_flight = true;
            resize.deadline = Instant::now() + SYNC_RESIZE_TIMEOUT;
        }
        grab.sync_resize = Some(resize);
        self.app.pointer_grab = Some(grab);
        Ok(())
    }

    fn protocol_error(&mut self, error: &xcb::ProtocolError) -> Result<(), Self::Error> {
        // Asynchronous protocol errors are reported for requests the window
        // manager already moved past, and upstream bspwm keeps running after
        // them. Report and continue rather than tearing down every client's
        // session over one rejected request.
        log::warn!("X protocol error: {error}");
        Ok(())
    }
}
