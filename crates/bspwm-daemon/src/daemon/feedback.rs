//! Preselection feedback windows: creation, stacking, visibility, and retirement.

use std::collections::HashSet;

use xcb::{Xid, XidNew, x};

use super::DaemonApp;
use crate::arrange;
use crate::helpers::color_pixel;
use crate::runtime::RuntimeError;
use crate::window;
use crate::world::{DesktopId, MonitorId};
use crate::x11::X11;

impl DaemonApp {
    /// The window of the topmost tiled client, which preselection feedback
    /// windows are stacked directly above.
    fn topmost_tiled_window(&self) -> Option<x::Window> {
        let xid = self
            .state
            .stacking_order
            .windows()
            .into_iter()
            .rev()
            .find(|xid| {
                self.managed_window(*xid).is_some_and(|(_, _, node)| {
                    self.client_of(node)
                        .is_some_and(|client| client.state.is_tiled())
                })
            })?;
        Some(x::Window::new(xid))
    }

    pub(super) fn restack_presel_feedbacks(
        &self,
        x11: &X11,
        desktop: DesktopId,
    ) -> Result<(), RuntimeError> {
        let Some(sibling) = self.topmost_tiled_window() else {
            return Ok(());
        };
        for plan in arrange::plan_presel_feedbacks(self.world(), desktop, &self.state.settings) {
            if let Some(feedback) = plan.feedback {
                window::stack_above(x11, x::Window::new(feedback), sibling)?;
            }
        }
        Ok(())
    }

    pub(super) fn sync_presel_feedbacks(
        &mut self,
        x11: &X11,
        monitor: MonitorId,
        desktop: DesktopId,
    ) -> Result<(), RuntimeError> {
        let plans = arrange::plan_presel_feedbacks(self.world(), desktop, &self.state.settings);
        let planned: HashSet<_> = plans.iter().map(|plan| plan.node).collect();
        let active = self.world().monitor(monitor).active_desktop == Some(desktop);
        let color = color_pixel(&self.state.settings.presel_feedback_color);

        for plan in plans {
            let feedback = if let Some(feedback) = plan.feedback {
                feedback
            } else {
                let feedback = x11.connection().generate_id::<x::Window>().resource_id();
                window::create_presel_feedback(x11, x::Window::new(feedback), color)?;
                if let Some(sibling) = self.topmost_tiled_window() {
                    window::stack_above(x11, x::Window::new(feedback), sibling)?;
                }
                self.node_mut(plan.node)
                    .presel
                    .as_mut()
                    .expect("feedback plan requires preselection")
                    .feedback = Some(feedback);
                feedback
            };
            window::move_resize(x11, x::Window::new(feedback), plan.rectangle)?;
            let visible = active && !self.node(plan.node).hidden;
            self.set_feedback_visibility(x11, feedback, visible)?;
        }

        let Some(root) = self.world().desktop(desktop).tree.root else {
            return Ok(());
        };
        let retired: Vec<u32> = self
            .tree()
            .preorder(root)
            .filter(|node| !planned.contains(node))
            .filter_map(|node| self.node(node).presel.and_then(|presel| presel.feedback))
            .collect();
        for feedback in retired {
            self.set_feedback_visibility(x11, feedback, false)?;
        }
        Ok(())
    }

    pub(super) fn set_feedback_visibility(
        &mut self,
        x11: &X11,
        feedback: u32,
        visible: bool,
    ) -> Result<(), RuntimeError> {
        if visible == self.mapped_feedbacks.contains(&feedback) {
            return Ok(());
        }
        window::set_visibility(x11, x::Window::new(feedback), visible)?;
        if visible {
            self.mapped_feedbacks.insert(feedback);
        } else {
            self.mapped_feedbacks.remove(&feedback);
        }
        Ok(())
    }

    pub(super) fn destroy_retired_feedbacks(&mut self, x11: &X11) -> Result<(), RuntimeError> {
        for feedback in self.tree_mut().take_retired_feedbacks() {
            self.mapped_feedbacks.remove(&feedback);
            window::destroy(x11, x::Window::new(feedback))?;
        }
        Ok(())
    }
}
