//! Draining [`CommandEffect`]s queued by the pure command layer.

use std::collections::HashSet;

use xcb::{Xid, XidNew, x};

use super::DaemonApp;
use super::action::XAction;
use crate::ewmh;
use crate::helpers::color_pixel;
use crate::monitor;
use crate::runtime::RuntimeError;
use crate::state::CommandEffect;
use crate::tree::NodeId;
use crate::types::{Layout, SubscriberMask, WmFlags};
use crate::window;
use crate::world::DesktopId;
use crate::x11::X11;

impl DaemonApp {
    /// Re-reads the lock modifier masks and reinstalls every client's passive
    /// button grabs.
    pub(super) fn regrab_client_buttons(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        let windows = self.all_client_windows();
        crate::pointer::ungrab_buttons(x11, windows.iter().copied())?;
        self.lock_masks = crate::pointer::LockMasks::query(x11)?;
        crate::pointer::grab_buttons(x11, windows, &self.state.settings, self.lock_masks)?;
        Ok(())
    }

    pub(super) fn set_subtree_visibility(
        &mut self,
        x11: &X11,
        desktop: DesktopId,
        root: NodeId,
        visible: bool,
    ) -> Result<(), RuntimeError> {
        let layout = self.world().desktop(desktop).layout;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let value = self.node(node);
            if !visible && !self.state.hide_sticky && value.sticky {
                continue;
            }
            let children = (value.first_child, value.second_child);
            let hidden = value.hidden;
            let window = value.external_id;
            let has_client = value.client.is_some();
            let feedback = value.presel.and_then(|presel| {
                (layout != Layout::Monocle)
                    .then_some(presel.feedback)
                    .flatten()
            });
            if has_client {
                if !visible || !hidden {
                    window::set_visibility(x11, x::Window::new(window), visible && !hidden)?;
                }
                if let Some(client) = self.node_mut(node).client.as_mut() {
                    client.shown = visible;
                }
            }
            if let Some(feedback) = feedback {
                self.set_feedback_visibility(x11, feedback, visible && !hidden)?;
            }
            if let Some(second) = children.1 {
                stack.push(second);
            }
            if let Some(first) = children.0 {
                stack.push(first);
            }
        }
        Ok(())
    }

    pub(super) fn sync_window_state(
        &mut self,
        x11: &X11,
        node: NodeId,
    ) -> Result<(), RuntimeError> {
        let value = self.node(node);
        let Some(client) = value.client.as_ref() else {
            return Ok(());
        };
        let represented = WmFlags::STICKY
            .union(WmFlags::HIDDEN)
            .union(WmFlags::FULLSCREEN)
            .union(WmFlags::ABOVE)
            .union(WmFlags::BELOW)
            .union(WmFlags::DEMANDS_ATTENTION);
        let mut flags = client.wm_flags.difference(represented);
        if value.sticky {
            flags = flags.union(WmFlags::STICKY);
        }
        if value.hidden {
            flags = flags.union(WmFlags::HIDDEN);
        }
        if client.urgent {
            flags = flags.union(WmFlags::DEMANDS_ATTENTION);
        }
        if client.state == crate::types::ClientState::Fullscreen {
            flags = flags.union(WmFlags::FULLSCREEN);
        }
        match client.layer {
            crate::types::StackLayer::Below => flags = flags.union(WmFlags::BELOW),
            crate::types::StackLayer::Above => flags = flags.union(WmFlags::ABOVE),
            crate::types::StackLayer::Normal => {}
        }
        // _NET_WM_STATE_FOCUSED: set only on the globally focused window.
        let is_focused = self
            .world()
            .focused_monitor
            .and_then(|m| self.world().monitor(m).active_desktop)
            .and_then(|d| self.world().desktop(d).tree.focus)
            == Some(node);
        if is_focused {
            flags = flags.union(WmFlags::FOCUSED);
        }
        let window = value.external_id;
        self.client_mut(node).wm_flags = flags;
        let states = ewmh::wm_state_atoms(flags, x11.atoms());
        ewmh::set_wm_state(x11, x::Window::new(window), &states)?;
        if self.state.settings.enable_ewmh_allowed_actions {
            let actions =
                ewmh::allowed_action_atoms(self.world(), node, &self.state.settings, x11.atoms());
            ewmh::set_allowed_actions(x11, x::Window::new(window), &actions)?;
        }
        Ok(())
    }

    pub(super) fn refresh_ewmh_allowed_actions(&self, x11: &X11) -> Result<(), RuntimeError> {
        let enabled = self.state.settings.enable_ewmh_allowed_actions;
        ewmh::set_supported(x11, enabled)?;
        for node in self.all_client_nodes() {
            let window = x::Window::new(self.xid(node));
            if enabled {
                let actions = ewmh::allowed_action_atoms(
                    self.world(),
                    node,
                    &self.state.settings,
                    x11.atoms(),
                );
                ewmh::set_allowed_actions(x11, window, &actions)?;
            } else {
                window::delete_property(x11, window, x11.atoms().net_wm_allowed_actions)?;
            }
        }
        Ok(())
    }

    pub(super) fn refresh_colors(&self, x11: &X11) -> Result<(), RuntimeError> {
        let feedback_color = color_pixel(&self.state.settings.presel_feedback_color);
        for feedback in self.tree().feedback_windows() {
            window::set_background_color(x11, x::Window::new(feedback), feedback_color)?;
            if self.mapped_feedbacks.contains(&feedback) {
                window::set_visibility(x11, x::Window::new(feedback), false)?;
                window::set_visibility(x11, x::Window::new(feedback), true)?;
            }
        }
        for node in self.all_client_nodes() {
            let desktop = self.world().node_desktop(node);
            let active = desktop.is_some_and(|desktop| {
                self.world()
                    .desktop_monitor(desktop)
                    .is_some_and(|monitor| {
                        self.world().monitor(monitor).active_desktop == Some(desktop)
                    })
            });
            let focused = desktop.is_some_and(|desktop| {
                self.world().focused_monitor == self.world().desktop_monitor(desktop)
                    && self.world().desktop(desktop).tree.focus == Some(node)
            });
            let color = if focused {
                &self.state.settings.focused_border_color
            } else if active {
                &self.state.settings.active_border_color
            } else {
                &self.state.settings.normal_border_color
            };
            let pixel = color_pixel(color);
            window::set_border_color(x11, x::Window::new(self.xid(node)), pixel)?;
        }
        Ok(())
    }

    fn adopt_orphans(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        let reply = x11.request(&x::QueryTree { window: x11.root() })?;
        for child in reply.children() {
            if window::wm_desktop(x11, *child)?.is_some() {
                let _ = self.schedule_window(x11, child.resource_id())?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub fn execute_pending_effects(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        self.destroy_retired_feedbacks(x11)?;
        self.state.forget_retired_nodes();
        let effects: Vec<_> = std::mem::take(&mut self.state.pending_effects)
            .into_iter()
            .filter(|effect| effect.references_live(&self.state))
            .collect();
        let mut arranged = HashSet::new();
        let mut synced_ewmh = false;
        for effect in effects {
            match effect {
                CommandEffect::Arrange { monitor, desktop } => {
                    if arranged.insert((monitor, desktop))
                        && self.world().desktop_monitor(desktop) == Some(monitor)
                    {
                        self.arrange_desktop(x11, monitor, desktop)?;
                    }
                }
                CommandEffect::Restack { node, auto_raise } => {
                    let focused = self.world().node_desktop(node).is_some_and(|desktop| {
                        self.world().desktop(desktop).tree.focus == Some(node)
                    });
                    let actions = self.state.stacking_order.stack(
                        &self.state.world.tree,
                        node,
                        focused,
                        auto_raise,
                    );
                    if let Some(desktop) = self.world().node_desktop(node) {
                        self.execute_restacks(x11, desktop, &actions)?;
                    }
                }
                CommandEffect::SyncEwmh => synced_ewmh = true,
                CommandEffect::SetWindowVisibility { node, visible } => {
                    if let Some(desktop) = self.world().node_desktop(node) {
                        self.set_subtree_visibility(x11, desktop, node, visible)?;
                        for client in self.client_nodes(node) {
                            self.sync_window_state(x11, client)?;
                        }
                    }
                }
                CommandEffect::SetDesktopVisibility {
                    desktop,
                    visible,
                    preserve_sticky,
                } => {
                    if let Some(root) = self.world().desktop(desktop).tree.root {
                        let hide_sticky = self.state.hide_sticky;
                        if preserve_sticky {
                            self.state.hide_sticky = false;
                        }
                        let result = self.set_subtree_visibility(x11, desktop, root, visible);
                        self.state.hide_sticky = hide_sticky;
                        result?;
                    }
                }
                CommandEffect::CreateMonitorRoot { monitor: id } => {
                    let value = self.world().monitor(id);
                    let root = monitor::create_monitor_root(
                        x11,
                        &value.name,
                        value.rectangle,
                        self.state.settings.focus_follows_pointer,
                    )?;
                    self.world_mut().monitor_mut(id).root_id = Some(root);
                }
                CommandEffect::UpdateMonitorRoot { monitor: id } => {
                    let value = self.world().monitor(id);
                    if let Some(root) = value.root_id {
                        monitor::update_monitor_root(x11, root, &value.name, value.rectangle)?;
                    }
                }
                CommandEffect::DestroyMonitorRoot { root } => {
                    monitor::destroy_monitor_root(x11, root)?;
                }
                CommandEffect::RegrabButtons => self.regrab_client_buttons(x11)?,
                CommandEffect::RefreshColors | CommandEffect::RefreshBorders => {
                    self.refresh_colors(x11)?;
                }
                CommandEffect::RefreshMonitors => self.reconcile_randr_monitors(x11)?,
                CommandEffect::RefreshFocusFollowsPointer => {
                    let ffp = self.state.settings.focus_follows_pointer;
                    for node in self.all_client_nodes() {
                        let id = self.xid(node);
                        Self::execute_action(
                            x11,
                            XAction::SetClientEventMask {
                                window: id,
                                enter_window: ffp,
                            },
                        )?;
                    }
                    // Map or unmap monitor root windows so cross-monitor
                    // EnterNotify events are delivered when FFP is on.
                    for monitor in self.world().monitor_order().to_vec() {
                        if let Some(root) = self.world().monitor(monitor).root_id {
                            let window = x::Window::new(root);
                            if ffp {
                                x11.send_and_check_request(&x::MapWindow { window })?;
                            } else {
                                x11.send_and_check_request(&x::UnmapWindow { window })?;
                            }
                        }
                    }
                }
                CommandEffect::RefreshEwmhAllowedActions => {
                    self.refresh_ewmh_allowed_actions(x11)?;
                }
                CommandEffect::Focus {
                    monitor,
                    previous_monitor,
                    desktop,
                    previous_desktop,
                    node,
                    previous_node,
                    activate,
                    auto_raise,
                } => {
                    if previous_desktop != Some(desktop) {
                        if let Some(root) = self.world().desktop(desktop).tree.root {
                            self.set_subtree_visibility(x11, desktop, root, true)?;
                        }
                        // The previous desktop may have been removed between
                        // queueing this focus and running it.
                        if let Some(previous) = previous_desktop
                            && let Some(root) =
                                self.world().get_desktop(previous).and_then(|d| d.tree.root)
                        {
                            self.set_subtree_visibility(x11, previous, root, false)?;
                        }
                    }
                    if !activate && let Some(node) = self.apply_focus(x11, node)? {
                        self.sync_window_state(x11, node)?;
                    }
                    // Sync the previously focused node to remove _NET_WM_STATE_FOCUSED.
                    if let Some(old) = previous_node.filter(|old| Some(*old) != node)
                        && self.world().tree.is_live(old)
                    {
                        self.sync_window_state(x11, old)?;
                    }
                    // Restack the previously focused node with focused=false
                    // so it drops back to its unfocused position. Without this,
                    // a tiled node raised during focus stays above floating
                    // windows after focus moves to a floating window.
                    if let Some(old) = previous_node.filter(|old| Some(*old) != node)
                        && self.world().tree.is_live(old)
                        && let Some(old_desktop) = self.world().node_desktop(old)
                    {
                        let old_actions = self.state.stacking_order.stack(
                            &self.state.world.tree,
                            old,
                            false,
                            auto_raise,
                        );
                        self.execute_restacks(x11, old_desktop, &old_actions)?;
                    }
                    if let Some(node) = node {
                        let actions = self.state.stacking_order.stack(
                            &self.state.world.tree,
                            node,
                            true,
                            auto_raise,
                        );
                        self.execute_restacks(x11, desktop, &actions)?;
                    }
                    let (desktop_mask, node_mask, verb) = if activate {
                        (
                            SubscriberMask::DESKTOP_ACTIVATE,
                            SubscriberMask::NODE_ACTIVATE,
                            "activate",
                        )
                    } else {
                        (
                            SubscriberMask::DESKTOP_FOCUS,
                            SubscriberMask::NODE_FOCUS,
                            "focus",
                        )
                    };
                    if !activate && previous_monitor != Some(monitor) {
                        let status = format!("monitor_focus 0x{:08X}\n", self.monitor_xid(monitor));
                        self.publish(SubscriberMask::MONITOR_FOCUS, &status);
                    }
                    if activate
                        || previous_monitor != Some(monitor)
                        || previous_desktop != Some(desktop)
                    {
                        let ids = self.desktop_ids(monitor, desktop);
                        self.publish(desktop_mask, &format!("desktop_{verb} {ids}\n"));
                    }
                    if let Some(node) = node {
                        let ids = self.node_ids(monitor, desktop, node);
                        self.publish(node_mask, &format!("node_{verb} {ids}\n"));
                    }
                    self.broadcast_report();
                    synced_ewmh = true;
                }
                CommandEffect::MoveResize { node, rectangle } => {
                    window::move_resize(x11, x::Window::new(self.xid(node)), rectangle)?;
                }
                CommandEffect::WarpPointer { rectangle } => {
                    if self.pointer_grab.is_none() {
                        window::warp_pointer_to_center(x11, rectangle)?;
                    }
                }
                CommandEffect::Broadcast {
                    mask,
                    status,
                    report,
                } => {
                    self.broadcast_status(mask, status.as_bytes());
                    if report {
                        self.broadcast_report();
                    }
                }
                CommandEffect::AdoptOrphans => self.adopt_orphans(x11)?,
                CommandEffect::Close { node } => {
                    for client in self.client_nodes(node) {
                        self.close_client(
                            x11,
                            client,
                            self.last_user_time.unwrap_or(x::CURRENT_TIME),
                        )?;
                    }
                }
                CommandEffect::Kill { node } => {
                    for client in self.client_nodes(node) {
                        x11.send_and_check_request(&x::KillClient {
                            resource: self.xid(client),
                        })?;
                    }
                }
                CommandEffect::SyncWindowState { node } => self.sync_window_state(x11, node)?,
                CommandEffect::SyncPreselFeedback {
                    node,
                    include_receptacle,
                } => {
                    if let Some(desktop) = self.world().node_desktop(node)
                        && let Some(monitor) = self.world().desktop_monitor(desktop)
                        && (include_receptacle || self.node(node).client.is_some())
                    {
                        self.sync_presel_feedbacks(x11, monitor, desktop)?;
                    }
                }
                CommandEffect::LoadState { restored } => {
                    self.reconstruct_state(*restored, x11, false)?;
                    synced_ewmh = false;
                }
            }
        }
        if synced_ewmh {
            self.update_ewmh(x11)?;
        }
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.state.validate(),
            Ok(()),
            "daemon state broke an invariant while draining pending effects"
        );
        Ok(())
    }
}
