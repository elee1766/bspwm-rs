//! Handlers for the events that describe a window's life and geometry.

use xcb::{Xid, XidNew, x};

use super::XEventContext;
use crate::daemon::DaemonApp;
use crate::daemon::action::{XAction, set_wm_state_property};
use crate::daemon::status::node_geometry_status;
use crate::daemon::{PointerGrab, PointerGrabOrigin};
use crate::events::{self, ConfigureRequestPlan};
use crate::ewmh;
use crate::monitor;
use crate::runtime::RuntimeError;
use crate::types::{
    ClientState, Point, PointerAction, Rectangle, ResizeHandle, SubscriberMask, wrapping_i16,
};
use crate::window;

/// The size a configure request asks for, defaulting each unmasked axis to the
/// client's current size.
fn requested_size(event: &x::ConfigureRequestEvent, current: Rectangle) -> (i32, i32) {
    let mask = event.value_mask();
    (
        if mask.contains(x::ConfigWindowMask::WIDTH) {
            i32::from(event.width())
        } else {
            current.width
        },
        if mask.contains(x::ConfigWindowMask::HEIGHT) {
            i32::from(event.height())
        } else {
            current.height
        },
    )
}

impl XEventContext<'_> {
    fn refresh_client_protocols(
        &mut self,
        window: x::Window,
        node: crate::tree::NodeId,
    ) -> Result<(), RuntimeError> {
        let protocols = window::get_property::<u32>(
            self.x11,
            window,
            self.x11.atoms().wm_protocols,
            x::ATOM_ATOM,
        )?;
        let atoms = self.x11.atoms();
        let client = self.client_mut(node);
        client.icccm.take_focus = protocols.contains(&atoms.wm_take_focus.resource_id());
        client.icccm.delete_window = protocols.contains(&atoms.wm_delete_window.resource_id());
        client.icccm.ping = protocols.contains(&atoms.net_wm_ping.resource_id());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_moveresize_window(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
        node: crate::tree::NodeId,
        gravity: u8,
        flags: u8,
        requested_x: i32,
        requested_y: i32,
        requested_width: u32,
        requested_height: u32,
    ) -> Result<(), RuntimeError> {
        let client = self.client(node).clone();
        if client.state != ClientState::Floating {
            let rectangle = if client.state == ClientState::Fullscreen {
                self.world().monitor(monitor).rectangle
            } else {
                client.tiled_rectangle
            };
            return DaemonApp::execute_plan(
                self.x11,
                &[XAction::SyntheticConfigure {
                    window: self.xid(node),
                    rectangle,
                    border_width: u16::try_from(client.border_width).unwrap_or(u16::MAX),
                }],
            );
        }

        let old = client.floating_rectangle;
        let mut rectangle = old;
        if flags & 0b0100 != 0 {
            rectangle.width = i32::try_from(requested_width).unwrap_or(i32::MAX).max(1);
        }
        if flags & 0b1000 != 0 {
            rectangle.height = i32::try_from(requested_height).unwrap_or(i32::MAX).max(1);
        }
        let (width, height) =
            crate::arrange::apply_size_hints(&client, rectangle.width, rectangle.height);
        rectangle.width = width;
        rectangle.height = height;

        let border = i32::try_from(client.border_width).unwrap_or(i32::MAX);
        let old_outer = (
            old.width.saturating_add(border.saturating_mul(2)),
            old.height.saturating_add(border.saturating_mul(2)),
        );
        let new_outer = (
            rectangle.width.saturating_add(border.saturating_mul(2)),
            rectangle.height.saturating_add(border.saturating_mul(2)),
        );
        let old_offset = gravity_offset(gravity, old_outer.0, old_outer.1);
        let new_offset = gravity_offset(gravity, new_outer.0, new_outer.1);
        rectangle.x = if flags & 0b0001 != 0 {
            requested_x.saturating_sub(new_offset.x)
        } else {
            old.x
                .saturating_add(old_offset.x)
                .saturating_sub(new_offset.x)
        };
        rectangle.y = if flags & 0b0010 != 0 {
            requested_y.saturating_sub(new_offset.y)
        } else {
            old.y
                .saturating_add(old_offset.y)
                .saturating_sub(new_offset.y)
        };

        self.client_mut(node).floating_rectangle = rectangle;
        window::move_resize(self.x11, x::Window::new(self.xid(node)), rectangle)?;
        self.publish_geometry(monitor, desktop, node);
        let monitors = self.app.monitor_rectangles();
        if let Some(destination_monitor) = monitor::monitor_from_client(&monitors, rectangle)
            && destination_monitor != monitor
            && let Some(destination_desktop) =
                self.world().monitor(destination_monitor).active_desktop
        {
            self.transfer_node(
                (monitor, desktop, node),
                (destination_monitor, destination_desktop),
            )
            .map_err(|error| RuntimeError::X11(format!("moveresize transfer failed: {error:?}")))?;
            self.client_mut(node).floating_rectangle = rectangle;
            self.app.execute_pending_effects(self.x11)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_wm_moveresize(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
        node: crate::tree::NodeId,
        root_x: i32,
        root_y: i32,
        direction: u32,
        button: u8,
    ) -> Result<(), RuntimeError> {
        if direction == 11 {
            if self.app.pointer_grab.is_some_and(|grab| {
                grab.node == node && matches!(grab.origin, PointerGrabOrigin::Ewmh { .. })
            }) {
                self.finish_pointer_grab()?;
            }
            return Ok(());
        }
        let Some((action, handle)) = moveresize_action(direction) else {
            return Ok(());
        };
        if self.app.pointer_grab.is_some()
            || self.client(node).state != ClientState::Floating
            || crate::pointer::grab_pointer(self.x11)? != x::GrabStatus::Success
        {
            return Ok(());
        }
        let grab = PointerGrab {
            monitor,
            desktop,
            node,
            action,
            handle,
            last_position: Point {
                x: i32::from(wrapping_i16(root_x)),
                y: i32::from(wrapping_i16(root_y)),
            },
            last_motion_time: 0,
            origin: PointerGrabOrigin::Ewmh { button },
            sync_resize: matches!(
                action,
                PointerAction::ResizeSide | PointerAction::ResizeCorner
            )
            .then(|| self.begin_sync_resize(node))
            .flatten(),
        };
        self.app.pointer_grab = Some(grab);
        self.pointer_status(grab, "begin");
        Ok(())
    }

    pub(super) fn on_map_request(
        &mut self,
        event: &x::MapRequestEvent,
    ) -> Result<(), RuntimeError> {
        let id = event.window().resource_id();
        let _ = self.app.schedule_window(self.x11, id)?;
        Ok(())
    }

    pub(super) fn on_destroy_notify(
        &mut self,
        event: &x::DestroyNotifyEvent,
    ) -> Result<(), RuntimeError> {
        self.cancel_grab_for_window(event.window().resource_id())?;
        let location = self.app.forget_window(event.window().resource_id());
        self.arrange_and_publish(location)
    }

    pub(super) fn on_unmap_notify(
        &mut self,
        event: &x::UnmapNotifyEvent,
    ) -> Result<(), RuntimeError> {
        if self
            .app
            .motion_recorder
            .is_some_and(|recorder| recorder.window == event.window().resource_id())
        {
            self.app
                .pointer_filter
                .record_motion_recorder_unmap(event.sequence());
            return Ok(());
        }
        if !window::exists(self.x11, event.window()) {
            return Ok(());
        }
        set_wm_state_property(self.x11, event.window(), 0)?;
        self.cancel_grab_for_window(event.window().resource_id())?;
        let location = self.app.forget_window(event.window().resource_id());
        self.arrange_and_publish(location)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn on_configure_request(
        &mut self,
        event: &x::ConfigureRequestEvent,
    ) -> Result<(), RuntimeError> {
        let id = event.window().resource_id();
        if let Some((monitor, desktop, node)) = self.app.managed_window(id) {
            let Some(client) = self.node(node).client.clone() else {
                return Ok(());
            };
            let mask = event.value_mask();
            if client.state == crate::types::ClientState::Floating {
                let mut rectangle = client.floating_rectangle;
                if mask.contains(x::ConfigWindowMask::X) {
                    let requested = i32::from(event.x())
                        .wrapping_sub(i32::try_from(client.border_width).unwrap_or(i32::MAX));
                    // Ignore negative sentinel positions (e.g. Steam uses -1).
                    if requested >= 0 {
                        rectangle.x = requested;
                    }
                }
                if mask.contains(x::ConfigWindowMask::Y) {
                    let requested = i32::from(event.y())
                        .wrapping_sub(i32::try_from(client.border_width).unwrap_or(i32::MAX));
                    if requested >= 0 {
                        rectangle.y = requested;
                    }
                }
                let (width, height) = requested_size(event, rectangle);
                let (width, height) = crate::arrange::apply_size_hints(&client, width, height);
                rectangle.width = width;
                rectangle.height = height;
                self.client_mut(node).floating_rectangle = rectangle;
                window::move_resize(self.x11, event.window(), rectangle)?;
                let status = node_geometry_status(
                    self.monitor_xid(monitor),
                    self.app.desktop_xid(desktop),
                    id,
                    rectangle,
                );
                self.publish(SubscriberMask::NODE_GEOMETRY, &status);
                // Skip cross-monitor transfer when the requested position is
                // clearly a sentinel (negative coordinates). Apps like Steam
                // send ConfigureRequests with (-1,-1) meaning "no preference",
                // which would otherwise map to the leftmost monitor.
                let monitors = self.app.monitor_rectangles();
                if rectangle.x >= 0
                    && rectangle.y >= 0
                    && let Some(destination_monitor) =
                        monitor::monitor_from_client(&monitors, rectangle)
                    && destination_monitor != monitor
                    && let Some(destination_desktop) =
                        self.world().monitor(destination_monitor).active_desktop
                {
                    self.transfer_node(
                        (monitor, desktop, node),
                        (destination_monitor, destination_desktop),
                    )
                    .map_err(|error| {
                        RuntimeError::X11(format!("configure transfer failed: {error:?}"))
                    })?;
                    // The client already supplied destination-relative geometry.
                    self.client_mut(node).floating_rectangle = rectangle;
                    self.app.execute_pending_effects(self.x11)?;
                }
                Ok(())
            } else {
                if client.state == crate::types::ClientState::PseudoTiled {
                    let (width, height) = requested_size(event, client.floating_rectangle);
                    let (width, height) = crate::arrange::apply_size_hints(&client, width, height);
                    if (width, height)
                        != (
                            client.floating_rectangle.width,
                            client.floating_rectangle.height,
                        )
                    {
                        let value = self.client_mut(node);
                        value.floating_rectangle.width = width;
                        value.floating_rectangle.height = height;
                        self.app.arrange_desktop(self.x11, monitor, desktop)?;
                    }
                }
                let value = self.node(node);
                let Some(client) = value.client.as_ref() else {
                    return Ok(());
                };
                let rectangle = if client.state == crate::types::ClientState::Fullscreen {
                    self.world().monitor(monitor).rectangle
                } else {
                    client.tiled_rectangle
                };
                DaemonApp::execute_plan(
                    self.x11,
                    &[XAction::SyntheticConfigure {
                        window: id,
                        rectangle,
                        border_width: u16::try_from(client.border_width).unwrap_or(u16::MAX),
                    }],
                )
            }
        } else {
            Ok(ConfigureRequestPlan::forward(event).execute(self.x11)?)
        }
    }

    // Mirrors the fallible signature of every other handler, so the
    // `EventHandler` impl stays a uniform set of forwards.
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn on_configure_notify(
        &mut self,
        event: &x::ConfigureNotifyEvent,
    ) -> Result<(), RuntimeError> {
        if event.window() == self.x11.root() {
            self.x11
                .update_screen_dimensions(event.width(), event.height());
            ewmh::update_desktop_geometry(self.x11)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn on_client_message(
        &mut self,
        event: &x::ClientMessageEvent,
    ) -> Result<(), RuntimeError> {
        let message_type = event.r#type();
        if message_type == self.x11.atoms().wm_protocols
            && event.window() == self.x11.root()
            && let x::ClientMessageData::Data32(data) = event.data()
            && data[0] == self.x11.atoms().net_wm_ping.resource_id()
            && self.app.acknowledge_ewmh_ping(data[2], data[1])
        {
            return Ok(());
        }
        if (message_type == self.x11.atoms().net_startup_info_begin
            || message_type == self.x11.atoms().net_startup_info)
            && let x::ClientMessageData::Data8(payload) = event.data()
        {
            self.app.startup.ingest(
                event.window().resource_id(),
                message_type == self.x11.atoms().net_startup_info_begin,
                &payload,
            );
            return Ok(());
        }
        let message = events::decode_ewmh_client_message(event, self.x11.atoms());
        if let Some(events::EwmhClientMessage::RequestFrameExtents) = message {
            let border_width = self
                .world()
                .focused_monitor
                .and_then(|monitor| self.world().monitor(monitor).active_desktop)
                .map_or(self.app.state.settings.border_width, |desktop| {
                    self.world().desktop(desktop).border_width
                });
            ewmh::set_frame_extents(self.x11, event.window(), border_width)?;
            return Ok(());
        }
        if let Some(events::EwmhClientMessage::CurrentDesktop { desktop }) = message {
            if let Some(location) = ewmh::locate_desktop(self.world(), desktop)
                && let (Some(monitor), Some(desktop)) = (location.monitor, location.desktop)
            {
                let node = self.world().desktop(desktop).tree.focus;
                let _ = self.focus(monitor, desktop, node, false)?;
            }
            return Ok(());
        }
        let Some((monitor, desktop, node)) = self.app.managed_window(event.window().resource_id())
        else {
            self.app.postpone_client_message(event);
            return Ok(());
        };
        let Some(message) = message else {
            return Ok(());
        };
        match message {
            events::EwmhClientMessage::WmState { action, states } => {
                for state in states {
                    self.handle_wm_state(monitor, desktop, node, state, action)?;
                }
            }
            events::EwmhClientMessage::ActiveWindow {
                source, timestamp, ..
            } => {
                let focused = self
                    .world()
                    .focused_monitor
                    .and_then(|monitor| self.world().monitor(monitor).active_desktop)
                    .and_then(|desktop| self.world().desktop(desktop).tree.focus);
                let stale_application = source == 1
                    && timestamp != 0
                    && self.app.last_user_time.is_some_and(|last| {
                        timestamp != last && !super::timestamp_is_later(timestamp, last)
                    });
                if !(stale_application || self.app.state.settings.ignore_ewmh_focus && source == 1)
                    && focused != Some(node)
                {
                    let _ = self.focus(monitor, desktop, Some(node), false)?;
                }
            }
            events::EwmhClientMessage::WmDesktop { desktop: index } => {
                if let Some(destination) = ewmh::locate_desktop(self.world(), index)
                    && let (Some(destination_monitor), Some(destination_desktop)) =
                        (destination.monitor, destination.desktop)
                    && self
                        .transfer_node(
                            (monitor, desktop, node),
                            (destination_monitor, destination_desktop),
                        )
                        .is_ok()
                {
                    self.app.execute_pending_effects(self.x11)?;
                }
            }
            events::EwmhClientMessage::CloseWindow { timestamp } => {
                self.app.close_client(self.x11, node, timestamp)?;
            }
            events::EwmhClientMessage::MoveResizeWindow {
                gravity,
                flags,
                x,
                y,
                width,
                height,
                ..
            } => self.handle_moveresize_window(
                monitor, desktop, node, gravity, flags, x, y, width, height,
            )?,
            events::EwmhClientMessage::WmMoveResize {
                root_x,
                root_y,
                direction,
                button,
                ..
            } => self
                .handle_wm_moveresize(monitor, desktop, node, root_x, root_y, direction, button)?,
            events::EwmhClientMessage::RestackWindow => {
                let focused = self.world().desktop(desktop).tree.focus == Some(node);
                let actions = self.app.state.stacking_order.stack(
                    &self.app.state.world.tree,
                    node,
                    focused,
                    true, // always raise on explicit restack request
                );
                self.app.execute_restacks(self.x11, desktop, &actions)?;
                self.app.update_ewmh(self.x11)?;
            }
            events::EwmhClientMessage::CurrentDesktop { .. }
            | events::EwmhClientMessage::RequestFrameExtents => unreachable!(),
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn on_property_notify(
        &mut self,
        event: &x::PropertyNotifyEvent,
    ) -> Result<(), RuntimeError> {
        if event.atom() == self.x11.atoms().net_wm_user_time
            && let Some(owner) = self.app.user_time_owner(event.window().resource_id())
        {
            if let Ok(values) = window::get_property::<u32>(
                self.x11,
                event.window(),
                self.x11.atoms().net_wm_user_time,
                x::ATOM_CARDINAL,
            ) && let Some(timestamp) = values.first()
                && self.client_is_globally_focused(owner)
            {
                self.app.note_user_time(*timestamp);
            }
            return Ok(());
        }
        if self
            .app
            .managed_window(event.window().resource_id())
            .is_none()
            && self.app.postpone_property_notify(event)
        {
            return Ok(());
        }
        if event.atom() == self.x11.atoms().net_wm_strut_partial
            || event.atom() == self.x11.atoms().net_wm_strut
        {
            if !self.app.state.settings.ignore_ewmh_struts
                && self.app.apply_strut(self.x11, event.window())?
            {
                self.app.arrange_all(self.x11)?;
                ewmh::update_workareas(self.x11, self.world())?;
            }
            return Ok(());
        }
        let Some((monitor, desktop, node)) = self.app.managed_window(event.window().resource_id())
        else {
            return Ok(());
        };
        if event.atom() == self.x11.atoms().wm_protocols
            || event.atom() == self.x11.atoms().net_wm_sync_request_counter
        {
            let window = event.window().resource_id();
            if event.atom() == self.x11.atoms().wm_protocols {
                self.refresh_client_protocols(event.window(), node)?;
            }
            if let Some(counter) = window::sync_request_counter(self.x11, event.window()) {
                self.app.sync_request_clients.insert(window, counter);
            } else {
                self.app.sync_request_clients.remove(&window);
            }
            Ok(())
        } else if event.atom() == self.x11.atoms().net_wm_user_time {
            let values = window::get_property::<u32>(
                self.x11,
                event.window(),
                self.x11.atoms().net_wm_user_time,
                x::ATOM_CARDINAL,
            )?;
            if self.world().focused_monitor == Some(monitor)
                && self.world().monitor(monitor).active_desktop == Some(desktop)
                && self.world().desktop(desktop).tree.focus == Some(node)
                && let Some(timestamp) = values.first()
            {
                self.app.note_user_time(*timestamp);
            }
            Ok(())
        } else if event.atom() == self.x11.atoms().net_wm_user_time_window {
            self.app
                .user_time_windows
                .retain(|_, owner| *owner != event.window().resource_id());
            let values = window::get_property::<u32>(
                self.x11,
                event.window(),
                self.x11.atoms().net_wm_user_time_window,
                x::ATOM_WINDOW,
            )?;
            if let Some(auxiliary) = values.first().copied()
                && auxiliary != x::WINDOW_NONE.resource_id()
                && auxiliary != event.window().resource_id()
                && window::listen_for_property_changes(self.x11, x::Window::new(auxiliary)).is_ok()
            {
                self.app
                    .user_time_windows
                    .insert(auxiliary, event.window().resource_id());
            }
            Ok(())
        } else if event.atom() == self.x11.atoms().wm_transient_for {
            let values = window::get_property::<u32>(
                self.x11,
                event.window(),
                self.x11.atoms().wm_transient_for,
                x::ATOM_WINDOW,
            )?;
            let new_parent = values
                .first()
                .copied()
                .filter(|id| *id != x::WINDOW_NONE.resource_id())
                .filter(|id| *id != event.window().resource_id());
            let old_parent = self.client(node).transient_for;
            if new_parent != old_parent {
                self.client_mut(node).transient_for = new_parent;
                // Reconcile stacking so the child appears above its new parent.
                let focused = self.world().desktop(desktop).tree.focus == Some(node);
                let actions = self.app.state.stacking_order.stack(
                    &self.app.state.world.tree,
                    node,
                    focused,
                    self.app.state.auto_raise,
                );
                self.app.execute_restacks(self.x11, desktop, &actions)?;
                self.app.update_ewmh(self.x11)?;
            }
            Ok(())
        } else if event.atom() == x::ATOM_WM_HINTS {
            let hints = window::wm_hints(self.x11, event.window())?;
            self.set_urgent(monitor, desktop, node, hints.urgent)?;
            Ok(())
        } else if event.atom() == self.x11.atoms().wm_normal_hints {
            let hints = window::normal_hints(self.x11, event.window())?;
            self.client_mut(node).size_hints = hints;
            self.app.arrange_desktop(self.x11, monitor, desktop)
        } else if event.atom() == self.x11.atoms().net_wm_opaque_region {
            // GTK4 sets _NET_WM_OPAQUE_REGION after mapping. If borderless_csd
            // is on and the window now has CSD, remove its border.
            if self.app.state.settings.borderless_csd
                && self.client(node).border_width > 0
                && ewmh::has_csd(self.x11, event.window())
            {
                self.client_mut(node).border_width = 0;
                window::set_border_width(self.x11, event.window(), 0)?;
                self.app.arrange_desktop(self.x11, monitor, desktop)?;
            }
            Ok(())
        } else {
            Ok(())
        }
    }

    fn client_is_globally_focused(&self, window: u32) -> bool {
        let Some((monitor, desktop, node)) = self.app.managed_window(window) else {
            return false;
        };
        self.world().focused_monitor == Some(monitor)
            && self.world().monitor(monitor).active_desktop == Some(desktop)
            && self.world().desktop(desktop).tree.focus == Some(node)
    }
}

fn gravity_offset(gravity: u8, width: i32, height: i32) -> Point {
    match gravity {
        2 => Point { x: width / 2, y: 0 },
        3 => Point { x: width, y: 0 },
        4 => Point {
            x: 0,
            y: height / 2,
        },
        5 => Point {
            x: width / 2,
            y: height / 2,
        },
        6 => Point {
            x: width,
            y: height / 2,
        },
        7 => Point { x: 0, y: height },
        8 => Point {
            x: width / 2,
            y: height,
        },
        9 => Point {
            x: width,
            y: height,
        },
        _ => Point { x: 0, y: 0 },
    }
}

fn moveresize_action(direction: u32) -> Option<(PointerAction, ResizeHandle)> {
    let value = match direction {
        0 => (PointerAction::ResizeCorner, ResizeHandle::TOP_LEFT),
        1 => (PointerAction::ResizeSide, ResizeHandle::TOP),
        2 => (PointerAction::ResizeCorner, ResizeHandle::TOP_RIGHT),
        3 => (PointerAction::ResizeSide, ResizeHandle::RIGHT),
        4 => (PointerAction::ResizeCorner, ResizeHandle::BOTTOM_RIGHT),
        5 => (PointerAction::ResizeSide, ResizeHandle::BOTTOM),
        6 => (PointerAction::ResizeCorner, ResizeHandle::BOTTOM_LEFT),
        7 => (PointerAction::ResizeSide, ResizeHandle::LEFT),
        8 => (PointerAction::Move, ResizeHandle::default()),
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moveresize_directions_map_only_pointer_operations() {
        assert_eq!(
            moveresize_action(0),
            Some((PointerAction::ResizeCorner, ResizeHandle::TOP_LEFT))
        );
        assert_eq!(
            moveresize_action(5),
            Some((PointerAction::ResizeSide, ResizeHandle::BOTTOM))
        );
        assert_eq!(
            moveresize_action(8),
            Some((PointerAction::Move, ResizeHandle::default()))
        );
        assert_eq!(moveresize_action(9), None);
        assert_eq!(moveresize_action(10), None);
        assert_eq!(moveresize_action(11), None);
    }

    #[test]
    fn gravity_offsets_cover_edges_and_center() {
        assert_eq!(gravity_offset(1, 100, 80), Point { x: 0, y: 0 });
        assert_eq!(gravity_offset(5, 100, 80), Point { x: 50, y: 40 });
        assert_eq!(gravity_offset(9, 100, 80), Point { x: 100, y: 80 });
        assert_eq!(gravity_offset(10, 100, 80), Point { x: 0, y: 0 });
    }
}
