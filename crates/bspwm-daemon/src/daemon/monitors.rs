//! Monitor reconciliation, client adaptation, struts, and arrangement.

use xcb::{Xid, XidNew, x};

use super::DaemonApp;
use super::action::XAction;
use super::status::node_geometry_status;
use crate::arrange;
use crate::ewmh;
use crate::monitor::{self, ExistingMonitor, MonitorHandle, ReconcileSettings};
use crate::runtime::RuntimeError;
use crate::types::{Rectangle, SubscriberMask};
use crate::world::{DesktopId, MonitorId};
use crate::x11::X11;

/// [`stack_mirror::StackBackend`] that issues X11 stacking operations.
pub(super) struct X11StackBackend<'a> {
    pub x11: &'a X11,
}

impl stack_mirror::StackBackend for X11StackBackend<'_> {
    type Error = RuntimeError;
    fn stack_above(&mut self, window: u32, sibling: u32) -> Result<(), RuntimeError> {
        crate::window::stack_above(
            self.x11,
            xcb::x::Window::new(window),
            xcb::x::Window::new(sibling),
        )?;
        Ok(())
    }
    fn stack_below(&mut self, window: u32, sibling: u32) -> Result<(), RuntimeError> {
        crate::window::stack_below(
            self.x11,
            xcb::x::Window::new(window),
            xcb::x::Window::new(sibling),
        )?;
        Ok(())
    }
}

impl DaemonApp {
    #[doc(hidden)]
    pub fn arrange_desktop(
        &mut self,
        x11: &X11,
        monitor: MonitorId,
        desktop: DesktopId,
    ) -> Result<(), RuntimeError> {
        self.arrange_desktop_with_events(x11, monitor, desktop, true)
    }

    pub(super) fn arrange_desktop_quiet(
        &mut self,
        x11: &X11,
        monitor: MonitorId,
        desktop: DesktopId,
    ) -> Result<(), RuntimeError> {
        self.arrange_desktop_with_events(x11, monitor, desktop, false)
    }

    fn arrange_desktop_with_events(
        &mut self,
        x11: &X11,
        monitor: MonitorId,
        desktop: DesktopId,
        publish_geometry: bool,
    ) -> Result<(), RuntimeError> {
        self.invalidate_window_index();
        let actions = arrange::arrange(
            &mut self.state.world,
            monitor,
            desktop,
            &self.state.settings,
        );
        // Upstream only reconfigures a window whose server-side geometry
        // actually differs, so the arrangement still has to read it. Pipeline
        // the whole batch instead of blocking on one reply per node:
        // `ArrangeAction::geometry_changed` cannot stand in for this, because
        // it compares against the rectangle of the client's *current* state and
        // so reports "unchanged" for a state transition that has not reached
        // the server yet.
        let geometries: Vec<_> = actions
            .iter()
            .map(|action| {
                x11.send(&x::GetGeometry {
                    drawable: x::Drawable::Window(x::Window::new(action.window)),
                })
            })
            .collect();
        for (action, cookie) in actions.into_iter().zip(geometries) {
            let reply = x11.connection().wait_for_reply(cookie)?;
            let actual = Rectangle::from_x11(reply.x(), reply.y(), reply.width(), reply.height());
            crate::ewmh::set_frame_extents(
                x11,
                x::Window::new(action.window),
                action.border_width,
            )?;
            if actual == action.rectangle {
                Self::execute_action(
                    x11,
                    XAction::SetBorderWidth {
                        window: action.window,
                        border_width: action.border_width,
                    },
                )?;
                continue;
            }
            Self::execute_action(x11, Self::arrange_action(&action))?;
            if publish_geometry {
                let status = node_geometry_status(
                    self.monitor_xid(monitor),
                    self.desktop_xid(desktop),
                    self.xid(action.node),
                    action.rectangle,
                );
                self.publish(SubscriberMask::NODE_GEOMETRY, &status);
            }
        }
        self.sync_presel_feedbacks(x11, monitor, desktop)
    }

    /// Updates the EWMH stacking list and restacks preselection feedbacks.
    ///
    /// Call after any operation that changes `state.stacking_order`.
    pub(super) fn sync_stacking_ewmh(
        &self,
        x11: &X11,
        desktop: DesktopId,
    ) -> Result<(), RuntimeError> {
        ewmh::update_client_stacking_list(x11, &self.state.stacking_order)?;
        self.restack_presel_feedbacks(x11, desktop)
    }

    pub(super) fn apply_strut(
        &mut self,
        x11: &X11,
        window: x::Window,
    ) -> Result<bool, RuntimeError> {
        let Some(strut) = ewmh::get_strut_partial(x11, window)? else {
            return Ok(false);
        };
        let screen = x11.geometry();
        let monitors = self.world().monitor_order().to_vec();
        let mut changed = false;
        for monitor in monitors {
            let rectangle = self.world().monitor(monitor).rectangle;
            changed |= ewmh::apply_strut_partial(
                &mut self.state.world.monitor_mut(monitor).padding,
                rectangle,
                screen.width,
                screen.height,
                strut,
            );
        }
        Ok(changed)
    }

    pub(super) fn arrange_all(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        let locations: Vec<_> = self.world().desktops().collect();
        for (monitor, desktop) in locations {
            self.arrange_desktop(x11, monitor, desktop)?;
        }
        Ok(())
    }

    fn adapt_monitor_clients(
        &mut self,
        monitor: MonitorId,
        source: Rectangle,
        destination: Rectangle,
    ) {
        for desktop in self.world().monitor(monitor).desktops.clone() {
            self.adapt_desktop_clients(desktop, source, destination);
        }
    }

    fn adapt_desktop_clients(
        &mut self,
        desktop: DesktopId,
        source: Rectangle,
        destination: Rectangle,
    ) {
        let Some(root) = self.world().desktop(desktop).tree.root else {
            return;
        };
        self.state
            .world
            .tree
            .adapt_client_geometry(root, source, destination);
    }

    pub(super) fn reconcile_randr_monitors(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        let Ok(query) = monitor::query_randr_monitor_info(x11) else {
            return Ok(());
        };
        self.reconcile_monitor_query(x11, &query)
    }

    #[doc(hidden)]
    #[allow(clippy::too_many_lines)]
    pub fn reconcile_monitor_query(
        &mut self,
        x11: &X11,
        query: &monitor::MonitorQuery,
    ) -> Result<(), RuntimeError> {
        let existing: Vec<_> = self
            .world()
            .monitor_order()
            .iter()
            .map(|id| {
                let value = self.world().monitor(*id);
                ExistingMonitor {
                    id: *id,
                    output: (value.randr_id != 0).then_some(value.randr_id),
                    rectangle: value.rectangle,
                }
            })
            .collect();
        let plan = monitor::reconcile_monitors(
            &existing,
            query,
            ReconcileSettings {
                remove_disabled_monitors: self.state.settings.remove_disabled_monitors,
                remove_unplugged_monitors: self.state.settings.remove_unplugged_monitors,
                merge_overlapping_monitors: self.state.settings.merge_overlapping_monitors,
            },
        );
        let mut added = Vec::with_capacity(plan.additions.len());
        let mut arrange = Vec::new();

        for update in plan.updates {
            let old = self.world().monitor(update.id).rectangle;
            if old != update.rectangle {
                self.adapt_monitor_clients(update.id, old, update.rectangle);
                self.world_mut()
                    .update_monitor_rectangle(update.id, update.rectangle);
            }
            self.world_mut().monitor_mut(update.id).wired = update.wired;
            if update.wired {
                if let Some(root) = self.world().monitor(update.id).root_id {
                    monitor::update_monitor_root(
                        x11,
                        root,
                        &self.state.world.monitor(update.id).name,
                        update.rectangle,
                    )?;
                }
                let value = self.world().monitor(update.id);
                let status = format!(
                    "monitor_geometry 0x{:08X} {}x{}+{}+{}\n",
                    value.external_id,
                    update.rectangle.width,
                    update.rectangle.height,
                    update.rectangle.x,
                    update.rectangle.y,
                );
                self.publish(SubscriberMask::MONITOR_GEOMETRY, &status);
                arrange.extend(
                    self.world()
                        .monitor(update.id)
                        .desktops
                        .iter()
                        .map(|desktop| (update.id, *desktop)),
                );
            }
        }

        let settings = self.state.settings.clone();
        for addition in plan.additions {
            let external_id = x11.connection().generate_id::<x::Window>().resource_id();
            let id = self.world_mut().create_monitor(
                external_id,
                Some(&addition.info.name),
                addition.info.rectangle,
                &settings,
            );
            self.world_mut().monitor_mut(id).randr_id = addition.info.output.unwrap_or(0);
            let root = monitor::create_monitor_root(
                x11,
                &addition.info.name,
                addition.info.rectangle,
                settings.focus_follows_pointer,
            )?;
            self.world_mut().monitor_mut(id).root_id = Some(root);
            let status = format!(
                "monitor_add 0x{external_id:08X} {} {}x{}+{}+{}\n",
                addition.info.name,
                addition.info.rectangle.width,
                addition.info.rectangle.height,
                addition.info.rectangle.x,
                addition.info.rectangle.y,
            );
            self.publish(SubscriberMask::MONITOR_ADD, &status);
            added.push(id);
        }

        let resolve = |handle: MonitorHandle, added: &[MonitorId]| match handle {
            MonitorHandle::Existing(id) => id,
            MonitorHandle::Added(index) => added[index],
        };
        for removal in plan.removals {
            let source = resolve(removal.source, &added);
            if !self.world().monitor_order().contains(&source) {
                continue;
            }
            if let Some(destination) = removal.merge_into.map(|handle| resolve(handle, &added))
                && self.world().monitor_order().contains(&destination)
            {
                let source_external = self.world().monitor(source).external_id;
                let destination_external = self.world().monitor(destination).external_id;
                let source_rectangle = self.world().monitor(source).rectangle;
                let destination_rectangle = self.world().monitor(destination).rectangle;
                for desktop in self.world().monitor(source).desktops.clone() {
                    let desktop_external = self.world().desktop(desktop).external_id;
                    let sticky_count =
                        if self.world().monitor(source).active_desktop == Some(desktop) {
                            self.world()
                                .desktop(desktop)
                                .tree
                                .root
                                .map_or(0, |root| self.tree().sticky_count(root))
                        } else {
                            0
                        };
                    if self.world_mut().transfer_desktop(desktop, destination) {
                        self.world_mut().monitor_mut(source).sticky_count = self
                            .world()
                            .monitor(source)
                            .sticky_count
                            .saturating_sub(sticky_count);
                        self.world_mut().monitor_mut(destination).sticky_count = self
                            .world()
                            .monitor(destination)
                            .sticky_count
                            .saturating_add(sticky_count);
                        self.state.history.transfer_desktop(desktop, destination);
                        self.adapt_desktop_clients(
                            desktop,
                            source_rectangle,
                            destination_rectangle,
                        );
                        let status = format!(
                            "desktop_transfer 0x{source_external:08X} 0x{desktop_external:08X} 0x{destination_external:08X}\n"
                        );
                        self.publish(SubscriberMask::DESKTOP_TRANSFER, &status);
                        arrange.push((destination, desktop));
                    }
                }
            }
            let external_id = self.world().monitor(source).external_id;
            let root_id = self.world().monitor(source).root_id;
            if let Some(removed) = self.world_mut().remove_monitor_runtime(source) {
                // The desktop slots are already freed, so the removal hands the
                // external ids over rather than leaving them to be looked up.
                for (desktop, desktop_id) in removed.desktops {
                    let status = format!("desktop_remove 0x{external_id:08X} 0x{desktop_id:08X}\n");
                    self.publish(SubscriberMask::DESKTOP_REMOVE, &status);
                    self.state.history.remove_desktop(desktop);
                }
                for root in removed.roots {
                    // Remove all client leaves from the stacking mirror.
                    for leaf in self.state.world.tree.leaves(root) {
                        if self.state.world.tree.node(leaf).client.is_some() {
                            self.state
                                .stacking_order
                                .remove(self.state.world.tree.node(leaf).external_id);
                        }
                    }
                    self.state.clients_count = self
                        .state
                        .clients_count
                        .saturating_sub(self.tree().clients_count(root));
                    self.tree_mut().destroy_subtree(root);
                }
                self.state.forget_retired_nodes();
                if let Some(root) = root_id {
                    monitor::destroy_monitor_root(x11, root)?;
                }
                let status = format!("monitor_remove 0x{external_id:08X}\n");
                self.publish(SubscriberMask::MONITOR_REMOVE, &status);
            }
        }

        for id in self.world().monitor_order().to_vec() {
            if self.world().monitor(id).desktops.is_empty() {
                let desktop_external = x11.connection().generate_id::<x::Window>().resource_id();
                let desktop = self
                    .world_mut()
                    .create_desktop(desktop_external, None, &settings);
                self.world_mut().add_desktop(id, desktop);
                let monitor_external = self.world().monitor(id).external_id;
                let name = self.world().desktop(desktop).name.clone();
                let status = format!(
                    "desktop_add 0x{monitor_external:08X} 0x{desktop_external:08X} {name}\n"
                );
                self.publish(SubscriberMask::DESKTOP_ADD, &status);
                arrange.push((id, desktop));
            }
        }
        self.world_mut().primary_monitor = plan.primary.map(|handle| resolve(handle, &added));
        for (monitor, desktop) in arrange {
            if self.world().desktop_monitor(desktop) == Some(monitor) {
                self.arrange_desktop(x11, monitor, desktop)?;
            }
        }
        self.update_ewmh(x11)?;
        self.broadcast_report();
        Ok(())
    }
}
