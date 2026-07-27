use super::{CommandHandler, node_state_status};
use crate::monitor;
use crate::query::Coordinates;
use crate::state::{CommandEffect, DaemonState, FocusPolicy};
use crate::types::{ClientState, Layout, ResizeHandle};

impl CommandHandler<'_> {
    pub(super) fn arrange_effect(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
    ) {
        self.state
            .pending_effects
            .push(CommandEffect::Arrange { monitor, desktop });
    }

    /// Arranges `target`'s desktop, if it names both a monitor and a desktop.
    pub(super) fn arrange_target(&mut self, target: Coordinates) {
        if let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop) {
            self.arrange_effect(monitor, desktop);
        }
    }

    pub(super) fn structural_effects(
        &mut self,
        locations: &[(crate::world::MonitorId, crate::world::DesktopId)],
    ) {
        for &(monitor, desktop) in locations {
            self.arrange_effect(monitor, desktop);
        }
        self.state.pending_effects.push(CommandEffect::SyncEwmh);
        self.state
            .pending_effects
            .push(CommandEffect::RefreshBorders);
    }

    pub(super) fn broadcast(&mut self, mask: crate::types::SubscriberMask, status: String) {
        self.state.pending_effects.push(CommandEffect::Broadcast {
            mask,
            status,
            report: false,
        });
    }

    pub(super) fn report_effect(&mut self) {
        self.state.pending_effects.push(CommandEffect::Broadcast {
            mask: crate::types::SubscriberMask::REPORT,
            status: String::new(),
            report: false,
        });
    }

    pub(crate) fn set_node_state(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
        node: crate::tree::NodeId,
        state: ClientState,
    ) -> bool {
        let presels = self.presel_snapshot(desktop);
        let changed =
            self.state
                .world
                .set_state(desktop, node, state, self.state.settings.single_monocle);
        if changed {
            self.broadcast_cancelled_presels(monitor, desktop, presels);
        }
        changed
    }

    pub(super) fn presel_snapshot(
        &self,
        desktop: crate::world::DesktopId,
    ) -> Vec<(crate::tree::NodeId, u32)> {
        let Some(root) = self.state.world.desktop(desktop).tree.root else {
            return Vec::new();
        };
        self.state
            .world
            .tree
            .preorder(root)
            .filter_map(|node| {
                let value = self.state.world.tree.node(node);
                value.presel.as_ref().map(|_| (node, value.external_id))
            })
            .collect()
    }

    pub(super) fn broadcast_cancelled_presels(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
        before: Vec<(crate::tree::NodeId, u32)>,
    ) {
        for (node, external_id) in before {
            if self
                .state
                .world
                .tree
                .get(node)
                .is_some_and(|value| value.presel.is_none())
            {
                self.broadcast(
                    crate::types::SubscriberMask::NODE_PRESEL,
                    format!(
                        "node_presel 0x{:08X} 0x{:08X} 0x{external_id:08X} cancel\n",
                        self.state.world.monitor(monitor).external_id,
                        self.state.world.desktop(desktop).external_id,
                    ),
                );
            }
        }
    }

    pub(super) fn layout_effect(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
        previous: Layout,
    ) {
        queue_layout_effect(self.state, monitor, desktop, previous);
    }

    pub(super) fn adapt_subtree_geometry(
        &mut self,
        root: crate::tree::NodeId,
        source: crate::types::Rectangle,
        destination: crate::types::Rectangle,
    ) {
        self.state
            .world
            .tree
            .adapt_client_geometry(root, source, destination);
    }

    pub(super) fn sticky_roots(&self, root: crate::tree::NodeId) -> Vec<crate::tree::NodeId> {
        self.state.world.tree.sticky_roots(root)
    }

    fn transfer_sticky_nodes(
        &mut self,
        monitor: crate::world::MonitorId,
        source: crate::world::DesktopId,
        destination: crate::world::DesktopId,
        policy: FocusPolicy,
    ) {
        let Some(root) = self.state.world.desktop(source).tree.root else {
            return;
        };
        let nodes = self.sticky_roots(root);
        let sticky_still = self.state.sticky_still;
        self.state.sticky_still = false;
        for node in nodes {
            let anchor = self.state.world.desktop(destination).tree.focus;
            let _ = self.transfer_node_complete_with(
                Coordinates::node(monitor, source, node),
                Coordinates::in_desktop(monitor, destination, anchor),
                false,
                policy,
            );
        }
        self.state.sticky_still = sticky_still;
    }

    pub(super) fn relocate_swapped_stickies(
        &mut self,
        nodes: &[crate::tree::NodeId],
        source_monitor: crate::world::MonitorId,
        source_desktop: crate::world::DesktopId,
        destination_monitor: crate::world::MonitorId,
        destination_desktop: crate::world::DesktopId,
    ) {
        let source_rectangle = self.state.world.monitor(source_monitor).rectangle;
        let destination_rectangle = self.state.world.monitor(destination_monitor).rectangle;
        for &node in nodes {
            let anchor = self.state.world.desktop(destination_desktop).tree.focus;
            self.state
                .history
                .remove_node(&self.state.world.tree, node, true);
            if self
                .state
                .world
                .transfer_node(
                    node,
                    destination_desktop,
                    anchor,
                    self.state.settings.split_ratio,
                )
                .is_err()
            {
                continue;
            }
            self.adapt_subtree_geometry(node, source_rectangle, destination_rectangle);
            let _ = self.state.stacking_order.stack(
                &self.state.world.tree,
                node,
                false,
                self.state.auto_raise,
            );
            self.broadcast(
                crate::types::SubscriberMask::NODE_TRANSFER,
                format!(
                    "node_transfer 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X}\n",
                    self.state.world.monitor(source_monitor).external_id,
                    self.state.world.desktop(source_desktop).external_id,
                    self.state.world.tree.node(node).external_id,
                    self.state.world.monitor(destination_monitor).external_id,
                    self.state.world.desktop(destination_desktop).external_id,
                    anchor.map_or(0, |anchor| self.state.world.tree.node(anchor).external_id),
                ),
            );
            self.state.pending_effects.push(CommandEffect::Restack {
                node,
                auto_raise: self.state.auto_raise,
            });
        }
    }

    pub(super) fn neutralize_occluding_windows(
        &mut self,
        monitor: crate::world::MonitorId,
        desktop: crate::world::DesktopId,
        target: crate::tree::NodeId,
        auto_raise: bool,
    ) {
        let Some(root) = self.state.world.desktop(desktop).tree.root else {
            return;
        };
        let target_layer = self
            .state
            .world
            .tree
            .node(target)
            .client
            .as_ref()
            .map(|client| client.layer);
        let mut changed = false;
        // `set_node_state` never relinks the tree, so the leaf chain is stable.
        let leaves: Vec<_> = self.state.world.tree.leaves(root).collect();
        for node in leaves {
            if node == target {
                continue;
            }
            let replacement = self
                .state
                .world
                .tree
                .node(node)
                .client
                .as_ref()
                .and_then(|client| {
                    let rank = |layer| match layer {
                        crate::types::StackLayer::Below => 0,
                        crate::types::StackLayer::Normal => 1,
                        crate::types::StackLayer::Above => 2,
                    };
                    (client.state == ClientState::Fullscreen
                        && target_layer.is_some_and(|layer| rank(layer) <= rank(client.layer)))
                    .then_some(client.last_state)
                });
            if let Some(state) = replacement {
                let layout = self.state.world.desktop(desktop).layout;
                let old = self
                    .state
                    .world
                    .tree
                    .node(node)
                    .client
                    .as_ref()
                    .unwrap()
                    .state;
                self.set_node_state(monitor, desktop, node, state);
                self.state.pending_effects.extend([
                    CommandEffect::SyncWindowState { node },
                    CommandEffect::Restack { node, auto_raise },
                ]);
                self.broadcast(
                    crate::types::SubscriberMask::NODE_STATE,
                    node_state_status(&self.state.world, monitor, desktop, node, old, false),
                );
                self.broadcast(
                    crate::types::SubscriberMask::NODE_STATE,
                    node_state_status(&self.state.world, monitor, desktop, node, state, true),
                );
                if self.state.world.monitor(monitor).active_desktop == Some(desktop)
                    && self.state.world.desktop(desktop).tree.focus == Some(node)
                {
                    self.report_effect();
                }
                self.layout_effect(monitor, desktop, layout);
                changed = true;
            }
        }
        if changed {
            self.arrange_effect(monitor, desktop);
        }
    }

    /// [`Self::focus_location_with`] under the user's configured focus policy.
    pub(crate) fn focus_location(&mut self, target: Coordinates, activate: bool) -> bool {
        self.focus_location_with(target, activate, FocusPolicy::configured(self.state))
    }

    /// Focuses `target`, honouring only the focus policy `policy` allows.
    ///
    /// Corrective focus changes driven by the pointer or by an X focus event
    /// pass a suppressed policy instead of temporarily clearing the persisted
    /// settings the policy is read from.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn focus_location_with(
        &mut self,
        target: Coordinates,
        activate: bool,
        policy: FocusPolicy,
    ) -> bool {
        let (Some(monitor), Some(desktop)) = (target.monitor, target.desktop) else {
            return false;
        };
        let guessed = target.node.is_none();
        let mut node = target.node.or_else(|| {
            let state = self.state.world.desktop(desktop).tree;
            state
                .focus
                .or_else(|| {
                    self.state
                        .history
                        .last_node(&self.state.world.tree, desktop, None)
                })
                .or_else(|| {
                    state
                        .root
                        .and_then(|root| self.state.world.tree.first_focusable_leaf(root))
                })
        });
        if node.is_some_and(|node| !self.state.world.tree.is_focusable(node)) {
            return false;
        }
        let previous_monitor = self.state.world.focused_monitor;
        let previous_desktop = self.state.world.monitor(monitor).active_desktop;
        if previous_desktop != Some(desktop)
            && self.state.world.monitor(monitor).sticky_count > 0
            && let Some(previous) = previous_desktop
        {
            if guessed
                && self
                    .state
                    .world
                    .desktop(previous)
                    .tree
                    .focus
                    .is_some_and(|focus| self.state.world.tree.node(focus).sticky)
            {
                node = self.state.world.desktop(previous).tree.focus;
            }
            self.transfer_sticky_nodes(monitor, previous, desktop, policy);
            if node.is_none() {
                node = self.state.world.desktop(desktop).tree.focus;
            }
        }
        if let Some(node) = node
            && self.state.world.desktop(desktop).tree.focus != Some(node)
        {
            self.neutralize_occluding_windows(monitor, desktop, node, policy.auto_raise);
        }
        if activate {
            if self.state.world.focused_monitor == Some(monitor)
                && previous_desktop == Some(desktop)
            {
                return false;
            }
            self.state.world.monitor_mut(monitor).active_desktop = Some(desktop);
        } else {
            self.state.world.focused_monitor = Some(monitor);
            self.state.world.monitor_mut(monitor).active_desktop = Some(desktop);
        }
        self.state.world.desktop_mut(desktop).tree.focus = node;
        if !activate
            && let Some(node) = node
            && self
                .state
                .world
                .tree
                .node(node)
                .client
                .as_ref()
                .is_some_and(|client| client.urgent)
        {
            if let Some(client) = self.state.world.tree.node_mut(node).client.as_mut() {
                client.urgent = false;
            }
            self.state.pending_effects.push(CommandEffect::Broadcast {
                mask: crate::types::SubscriberMask::NODE_FLAG,
                status: format!(
                    "node_flag 0x{:08X} 0x{:08X} 0x{:08X} urgent off\n",
                    self.state.world.monitor(monitor).external_id,
                    self.state.world.desktop(desktop).external_id,
                    self.state.world.tree.node(node).external_id,
                ),
                report: true,
            });
        }
        self.state.history.add(
            crate::history::Coordinates {
                monitor,
                desktop,
                node,
            },
            !activate,
        );
        self.state.pending_effects.push(CommandEffect::Focus {
            monitor,
            previous_monitor,
            desktop,
            previous_desktop,
            node,
            activate,
            auto_raise: policy.auto_raise,
        });
        self.state
            .pending_effects
            .push(CommandEffect::RefreshBorders);
        if !activate && previous_monitor != Some(monitor) && policy.pointer_follows_monitor {
            self.state.pending_effects.push(CommandEffect::WarpPointer {
                rectangle: self.state.world.monitor(monitor).rectangle,
            });
        }
        if !activate
            && policy.pointer_follows_focus
            && let Some(node) = node
        {
            self.state.pending_effects.push(CommandEffect::WarpPointer {
                rectangle: self.state.world.tree.node(node).rectangle,
            });
        }
        true
    }

    pub(super) fn resize_node(
        &mut self,
        target: Coordinates,
        handle: ResizeHandle,
        dx: i32,
        dy: i32,
    ) -> bool {
        let (Some(monitor), Some(desktop), Some(node)) =
            (target.monitor, target.desktop, target.node)
        else {
            return false;
        };
        let Some(client) = self.state.world.tree.node(node).client.clone() else {
            return false;
        };
        match client.state {
            ClientState::Fullscreen => false,
            ClientState::Tiled => {
                let plan = crate::pointer::plan_tiled_resize(
                    &self.state.world.tree,
                    node,
                    handle,
                    crate::pointer::ResizeInput::Relative { dx, dy },
                );
                if plan.vertical.is_none() && plan.horizontal.is_none() {
                    return false;
                }
                crate::pointer::apply_tiled_resize_plan(&mut self.state.world.tree, plan);
                self.arrange_effect(monitor, desktop);
                true
            }
            ClientState::Floating | ClientState::PseudoTiled => {
                let mut rectangle = crate::pointer::plan_floating_resize(
                    client.floating_rectangle,
                    handle,
                    crate::pointer::ResizeInput::Relative { dx, dy },
                );
                let (width, height) =
                    crate::arrange::apply_size_hints(&client, rectangle.width, rectangle.height);
                if handle.contains(ResizeHandle::LEFT) {
                    rectangle.x += rectangle.width - width;
                }
                if handle.contains(ResizeHandle::TOP) {
                    rectangle.y += rectangle.height - height;
                }
                rectangle.width = width;
                rectangle.height = height;
                self.state
                    .world
                    .tree
                    .node_mut(node)
                    .client
                    .as_mut()
                    .unwrap()
                    .floating_rectangle = rectangle;
                if client.state == ClientState::Floating {
                    self.state
                        .pending_effects
                        .push(CommandEffect::MoveResize { node, rectangle });
                } else {
                    self.arrange_effect(monitor, desktop);
                }
                true
            }
        }
    }

    /// [`Self::transfer_node_complete_with`] under the configured focus policy.
    pub(crate) fn transfer_node_complete(
        &mut self,
        source: Coordinates,
        destination: Coordinates,
        follow: bool,
    ) -> Result<crate::world::NodeMove, crate::tree::StructuralError> {
        let policy = FocusPolicy::configured(self.state);
        self.transfer_node_complete_with(source, destination, follow, policy)
    }

    /// Transfers a node, repairing focus under `policy` when the move takes the
    /// focused node out of its desktop.
    #[allow(clippy::too_many_lines)]
    fn transfer_node_complete_with(
        &mut self,
        source: Coordinates,
        destination: Coordinates,
        follow: bool,
        policy: FocusPolicy,
    ) -> Result<crate::world::NodeMove, crate::tree::StructuralError> {
        let (
            Some(source_monitor),
            Some(source_desktop),
            Some(node),
            Some(destination_monitor),
            Some(destination_desktop),
        ) = (
            source.monitor,
            source.desktop,
            source.node,
            destination.monitor,
            destination.desktop,
        )
        else {
            return Err(crate::tree::StructuralError::NotAttached);
        };
        let source_active =
            self.state.world.monitor(source_monitor).active_desktop == Some(source_desktop);
        let destination_active = self.state.world.monitor(destination_monitor).active_desktop
            == Some(destination_desktop);
        let source_focused =
            self.state.world.focused_monitor == Some(source_monitor) && source_active;
        let old_focus = self.state.world.desktop(source_desktop).tree.focus;
        let held_focus =
            old_focus.is_some_and(|focus| self.state.world.tree.is_descendant(focus, node));
        let sticky = if source_active {
            self.state.world.tree.sticky_count(node)
        } else {
            0
        };
        if self.state.sticky_still && sticky > 0 && !destination_active {
            return Err(crate::tree::StructuralError::InvalidAnchor);
        }
        let source_rectangle = self.state.world.monitor(source_monitor).rectangle;
        let destination_rectangle = self.state.world.monitor(destination_monitor).rectangle;
        let monitor_rectangles = self
            .state
            .world
            .monitor_order()
            .iter()
            .map(|monitor| (*monitor, self.state.world.monitor(*monitor).rectangle))
            .collect::<Vec<_>>();
        let adapt_geometry =
            self.state
                .world
                .tree
                .node(node)
                .client
                .as_ref()
                .is_none_or(|client| {
                    monitor::monitor_from_client(&monitor_rectangles, client.floating_rectangle)
                        != Some(destination_monitor)
                });
        let anchor = destination.node;
        self.state
            .history
            .remove_node(&self.state.world.tree, node, true);
        let moved = self.state.world.transfer_node(
            node,
            destination_desktop,
            anchor,
            self.state.settings.split_ratio,
        )?;
        if source_monitor != destination_monitor {
            if adapt_geometry {
                self.adapt_subtree_geometry(node, source_rectangle, destination_rectangle);
            }
            self.state.world.monitor_mut(source_monitor).sticky_count = self
                .state
                .world
                .monitor(source_monitor)
                .sticky_count
                .saturating_sub(sticky);
            self.state
                .world
                .monitor_mut(destination_monitor)
                .sticky_count = self
                .state
                .world
                .monitor(destination_monitor)
                .sticky_count
                .saturating_add(sticky);
        }
        let _ =
            self.state
                .stacking_order
                .stack(&self.state.world.tree, node, false, policy.auto_raise);
        if source_desktop != destination_desktop {
            if source_active && !destination_active {
                self.state
                    .pending_effects
                    .push(CommandEffect::SetDesktopVisibility {
                        desktop: destination_desktop,
                        visible: false,
                        preserve_sticky: false,
                    });
            } else if !source_active && destination_active {
                self.state
                    .pending_effects
                    .push(CommandEffect::SetDesktopVisibility {
                        desktop: destination_desktop,
                        visible: true,
                        preserve_sticky: false,
                    });
            }
        }
        if held_focus {
            if follow && source_focused {
                let _ = self.focus_location_with(
                    Coordinates::in_desktop(destination_monitor, destination_desktop, old_focus),
                    false,
                    policy,
                );
                let repaired = self.state.world.desktop(source_desktop).tree.focus;
                if repaired.is_some() {
                    let _ = self.focus_location_with(
                        Coordinates::in_desktop(source_monitor, source_desktop, repaired),
                        true,
                        policy,
                    );
                }
            } else if let Some(repaired) = self.state.world.desktop(source_desktop).tree.focus {
                let _ = self.focus_location_with(
                    Coordinates::node(source_monitor, source_desktop, repaired),
                    !source_focused,
                    policy,
                );
            }
        }
        self.structural_effects(&[
            (source_monitor, source_desktop),
            (destination_monitor, destination_desktop),
        ]);
        self.state.pending_effects.push(CommandEffect::Restack {
            node,
            auto_raise: policy.auto_raise,
        });
        let status = format!(
            "node_transfer 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X} 0x{:08X}\n",
            self.state.world.monitor(source_monitor).external_id,
            self.state.world.desktop(source_desktop).external_id,
            self.state.world.tree.node(node).external_id,
            self.state.world.monitor(destination_monitor).external_id,
            self.state.world.desktop(destination_desktop).external_id,
            anchor.map_or(0, |anchor| self.state.world.tree.node(anchor).external_id),
        );
        self.broadcast(crate::types::SubscriberMask::NODE_TRANSFER, status);
        Ok(moved)
    }

    /// Drops the stores' references to a removed monitor or desktop, and frees
    /// the tree nodes the removal orphaned.
    pub(super) fn purge_removal(&mut self, removal: &crate::world::Removal) {
        for (desktop, _) in &removal.desktops {
            self.state.history.remove_desktop(*desktop);
        }
        for root in &removal.roots {
            let count = self.state.world.tree.clients_count(*root);
            self.state.clients_count = self.state.clients_count.saturating_sub(count);
            self.state
                .stacking_order
                .remove_subtree(&self.state.world.tree, *root);
            // The desktops that held these trees are gone, so nothing else will
            // ever reach them. Upstream `free`s them here too.
            self.state.world.tree.destroy_subtree(*root);
        }
        self.state.forget_retired_nodes();
    }
}

pub(super) fn queue_layout_effect(
    state: &mut DaemonState,
    monitor: crate::world::MonitorId,
    desktop: crate::world::DesktopId,
    previous: Layout,
) {
    let next = state.world.desktop(desktop).layout;
    if next == previous {
        return;
    }
    state
        .pending_effects
        .push(CommandEffect::Arrange { monitor, desktop });
    state.pending_effects.push(CommandEffect::Broadcast {
        mask: crate::types::SubscriberMask::DESKTOP_LAYOUT,
        status: format!(
            "desktop_layout 0x{:08X} 0x{:08X} {}\n",
            state.world.monitor(monitor).external_id,
            state.world.desktop(desktop).external_id,
            next.protocol_name(),
        ),
        report: state.world.monitor(monitor).active_desktop == Some(desktop),
    });
}
