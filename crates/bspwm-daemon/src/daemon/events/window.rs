//! Handlers for the events that describe a window's life and geometry.

use xcb::{Xid, x};

use super::XEventContext;
use crate::daemon::DaemonApp;
use crate::daemon::action::{XAction, set_wm_state_property};
use crate::daemon::status::node_geometry_status;
use crate::events::{self, ConfigureRequestPlan};
use crate::ewmh;
use crate::monitor;
use crate::runtime::RuntimeError;
use crate::types::{Rectangle, SubscriberMask};
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
            let client = self.node(node).client.clone().unwrap();
            let mask = event.value_mask();
            if client.state == crate::types::ClientState::Floating {
                let mut rectangle = client.floating_rectangle;
                if mask.contains(x::ConfigWindowMask::X) {
                    rectangle.x = i32::from(event.x())
                        .wrapping_sub(i32::try_from(client.border_width).unwrap_or(i32::MAX));
                }
                if mask.contains(x::ConfigWindowMask::Y) {
                    rectangle.y = i32::from(event.y())
                        .wrapping_sub(i32::try_from(client.border_width).unwrap_or(i32::MAX));
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
                let monitors = self.app.monitor_rectangles();
                if let Some(destination_monitor) =
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
                let client = value.client.as_ref().unwrap();
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
        }
        Ok(())
    }

    pub(super) fn on_client_message(
        &mut self,
        event: &x::ClientMessageEvent,
    ) -> Result<(), RuntimeError> {
        let message = events::decode_ewmh_client_message(event, self.x11.atoms());
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
            events::EwmhClientMessage::ActiveWindow { source } => {
                let focused = self
                    .world()
                    .focused_monitor
                    .and_then(|monitor| self.world().monitor(monitor).active_desktop)
                    .and_then(|desktop| self.world().desktop(desktop).tree.focus);
                if !(self.app.state.settings.ignore_ewmh_focus && source == 1)
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
            events::EwmhClientMessage::CloseWindow => {
                let delete_window = self.client(node).icccm.delete_window;
                window::close_cached(self.x11, event.window(), delete_window)?;
            }
            events::EwmhClientMessage::CurrentDesktop { .. } => unreachable!(),
        }
        Ok(())
    }

    pub(super) fn on_property_notify(
        &mut self,
        event: &x::PropertyNotifyEvent,
    ) -> Result<(), RuntimeError> {
        if self
            .app
            .managed_window(event.window().resource_id())
            .is_none()
            && self.app.postpone_property_notify(event)
        {
            return Ok(());
        }
        if event.atom() == self.x11.atoms().net_wm_strut_partial {
            if !self.app.state.settings.ignore_ewmh_struts
                && self.app.apply_strut(self.x11, event.window())?
            {
                self.app.arrange_all(self.x11)?;
            }
            return Ok(());
        }
        let Some((monitor, desktop, node)) = self.app.managed_window(event.window().resource_id())
        else {
            return Ok(());
        };
        if event.atom() == x::ATOM_WM_HINTS {
            let urgent = window::wm_hints(self.x11, event.window())?;
            if urgent.urgent {
                self.set_urgent(monitor, desktop, node, true)?;
            }
            Ok(())
        } else if event.atom() == self.x11.atoms().wm_normal_hints {
            let hints = window::normal_hints(self.x11, event.window())?;
            self.client_mut(node).size_hints = hints;
            self.app.arrange_desktop(self.x11, monitor, desktop)
        } else {
            Ok(())
        }
    }
}
