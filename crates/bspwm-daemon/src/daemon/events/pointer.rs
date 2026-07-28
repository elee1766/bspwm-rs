//! Handlers for the pointer- and focus-driven events.

use xcb::{Xid, XidNew, x};

use super::XEventContext;
use crate::daemon::{PointerGrab, PointerGrabOrigin};
use crate::events::MotionDisposition;
use crate::monitor;
use crate::runtime::RuntimeError;
use crate::state::FocusPolicy;
use crate::types::{ClientState, Point, PointerAction, Rectangle, ResizeHandle};
use crate::window;

impl XEventContext<'_> {
    fn resize_grabbed_floating_client(
        &mut self,
        grab: PointerGrab,
        client: &crate::tree::Client,
        position: Point,
    ) -> Rectangle {
        let dx = position.x - grab.last_position.x;
        let dy = position.y - grab.last_position.y;
        let input = if client.honor_size_hints.should_honor(client.state) {
            crate::pointer::ResizeInput::Absolute(position)
        } else {
            crate::pointer::ResizeInput::Relative { dx, dy }
        };
        let mut rectangle =
            crate::pointer::plan_floating_resize(client.floating_rectangle, grab.handle, input);
        let (width, height) =
            crate::arrange::apply_size_hints(client, rectangle.width, rectangle.height);
        if grab.handle.contains(ResizeHandle::LEFT) {
            rectangle.x += rectangle.width - width;
        }
        if grab.handle.contains(ResizeHandle::TOP) {
            rectangle.y += rectangle.height - height;
        }
        rectangle.width = width;
        rectangle.height = height;
        self.client_mut(grab.node).floating_rectangle = rectangle;
        rectangle
    }

    pub(super) fn on_enter_notify(
        &mut self,
        event: &x::EnterNotifyEvent,
    ) -> Result<(), RuntimeError> {
        if !self.app.state.settings.focus_follows_pointer
            || event.mode() != x::NotifyMode::Normal
            || event.detail() == x::NotifyDetail::Inferior
            || self.app.pointer_filter.is_generated_enter(
                self.app
                    .motion_recorder
                    .is_some_and(|recorder| recorder.enabled),
                event.sequence(),
            )
        {
            return Ok(());
        }
        let window = event.event().resource_id();
        let focused = self
            .world()
            .focused_monitor
            .and_then(|monitor| self.world().monitor(monitor).active_desktop)
            .and_then(|desktop| self.world().desktop(desktop).tree.focus);
        let should_record = self
            .app
            .managed_window(window)
            .is_some_and(|(_, _, node)| Some(node) != focused)
            || self.world().monitor_order().iter().any(|monitor| {
                self.world().monitor(*monitor).root_id == Some(window)
                    && self.world().focused_monitor != Some(*monitor)
            });
        let Some(recorder) = self.app.motion_recorder.as_mut() else {
            return Ok(());
        };
        if !should_record {
            if recorder.enabled {
                self.x11.send_and_check_request(&x::UnmapWindow {
                    window: x::Window::new(recorder.window),
                })?;
                recorder.enabled = false;
            }
            return Ok(());
        }
        let geometry = window::geometry(self.x11, event.event())?;
        let border = i32::from(geometry.border_width) * 2;
        let rectangle = Rectangle::new(
            geometry.rectangle.x,
            geometry.rectangle.y,
            geometry.rectangle.width.saturating_add(border),
            geometry.rectangle.height.saturating_add(border),
        );
        let recorder_window = x::Window::new(recorder.window);
        window::move_resize(self.x11, recorder_window, rectangle)?;
        window::stack_above(self.x11, recorder_window, event.event())?;
        self.x11.send_and_check_request(&x::MapWindow {
            window: recorder_window,
        })?;
        recorder.enabled = true;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn on_motion_notify(
        &mut self,
        event: &x::MotionNotifyEvent,
    ) -> Result<(), RuntimeError> {
        if let Some(mut grab) = self.app.pointer_grab {
            if event.time().wrapping_sub(grab.last_motion_time)
                < self.app.state.settings.pointer_motion_interval
            {
                return Ok(());
            }
            // The drag's node, desktop, or monitor may have been removed under
            // it; the release still has to ungrab, so keep the grab and only
            // skip the work that would resolve it.
            if !self.app.pointer_grab_is_live() {
                return Ok(());
            }
            let position = Point::from_x11(event.root_x(), event.root_y());
            let previous_x = i16::try_from(grab.last_position.x)
                .expect("pointer grab positions originate from X i16 coordinates");
            let previous_y = i16::try_from(grab.last_position.y)
                .expect("pointer grab positions originate from X i16 coordinates");
            let dx = i32::from(event.root_x().wrapping_sub(previous_x));
            let dy = i32::from(event.root_y().wrapping_sub(previous_y));
            let client = self.node(grab.node).client.clone();
            let Some(client) = client else {
                self.app.pointer_grab = None;
                return Ok(());
            };
            if matches!(grab.origin, PointerGrabOrigin::Ewmh { .. })
                && client.state != ClientState::Floating
            {
                return self.finish_pointer_grab();
            }
            match grab.action {
                PointerAction::Move if client.state.is_tiled() => {
                    let (pointer_window, point) = self.query_pointer()?;
                    if pointer_window.resource_id() != self.xid(grab.node) {
                        if let Some((target_monitor, _, target)) =
                            self.app.managed_window(pointer_window.resource_id())
                        {
                            let target_tiled = self
                                .client_of(target)
                                .is_some_and(|client| client.state.is_tiled());
                            if target_monitor == grab.monitor && target_tiled {
                                let source_id = self.xid(grab.node);
                                let target_id = self.xid(target);
                                self.world_mut().swap_nodes(grab.node, target).map_err(
                                    |error| {
                                        RuntimeError::X11(format!("pointer swap failed: {error:?}"))
                                    },
                                )?;
                                let status = format!(
                                    "node_swap {} {}\n",
                                    self.app.node_ids_raw(grab.monitor, grab.desktop, source_id),
                                    self.app.node_ids_raw(grab.monitor, grab.desktop, target_id),
                                );
                                self.publish(crate::types::SubscriberMask::NODE_SWAP, &status);
                                self.app.arrange_desktop_quiet(
                                    self.x11,
                                    grab.monitor,
                                    grab.desktop,
                                )?;
                            } else if target_monitor != grab.monitor {
                                let _ = self.transfer_grabbed_node(&mut grab, target_monitor)?;
                            }
                        } else if let Some(destination) = self.monitor_at(point) {
                            let _ = self.transfer_grabbed_node(&mut grab, destination)?;
                        }
                    }
                }
                PointerAction::Move => {
                    let rectangle =
                        crate::pointer::plan_floating_move(client.floating_rectangle, dx, dy);
                    self.client_mut(grab.node).floating_rectangle = rectangle;
                    window::move_resize(self.x11, x::Window::new(self.xid(grab.node)), rectangle)?;
                    let monitors = self.app.monitor_rectangles();
                    if let Some(destination) = monitor::monitor_from_client(&monitors, rectangle) {
                        let _ = self.transfer_grabbed_node(&mut grab, destination)?;
                    }
                }
                PointerAction::ResizeSide | PointerAction::ResizeCorner => {
                    let input = if client.honor_size_hints.should_honor(client.state) {
                        crate::pointer::ResizeInput::Absolute(position)
                    } else {
                        crate::pointer::ResizeInput::Relative { dx, dy }
                    };
                    if client.state == ClientState::Tiled {
                        let plan = crate::pointer::plan_tiled_resize(
                            self.tree(),
                            grab.node,
                            grab.handle,
                            input,
                        );
                        crate::pointer::apply_tiled_resize_plan(self.tree_mut(), plan);
                        if let Some(update) = plan.vertical {
                            let rectangle = self.node(update.node).rectangle;
                            self.tree_mut().adjust_ratios(update.node, rectangle);
                        }
                        if let Some(update) = plan.horizontal {
                            let rectangle = self.node(update.node).rectangle;
                            self.tree_mut().adjust_ratios(update.node, rectangle);
                        }
                        self.app
                            .arrange_desktop_quiet(self.x11, grab.monitor, grab.desktop)?;
                    } else {
                        let rectangle =
                            self.resize_grabbed_floating_client(grab, &client, position);
                        if client.state == ClientState::Floating {
                            if grab.sync_resize.is_some() {
                                self.send_sync_resize(&mut grab, rectangle, event.time());
                            } else {
                                window::move_resize(
                                    self.x11,
                                    x::Window::new(self.xid(grab.node)),
                                    rectangle,
                                )?;
                            }
                        } else {
                            self.app
                                .arrange_desktop_quiet(self.x11, grab.monitor, grab.desktop)?;
                        }
                    }
                }
                PointerAction::None | PointerAction::Focus => {}
            }
            grab.last_position = position;
            grab.last_motion_time = event.time();
            self.app.pointer_grab = Some(grab);
            return Ok(());
        }

        if !self.app.state.settings.focus_follows_pointer
            || self.app.pointer_filter.classify_motion(event) != MotionDisposition::Dispatch
        {
            return Ok(());
        }
        if let Some(recorder) = self.app.motion_recorder.as_mut()
            && recorder.enabled
        {
            self.x11.send_and_check_request(&x::UnmapWindow {
                window: x::Window::new(recorder.window),
            })?;
            recorder.enabled = false;
        }
        let (window, point) = self.query_pointer()?;
        let policy = FocusPolicy::suppressed(&self.app.state, true, true);
        if let Some((monitor, desktop, node)) = self.app.managed_window(window.resource_id()) {
            if self.world().monitor(monitor).active_desktop == Some(desktop)
                && self.world().desktop(desktop).tree.focus != Some(node)
            {
                self.focus_with(monitor, desktop, Some(node), false, policy)?;
            }
        } else if let Some(monitor) = self.monitor_at(point)
            && self.world().focused_monitor != Some(monitor)
            && let Some(desktop) = self.world().monitor(monitor).active_desktop
        {
            let node = self.world().desktop(desktop).tree.focus;
            self.focus_with(monitor, desktop, node, false, policy)?;
        }
        Ok(())
    }

    pub(super) fn on_button_press(
        &mut self,
        event: &x::ButtonPressEvent,
    ) -> Result<(), RuntimeError> {
        self.app.note_user_time(event.time());
        let action = crate::pointer::resolve_button_action(
            event.detail(),
            u16::try_from(event.state().bits() & u32::from(u16::MAX))
                .expect("masked modifier state fits u16"),
            self.app.state.settings.click_to_focus,
            self.app.state.settings.pointer_actions,
            self.app.lock_masks,
        );
        let mut replay = false;
        if let Some(action) = action {
            let (window, position) = self.query_pointer()?;
            match action {
                crate::pointer::ButtonAction::Focus => {
                    let policy = FocusPolicy::suppressed(&self.app.state, true, false);
                    let changed = self.focus_clicked_window(window, position, policy)?;
                    replay = crate::pointer::replay_focus_click(
                        changed,
                        self.app.state.settings.swallow_first_click,
                    );
                }
                crate::pointer::ButtonAction::Pointer(action) => {
                    if let Some((monitor, desktop, node)) =
                        self.app.managed_window(window.resource_id())
                    {
                        let client = self.client(node);
                        if client.state != ClientState::Fullscreen
                            && crate::pointer::grab_pointer(self.x11)? == x::GrabStatus::Success
                        {
                            let rectangle = if client.state == ClientState::Floating {
                                client.floating_rectangle
                            } else {
                                client.tiled_rectangle
                            };
                            let grab = PointerGrab {
                                monitor,
                                desktop,
                                node,
                                action,
                                handle: crate::pointer::resize_handle(rectangle, position, action),
                                last_position: position,
                                last_motion_time: 0,
                                origin: PointerGrabOrigin::Binding,
                                sync_resize: (client.state == ClientState::Floating
                                    && matches!(
                                        action,
                                        PointerAction::ResizeSide | PointerAction::ResizeCorner
                                    ))
                                .then(|| self.begin_sync_resize(node))
                                .flatten(),
                            };
                            self.app.pointer_grab = Some(grab);
                            self.pointer_status(grab, "begin");
                        }
                    }
                }
            }
        }
        self.x11.send_and_check_request(&x::AllowEvents {
            mode: if replay {
                x::Allow::ReplayPointer
            } else {
                x::Allow::SyncPointer
            },
            time: event.time(),
        })?;
        Ok(())
    }

    pub(super) fn on_button_release(
        &mut self,
        event: &x::ButtonReleaseEvent,
    ) -> Result<(), RuntimeError> {
        let Some(mut grab) = self.app.pointer_grab else {
            return Ok(());
        };
        if let PointerGrabOrigin::Ewmh { button } = grab.origin
            && button != 0
            && event.detail() != button
        {
            return Ok(());
        }
        if grab.sync_resize.is_some()
            && self.app.pointer_grab_is_live()
            && matches!(
                grab.action,
                PointerAction::ResizeSide | PointerAction::ResizeCorner
            )
        {
            let position = Point::from_x11(event.root_x(), event.root_y());
            if position != grab.last_position {
                let client = self.client(grab.node).clone();
                let rectangle = self.resize_grabbed_floating_client(grab, &client, position);
                self.send_sync_resize(&mut grab, rectangle, event.time());
                grab.last_position = position;
                self.app.pointer_grab = Some(grab);
            }
        }
        self.finish_pointer_grab()
    }

    pub(super) fn on_focus_in(&mut self, event: &x::FocusInEvent) -> Result<(), RuntimeError> {
        if matches!(event.mode(), x::NotifyMode::Grab | x::NotifyMode::Ungrab)
            || matches!(
                event.detail(),
                x::NotifyDetail::Pointer | x::NotifyDetail::PointerRoot | x::NotifyDetail::None
            )
        {
            return Ok(());
        }
        let Some(monitor) = self.world().focused_monitor else {
            return Ok(());
        };
        let Some(desktop) = self.world().monitor(monitor).active_desktop else {
            return Ok(());
        };
        let focus = self.world().desktop(desktop).tree.focus;
        let focused_window = focus
            .filter(|node| self.node(*node).client.is_some())
            .map(|node| self.xid(node));
        if focused_window == Some(event.event().resource_id()) {
            return Ok(());
        }
        if event.event() == self.x11.root() {
            let policy = FocusPolicy::suppressed(&self.app.state, false, false);
            self.focus_with(monitor, desktop, focus, false, policy)?;
        } else if self
            .app
            .managed_window(event.event().resource_id())
            .is_some()
        {
            self.app.apply_focus(self.x11, focus)?;
        }
        Ok(())
    }
}
