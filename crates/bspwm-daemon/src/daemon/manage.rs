//! Window adoption: scheduling, rule resolution, bookkeeping, and removal.

use xcb::{Xid, XidNew, sync, x};

use super::action::XAction;
use super::events::XEventContext;
use super::{DaemonApp, WindowLocation};
use crate::events::EventHandler;
use crate::ewmh;
use crate::monitor;
use crate::rule::{ExternalRuleProcess, RuleConsequence, apply_builtin_rules, parse_keys_values};
use crate::runtime::RuntimeError;
use crate::state::CommandEffect;
use crate::tree::{ChildPolarity, Client, IcccmProps, SizeHints};
use crate::types::{Direction, Rectangle, SubscriberMask, WmFlags};
use crate::window;
use crate::world::{DesktopId, MonitorId};
use crate::x11::X11;

#[derive(Debug)]
pub(super) enum PendingRuleEvent {
    ClientMessage {
        window: x::Window,
        message_type: x::Atom,
        data: x::ClientMessageData,
    },
    PropertyNotify {
        window: x::Window,
        atom: x::Atom,
        time: x::Timestamp,
        state: x::Property,
    },
}

#[derive(Debug)]
pub(super) struct PendingRule {
    window: u32,
    consequence: RuleConsequence,
    initial_rectangle: Rectangle,
    size_hints: SizeHints,
    client_initial: ClientInitial,
    desktop_window: bool,
    process: ExternalRuleProcess,
    events: Vec<PendingRuleEvent>,
}

/// The per-client X state a rule consequence cannot supply.
#[doc(hidden)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientInitial {
    icccm: IcccmProps,
    urgent: bool,
    wm_flags: WmFlags,
    user_time: Option<x::Timestamp>,
    user_time_window: Option<u32>,
    startup_id: Option<String>,
    sync_request_counter: Option<sync::Counter>,
    transient_for: Option<u32>,
}

impl DaemonApp {
    #[doc(hidden)]
    #[allow(clippy::too_many_lines)]
    pub fn manage_window_with_initial(
        &mut self,
        window: u32,
        consequence: &RuleConsequence,
        initial_rectangle: Rectangle,
        size_hints: SizeHints,
        client_initial: &ClientInitial,
        internal_xid: u32,
    ) -> Option<WindowLocation> {
        if let Some(location) = self.managed_window(window) {
            return Some(location);
        }
        if !consequence.manage {
            return None;
        }
        let mut target = self.command().resolve_rule_target(consequence);
        if consequence.sticky {
            let monitor = self.world().focused_monitor?;
            let desktop = self.world().monitor(monitor).active_desktop?;
            target.monitor = Some(monitor);
            target.desktop = Some(desktop);
            target.node = self.world().desktop(desktop).tree.focus;
        }
        let monitor = target.monitor?;
        let desktop = target.desktop?;
        let was_empty = self.world().desktop(desktop).tree.root.is_none();
        let mut anchor = target
            .node
            .or(self.world().desktop(desktop).tree.focus)
            .or(self.world().desktop(desktop).tree.root);
        let split_ratio = self.state.settings.split_ratio;
        if let Some(value) = anchor {
            if let Some(direction) = consequence.split_dir {
                self.tree_mut()
                    .set_presel_direction(value, direction, split_ratio);
                let status = format!(
                    "node_presel {} dir {}\n",
                    self.node_ids(monitor, desktop, value),
                    direction.protocol_name(),
                );
                self.publish(SubscriberMask::NODE_PRESEL, &status);
            }
            if consequence.split_ratio != 0.0 {
                self.tree_mut()
                    .set_presel_ratio(value, consequence.split_ratio, split_ratio);
                let status = format!(
                    "node_presel {} ratio {:.6}\n",
                    self.node_ids(monitor, desktop, value),
                    consequence.split_ratio,
                );
                self.publish(SubscriberMask::NODE_PRESEL, &status);
            }

            let bare_receptacle = self.tree().is_leaf(value)
                && self.node(value).client.is_none()
                && self.node(value).presel.is_none();
            if !bare_receptacle && self.tree().is_protected_insertion_anchor(value) {
                let desktop_value = self.world().desktop(desktop);
                let gap = if self.state.settings.gapless_monocle
                    && desktop_value.layout == crate::types::Layout::Monocle
                {
                    0
                } else {
                    desktop_value.window_gap
                };
                if let Some(root) = desktop_value.tree.root
                    && let Some(public) = self.tree().find_public(root, gap)
                {
                    anchor = Some(public);
                }
                // `anchor` was `Some` when we entered this `if let` and is
                // only ever reassigned to another `Some`, so this is safe.
                let value = anchor?;
                if self.tree().is_protected_insertion_anchor(value) {
                    let rectangle = self.tree().placement_rectangle(value, gap);
                    let direction = if rectangle.width >= rectangle.height {
                        Direction::East
                    } else {
                        Direction::South
                    };
                    self.tree_mut()
                        .set_presel_direction(value, direction, split_ratio);
                    let status = format!(
                        "node_presel {} dir {}\n",
                        self.node_ids(monitor, desktop, value),
                        direction.protocol_name(),
                    );
                    self.publish(SubscriberMask::NODE_PRESEL, &status);
                }
            }
        }
        let node = self.tree_mut().add_node(window, split_ratio);
        let mut client = Client::from_settings(&self.state.settings);
        client.class_name.clone_from(&consequence.class_name);
        client.instance_name.clone_from(&consequence.instance_name);
        client.name.clone_from(&consequence.name);
        client.border_width = if consequence.border {
            self.world().desktop(desktop).border_width
        } else {
            0
        };
        client.floating_rectangle = consequence.rect.unwrap_or(initial_rectangle);
        let monitors = self.monitor_rectangles();
        if let Some(source_monitor) =
            monitor::monitor_from_client(&monitors, client.floating_rectangle)
        {
            let source = self.world().monitor(source_monitor).rectangle;
            client.floating_rectangle = window::embrace(client.floating_rectangle, source);
            client.floating_rectangle = window::adapt_geometry(
                client.floating_rectangle,
                source,
                self.world().monitor(monitor).rectangle,
            );
        }
        // Upstream centers when x == 0 && y == 0. Many clients (Steam, some Qt
        // apps) use small negative sentinels like (-1, -1) to mean "no
        // preference." Also center when the initial position falls outside the
        // target monitor, which prevents a configure-request-driven cross-monitor
        // transfer to the wrong desktop immediately after management.
        let unpositioned = initial_rectangle.x <= 0 && initial_rectangle.y <= 0;
        if consequence.center || consequence.rect.is_none() && unpositioned {
            client.floating_rectangle = window::center(
                client.floating_rectangle,
                self.world().monitor(monitor).rectangle,
                u16::try_from(client.border_width).unwrap_or(u16::MAX),
            );
        }
        if let Some(state) = consequence.state {
            client.state = state;
        }
        if consequence.state == Some(crate::types::ClientState::Floating)
            && let Some(anchor_client) = anchor.and_then(|anchor| self.client_of(anchor))
        {
            client.layer = anchor_client.layer;
        }
        if let Some(layer) = consequence.layer {
            client.layer = layer;
        }
        if consequence.honor_size_hints != crate::types::HonorSizeHintsMode::Default {
            client.honor_size_hints = consequence.honor_size_hints;
        }
        client.size_hints = size_hints;
        client.icccm = client_initial.icccm;
        client.urgent = client_initial.urgent;
        client.wm_flags = client_initial.wm_flags;
        client.transient_for = client_initial.transient_for;
        if let Some(counter) = client_initial.sync_request_counter {
            self.sync_request_clients.insert(window, counter);
        }
        let receives_focus = !consequence.hidden
            && consequence.focus
            && (self.world().monitor(monitor).active_desktop == Some(desktop)
                || consequence.follow);
        if receives_focus {
            client.urgent = false;
        }
        client.shown =
            self.world().monitor(monitor).active_desktop == Some(desktop) && !consequence.hidden;
        self.node_mut(node).client = Some(client);
        {
            let item = self.node_mut(node);
            item.hidden = consequence.hidden;
            item.sticky = consequence.sticky;
            item.private = consequence.private;
            item.locked = consequence.locked;
            item.marked = consequence.marked;
        }

        // Read before the insert: filling a bare receptacle consumes the
        // anchor, and the `node_add` record still names it.
        let anchor_id = anchor.map_or(0, |id| self.xid(id));
        let mut tree_state = self.world().desktop(desktop).tree;
        if let Some(anchor) = anchor {
            let split_ratio = self.state.settings.split_ratio;
            let branch = self.tree_mut().add_node(internal_xid, split_ratio);
            let polarity = match self.state.settings.initial_polarity {
                crate::types::ChildPolarity::FirstChild => ChildPolarity::First,
                crate::types::ChildPolarity::SecondChild => ChildPolarity::Second,
            };
            let had_presel = self.node(anchor).presel.is_some();
            // Spiral insertion must know whether the new subtree is vacant
            // before deciding whether to rotate the existing parent.
            self.tree_mut().sync_vacancy(node);
            let scheme = self.state.settings.automatic_scheme;
            let result = self.tree_mut().insert_automatic(
                &mut tree_state,
                node,
                anchor,
                branch,
                polarity,
                scheme,
            );
            match result {
                // The anchor was a bare receptacle, so `insert` replaced it in
                // place and the branch allocated for the split went unused.
                Ok(Some(_)) => self.tree_mut().destroy_subtree(branch),
                Ok(None) => {}
                Err(_) => {
                    self.tree_mut().destroy_subtree(branch);
                    self.tree_mut().destroy_subtree(node);
                    self.state.forget_retired_nodes();
                    return None;
                }
            }
            if had_presel {
                let status = format!(
                    "node_presel {} cancel\n",
                    self.node_ids(monitor, desktop, anchor),
                );
                self.publish(SubscriberMask::NODE_PRESEL, &status);
            }
            self.state.forget_retired_nodes();
        } else if self
            .tree_mut()
            .insert(&mut tree_state, node, None, None, ChildPolarity::Second)
            .is_err()
        {
            self.tree_mut().destroy_subtree(node);
            self.state.forget_retired_nodes();
            return None;
        }
        self.tree_mut().sync_vacancy(node);
        if !consequence.hidden && consequence.focus {
            tree_state.focus = Some(node);
            if self.world().monitor(monitor).active_desktop == Some(desktop) || consequence.follow {
                self.world_mut().focused_monitor = Some(monitor);
                self.world_mut().monitor_mut(monitor).active_desktop = Some(desktop);
                self.node_mut(node).client.as_mut()?.shown = true;
            }
        }
        self.world_mut().desktop_mut(desktop).tree = tree_state;
        let previous_layout = self.world().desktop(desktop).layout;
        let single_monocle = self.state.settings.single_monocle;
        let leave_single_monocle = single_monocle
            && previous_layout == crate::types::Layout::Monocle
            && self
                .world()
                .desktop(desktop)
                .tree
                .root
                .is_some_and(|root| self.tree().tiled_count(root, true) > 1);
        if leave_single_monocle {
            let user_layout = self.world().desktop(desktop).user_layout;
            if self
                .world_mut()
                .set_layout(desktop, user_layout, false, single_monocle)
            {
                let status = format!(
                    "desktop_layout {} {}\n",
                    self.desktop_ids(monitor, desktop),
                    user_layout.protocol_name(),
                );
                self.publish(SubscriberMask::DESKTOP_LAYOUT, &status);
                if self.world().focused_monitor == Some(monitor)
                    && self.world().monitor(monitor).active_desktop == Some(desktop)
                {
                    self.broadcast_report();
                }
            }
        }
        if consequence.sticky {
            let sticky_count = self.world().monitor(monitor).sticky_count.saturating_add(1);
            self.world_mut().monitor_mut(monitor).sticky_count = sticky_count;
        }
        self.state.clients_count = self.state.clients_count.saturating_add(1);
        self.state.history.add(
            crate::history::Coordinates {
                monitor,
                desktop,
                node: Some(node),
            },
            !consequence.hidden && consequence.focus,
        );
        let status = format!(
            "node_add {} 0x{window:08X}\n",
            self.node_ids_raw(monitor, desktop, anchor_id)
        );
        self.publish(SubscriberMask::NODE_ADD, &status);
        if was_empty {
            self.broadcast_report();
        }
        Some((monitor, desktop, node))
    }

    #[doc(hidden)]
    pub fn schedule_window(
        &mut self,
        x11: &X11,
        window_id: u32,
    ) -> Result<Option<WindowLocation>, RuntimeError> {
        if self.managed_window(window_id).is_some()
            || self
                .pending_rules
                .iter()
                .any(|rule| rule.window == window_id)
        {
            return Ok(None);
        }
        let window_id_typed = x::Window::new(window_id);
        let properties = window::rule_properties(x11, window_id_typed)?;
        if properties.override_redirect {
            return Ok(None);
        }
        let mut consequence = RuleConsequence::default();
        let desktop_window = properties
            .builtin
            .window_types
            .contains(&crate::rule::BuiltinWindowType::Desktop);
        apply_builtin_rules(&properties.builtin, &mut consequence);
        consequence.set_window_properties(&properties.identity);
        self.state.rules.apply_rules(&mut consequence);
        if !self.state.settings.external_rules_command.is_empty() {
            self.command().resolve_rule_consequence(&mut consequence);
            if let Ok(process) = ExternalRuleProcess::spawn(
                &self.state.settings.external_rules_command,
                window_id,
                &consequence,
            ) {
                self.pending_rules.push(PendingRule {
                    window: window_id,
                    consequence,
                    initial_rectangle: properties.geometry.rectangle,
                    size_hints: properties.size_hints,
                    client_initial: ClientInitial {
                        icccm: properties.icccm,
                        urgent: properties.urgent,
                        wm_flags: properties.wm_flags,
                        user_time: properties.user_time,
                        user_time_window: properties.user_time_window,
                        startup_id: properties.startup_id,
                        sync_request_counter: properties.sync_request_counter,
                        transient_for: properties.transient_for,
                    },
                    desktop_window,
                    process,
                    events: Vec::new(),
                });
                return Ok(None);
            }
        }

        self.finish_scheduled_window(
            x11,
            window_id,
            &consequence,
            properties.geometry.rectangle,
            properties.size_hints,
            &ClientInitial {
                icccm: properties.icccm,
                urgent: properties.urgent,
                wm_flags: properties.wm_flags,
                user_time: properties.user_time,
                user_time_window: properties.user_time_window,
                startup_id: properties.startup_id,
                sync_request_counter: properties.sync_request_counter,
                transient_for: properties.transient_for,
            },
            desktop_window,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn finish_scheduled_window(
        &mut self,
        x11: &X11,
        window_id: u32,
        consequence: &RuleConsequence,
        initial_rectangle: Rectangle,
        size_hints: SizeHints,
        client_initial: &ClientInitial,
        desktop_window: bool,
    ) -> Result<Option<WindowLocation>, RuntimeError> {
        let window_id_typed = x::Window::new(window_id);
        if !window::exists(x11, x::Window::new(window_id)) {
            return Ok(None);
        }
        if !self.state.settings.ignore_ewmh_struts && self.apply_strut(x11, window_id_typed)? {
            let _ = window::listen_for_property_changes(x11, window_id_typed);
            self.arrange_all(x11)?;
            ewmh::update_workareas(x11, self.world())?;
        }
        if !consequence.manage {
            let mut plan = Vec::new();
            if desktop_window {
                plan.push(XAction::Lower { window: window_id });
            }
            plan.push(XAction::SetWmStateNormal { window: window_id });
            plan.push(XAction::Map { window: window_id });
            Self::execute_plan(x11, &plan)?;
            return Ok(None);
        }

        let focus_allowed = self.initial_focus_allowed(client_initial);
        let mut consequence = consequence.clone();
        if consequence.focus && !focus_allowed {
            consequence.focus = false;
        }
        if consequence.border
            && self.state.settings.borderless_csd
            && ewmh::has_csd(x11, window_id_typed)
        {
            consequence.border = false;
        }
        let user_time_window = client_initial.user_time_window;
        let user_time = client_initial.user_time;
        let internal_xid = x11.connection().generate_id::<x::Window>().resource_id();
        let Some(location) = self.manage_window_with_initial(
            window_id,
            &consequence,
            initial_rectangle,
            size_hints,
            client_initial,
            internal_xid,
        ) else {
            return Ok(None);
        };
        let (monitor, desktop, node) = location;
        if let Some(auxiliary) = user_time_window
            && auxiliary != window_id
            && window::listen_for_property_changes(x11, x::Window::new(auxiliary)).is_ok()
        {
            self.user_time_windows.insert(auxiliary, window_id);
        }
        if consequence.focus
            && let Some(user_time) = user_time
        {
            self.note_user_time(user_time);
        }
        self.destroy_retired_feedbacks(x11)?;
        self.arrange_desktop(x11, monitor, desktop)?;
        let mut plan = Vec::new();
        plan.push(XAction::SetClientEventMask {
            window: window_id,
            enter_window: self.state.settings.focus_follows_pointer,
        });
        plan.push(XAction::SetWmStateNormal { window: window_id });
        let client = self.client_of(node);
        if client.is_some_and(|client| client.shown) {
            plan.push(XAction::Map { window: window_id });
        }
        let focus_client = if !consequence.hidden
            && consequence.focus
            && self.world().focused_monitor == Some(monitor)
            && self.world().monitor(monitor).active_desktop == Some(desktop)
        {
            let client = self.client(node);
            Some((client.icccm.input_hint, client.icccm.take_focus))
        } else {
            None
        };
        let focused = focus_client.is_some();
        Self::execute_plan(x11, &plan)?;
        if let Some((input_hint, take_focus)) = focus_client {
            window::focus_client(x11, window_id_typed, input_hint, take_focus)?;
        }
        crate::pointer::grab_client_buttons(
            x11,
            window_id_typed,
            &self.state.settings,
            self.lock_masks,
        )?;
        let stack_info = self.client_of(node).map(|client| {
            (
                self.xid(node),
                crate::stack::stack_level(client),
                crate::stack::stacking_enabled(client, self.state.auto_raise),
                client.transient_for,
            )
        });
        if let Some((window_id, level, stacking_enabled, transient_for)) = stack_info {
            let mut backend = super::monitors::X11StackBackend { x11 };
            if let Some(parent_xid) = transient_for {
                self.state
                    .stacking_order
                    .set_transient(window_id, parent_xid);
            }
            // Upstream stack() skips floating clients while auto_raise is
            // disabled. Other new clients are placed at the top of their
            // level when focused and at the bottom otherwise.
            if stacking_enabled {
                self.state
                    .stacking_order
                    .set_level(&mut backend, window_id, level, focused)?;
            }
        }
        self.sync_stacking_ewmh(x11, desktop)?;
        self.sync_window_state(x11, node)?;
        self.refresh_colors(x11)?;
        self.update_ewmh(x11)?;
        Ok(Some(location))
    }

    fn initial_focus_allowed(&self, initial: &ClientInitial) -> bool {
        if initial.user_time == Some(0) {
            return false;
        }
        let mut candidate = initial.user_time;
        if let Some(startup_time) = initial
            .startup_id
            .as_deref()
            .and_then(|id| self.startup.timestamp(id))
            && candidate.is_none_or(|time| timestamp_is_later(startup_time, time))
        {
            candidate = Some(startup_time);
        }
        let (Some(candidate), Some(last_user_time)) = (candidate, self.last_user_time) else {
            return true;
        };
        candidate == last_user_time || timestamp_is_later(candidate, last_user_time)
    }

    pub(super) fn note_user_time(&mut self, timestamp: x::Timestamp) {
        if timestamp != 0
            && self
                .last_user_time
                .is_none_or(|current| timestamp_is_later(timestamp, current))
        {
            self.last_user_time = Some(timestamp);
        }
    }

    pub(super) fn user_time_owner(&self, window: u32) -> Option<u32> {
        self.user_time_windows.get(&window).copied()
    }

    pub(super) fn poll_pending_rules(&mut self, x11: &X11) -> Result<bool, RuntimeError> {
        self.reaping_rules.retain_mut(|process| !process.reap());
        let completed: Vec<PendingRule> = self
            .pending_rules
            .extract_if(.., |rule| rule.process.poll())
            .collect();
        if completed.is_empty() {
            return Ok(false);
        }
        for mut pending in completed {
            parse_keys_values(
                &String::from_utf8_lossy(pending.process.output()),
                &mut pending.consequence,
            );
            let location = self.finish_scheduled_window(
                x11,
                pending.window,
                &pending.consequence,
                pending.initial_rectangle,
                pending.size_hints,
                &pending.client_initial,
                pending.desktop_window,
            )?;
            if location.is_some() {
                for event in pending.events {
                    let mut context = XEventContext { app: self, x11 };
                    match event {
                        PendingRuleEvent::ClientMessage {
                            window,
                            message_type,
                            data,
                        } => {
                            let event = x::ClientMessageEvent::new(window, message_type, data);
                            context.client_message(&event)?;
                        }
                        PendingRuleEvent::PropertyNotify {
                            window,
                            atom,
                            time,
                            state,
                        } => {
                            let event = x::PropertyNotifyEvent::new(window, atom, time, state);
                            context.property_notify(&event)?;
                        }
                    }
                }
            }
            if !pending.process.reap() {
                self.reaping_rules.push(pending.process);
            }
        }
        Ok(true)
    }

    /// Terminates and forgets every external rule process, pending or reaping.
    pub(super) fn terminate_pending_rules(&mut self) {
        for pending in &mut self.pending_rules {
            pending.process.terminate();
        }
        self.pending_rules.clear();
        for process in &mut self.reaping_rules {
            process.terminate();
        }
        self.reaping_rules.clear();
    }

    /// Queues an event for replay once `window`'s external rule finishes, and
    /// reports whether such a rule is still pending.
    fn postpone_event(&mut self, window: x::Window, event: PendingRuleEvent) -> bool {
        let window = window.resource_id();
        let Some(pending) = self
            .pending_rules
            .iter_mut()
            .find(|pending| pending.window == window)
        else {
            return false;
        };
        pending.events.push(event);
        true
    }

    pub(super) fn postpone_client_message(&mut self, event: &x::ClientMessageEvent) -> bool {
        self.postpone_event(
            event.window(),
            PendingRuleEvent::ClientMessage {
                window: event.window(),
                message_type: event.r#type(),
                data: event.data(),
            },
        )
    }

    pub(super) fn postpone_property_notify(&mut self, event: &x::PropertyNotifyEvent) -> bool {
        self.postpone_event(
            event.window(),
            PendingRuleEvent::PropertyNotify {
                window: event.window(),
                atom: event.atom(),
                time: event.time(),
                state: event.state(),
            },
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn forget_window(&mut self, window: u32) -> Option<(MonitorId, DesktopId)> {
        let (monitor, desktop, node) = self.managed_window(window)?;
        self.user_time_windows.retain(|_, owner| *owner != window);
        self.sync_request_clients.remove(&window);
        self.pending_ewmh_pings.remove(&window);
        if self.pointer_grab.as_ref().is_some_and(|grab| {
            self.state
                .world
                .tree
                .get(grab.node)
                .is_some_and(|n| n.external_id == window)
        }) {
            self.pointer_grab = None;
        }
        // Clear stale transient-parent references so the XID cannot bind to
        // an unrelated future window.
        let orphans: Vec<_> = self
            .all_client_nodes()
            .into_iter()
            .filter(|n| {
                self.state
                    .world
                    .tree
                    .get(*n)
                    .and_then(|nd| nd.client.as_ref())
                    .is_some_and(|c| c.transient_for == Some(window))
            })
            .collect();
        for orphan in orphans {
            if let Some(client) = self.node_mut(orphan).client.as_mut() {
                client.transient_for = None;
            }
        }
        let was_sticky = self.node(node).sticky;
        let status = format!(
            "node_remove {} 0x{window:08X}\n",
            self.desktop_ids(monitor, desktop)
        );
        self.publish(SubscriberMask::NODE_REMOVE, &status);
        self.state
            .history
            .remove_node(&self.state.world.tree, node, true);
        // Remove all client leaves of this subtree from the stacking mirror.
        for leaf in self.state.world.tree.leaves(node) {
            if self.state.world.tree.node(leaf).client.is_some() {
                let xid = self.state.world.tree.node(leaf).external_id;
                self.state.stacking_order.remove(xid);
                self.state.stacking_order.clear_transient(xid);
            }
        }
        self.tree_mut().cancel_presel(node);
        let mut tree_state = self.world().desktop(desktop).tree;
        let Ok(unlink_result) = self.tree_mut().unlink(&mut tree_state, node) else {
            return None;
        };
        if self.state.settings.removal_adjustment {
            let scheme = self.state.settings.automatic_scheme;
            self.tree_mut()
                .apply_removal_adjustment(&unlink_result, scheme);
        }
        // `unlink` only detaches: the node is now unreachable from any desktop,
        // so this is where upstream `free`s it.
        self.tree_mut().destroy_subtree(node);
        self.state.forget_retired_nodes();
        let repair_focus = tree_state.focus.is_none();
        if repair_focus {
            tree_state.focus = self
                .state
                .history
                .last_node(&self.state.world.tree, desktop, None)
                .or_else(|| {
                    tree_state
                        .root
                        .and_then(|root| self.tree().first_focusable_leaf(root))
                });
        }
        self.world_mut().desktop_mut(desktop).tree = tree_state;
        if was_sticky {
            let sticky_count = self.world().monitor(monitor).sticky_count.saturating_sub(1);
            self.world_mut().monitor_mut(monitor).sticky_count = sticky_count;
        }
        self.state.clients_count = self.state.clients_count.saturating_sub(1);
        let previous_layout = self.world().desktop(desktop).layout;
        let single_monocle = self.state.settings.single_monocle;
        let enter_single_monocle = single_monocle
            && previous_layout != crate::types::Layout::Monocle
            && self
                .world()
                .desktop(desktop)
                .tree
                .root
                .is_none_or(|root| self.tree().tiled_count(root, true) <= 1);
        if enter_single_monocle
            && self.world_mut().set_layout(
                desktop,
                crate::types::Layout::Monocle,
                false,
                single_monocle,
            )
        {
            let status = format!(
                "desktop_layout {} monocle\n",
                self.desktop_ids(monitor, desktop),
            );
            self.publish(SubscriberMask::DESKTOP_LAYOUT, &status);
        }
        if repair_focus {
            let successor = self.world().desktop(desktop).tree.focus;
            let globally_focused = self.world().focused_monitor == Some(monitor)
                && self.world().monitor(monitor).active_desktop == Some(desktop);
            if successor.is_some() {
                self.state.history.add(
                    crate::history::Coordinates {
                        monitor,
                        desktop,
                        node: successor,
                    },
                    globally_focused,
                );
            }
            let previous_node = if globally_focused {
                self.world()
                    .focused_monitor
                    .and_then(|m| self.world().monitor(m).active_desktop)
                    .and_then(|d| self.world().desktop(d).tree.focus)
            } else {
                None
            };
            self.state.pending_effects.push(CommandEffect::Focus {
                monitor,
                previous_monitor: self.world().focused_monitor,
                desktop,
                previous_desktop: Some(desktop),
                node: successor,
                previous_node,
                activate: !globally_focused,
                auto_raise: self.state.auto_raise,
            });
            self.state
                .pending_effects
                .push(CommandEffect::RefreshBorders);
        } else {
            // Upstream always arranges after remove_node (window.c:243).
            self.state
                .pending_effects
                .push(CommandEffect::Arrange { monitor, desktop });
            self.broadcast_report();
        }
        Some((monitor, desktop))
    }
}

fn timestamp_is_later(candidate: x::Timestamp, reference: x::Timestamp) -> bool {
    candidate.wrapping_sub(reference).cast_signed() > 0
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::arrange;
    use crate::commands::CommandHandler;
    use crate::daemon::test_support::{
        app_with_desktop, manage_window, manage_window_with, read_available, subscribe_socket,
    };
    use crate::state::CommandEffect;
    use crate::tree::Presel;
    use crate::types::{ClientState, HonorSizeHintsMode, Layout, SplitType, StackLayer};

    #[test]
    fn unix_subscription_gets_manage_and_unmanage_records() {
        let (mut app, _, _) = app_with_desktop();
        let mut client = subscribe_socket(
            &mut app,
            b"subscribe\x00-c\x002\x00node_add\x00node_remove\x00",
        );
        let _ = manage_window(&mut app, 0xabc);
        assert_eq!(
            read_available(&mut client),
            b"node_add 0x00000001 0x00000002 0x00000000 0x00000ABC\n"
        );
        assert!(app.forget_window(0xabc).is_some());
        assert_eq!(
            read_available(&mut client),
            b"node_remove 0x00000001 0x00000002 0x00000ABC\n"
        );
        assert_eq!(app.subscriber_count(), 0);
    }

    #[test]
    fn map_bookkeeping_produces_an_executable_arrangement_plan() {
        let (mut app, monitor, desktop) = app_with_desktop();
        let (_, _, node) = manage_window(&mut app, 0xabc);
        let arranged =
            arrange::arrange(&mut app.state.world, monitor, desktop, &app.state.settings);
        let plan: Vec<_> = arranged.iter().map(DaemonApp::arrange_action).collect();
        assert_eq!(app.state.clients_count, 1);
        assert_eq!(app.state.world.tree.node(node).external_id, 0xabc);
        assert!(matches!(
            plan.first(),
            Some(XAction::Configure { window: 0xabc, .. })
        ));
        let repeated =
            arrange::arrange(&mut app.state.world, monitor, desktop, &app.state.settings);
        assert_eq!(repeated.len(), 1);
        assert_eq!(repeated[0].node, node);
        assert_eq!(repeated[0].window, 0xabc);
        assert!(!repeated[0].geometry_changed);
    }

    #[test]
    fn focus_mutation_updates_history_urgency_and_deferred_lifecycle() {
        let (mut app, monitor, desktop) = app_with_desktop();
        let (_, _, node) = manage_window(&mut app, 10);
        app.state
            .world
            .tree
            .node_mut(node)
            .client
            .as_mut()
            .unwrap()
            .urgent = true;
        assert!(CommandHandler::new(&mut app.state).focus_location(
            crate::query::Coordinates {
                monitor: Some(monitor),
                desktop: Some(desktop),
                node: Some(node),
            },
            false,
        ));
        assert!(
            !app.state
                .world
                .tree
                .node(node)
                .client
                .as_ref()
                .unwrap()
                .urgent
        );
        assert_eq!(
            app.state.history.entries().last().unwrap().location.node,
            Some(node)
        );
        assert!(app.state.pending_effects.iter().any(|effect| {
            matches!(effect, CommandEffect::Focus { node: Some(value), .. } if *value == node)
        }));
    }

    #[test]
    fn destroy_bookkeeping_is_idempotent_and_preserves_invariants() {
        let (mut app, _, _) = app_with_desktop();
        manage_window(&mut app, 12);
        assert!(app.forget_window(12).is_some());
        assert!(app.forget_window(12).is_none());
        assert_eq!(app.state.clients_count, 0);
        assert_eq!(app.state.validate(), Ok(()));
    }

    /// The arena used to be a `Vec` that only ever grew: every map/unmap cycle
    /// left both the client's node and the branch that had been split for it
    /// behind forever. Freeing the slots has to return the arena to the size it
    /// started at, however many cycles run.
    #[test]
    fn repeated_map_and_unmap_cycles_return_the_arena_to_its_starting_size() {
        let (mut app, _, _) = app_with_desktop();
        let baseline = app.tree().len();
        assert_eq!(baseline, 0);

        for round in 0..40_u32 {
            let windows = [0x100 + round * 4, 0x101 + round * 4, 0x102 + round * 4];
            for window in windows {
                manage_window(&mut app, window);
            }
            // Three clients need two internal branches to hold them.
            assert_eq!(app.tree().len(), 5);
            assert_eq!(app.state.validate(), Ok(()));
            for window in windows {
                assert!(app.forget_window(window).is_some());
            }
            assert_eq!(
                app.tree().len(),
                baseline,
                "arena grew after {} map/unmap cycles",
                round + 1
            );
            assert_eq!(app.state.clients_count, 0);
            assert_eq!(app.state.validate(), Ok(()));
        }
        assert!(app.state.history.entries().is_empty());
        assert!(app.state.stacking_order.is_empty());
    }

    /// Filling a receptacle consumes it, and the branch `manage_window`
    /// speculatively allocated for the split goes unused; neither may survive.
    #[test]
    fn filling_a_receptacle_frees_both_it_and_the_unused_branch() {
        let (mut app, _, desktop) = app_with_desktop();
        let external_id = app.state.world.next_external_id();
        let receptacle = app
            .state
            .world
            .insert_receptacle(desktop, None, 0.5)
            .unwrap();
        assert_eq!(app.tree().len(), 1);

        let (_, _, node) = manage_window(&mut app, 0xabc);
        assert_ne!(node, receptacle);
        assert!(!app.tree().is_live(receptacle));
        assert_eq!(app.tree().len(), 1);
        assert_eq!(app.state.validate(), Ok(()));
        // The freed receptacle's external id is available again.
        assert_eq!(app.state.world.next_external_id(), external_id);
    }

    #[test]
    fn consequence_placement_applies_fields_and_uses_unique_internal_ids() {
        let (mut app, monitor, desktop) = app_with_desktop();
        let mut consequence = RuleConsequence::default();
        consequence
            .set_window_properties(&crate::rule::WindowProperties::new("App", "main", "Title"));
        consequence.state = Some(ClientState::Floating);
        consequence.layer = Some(StackLayer::Above);
        consequence.split_dir = Some(Direction::North);
        consequence.split_ratio = 0.3;
        consequence.rect = Some(Rectangle::new(4, 5, 20, 10));
        consequence.honor_size_hints = HonorSizeHintsMode::Yes;
        consequence.private = true;
        consequence.locked = true;
        consequence.marked = true;
        consequence.border = false;
        let first = manage_window_with(
            &mut app,
            10,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1000,
        )
        .unwrap()
        .2;
        let _second = manage_window_with(
            &mut app,
            11,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1001,
        )
        .unwrap()
        .2;
        let third = manage_window_with(
            &mut app,
            12,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1002,
        )
        .unwrap()
        .2;
        let client = app.state.world.tree.node(first).client.as_ref().unwrap();
        assert_eq!(client.class_name, "App");
        assert_eq!(client.instance_name, "main");
        assert_eq!(client.name, "Title");
        assert_eq!(client.state, ClientState::Floating);
        assert!(app.state.world.tree.node(first).vacant);
        assert!(app.state.world.tree.node(third).vacant);
        assert_eq!(client.layer, StackLayer::Above);
        assert_eq!(client.border_width, 0);
        assert_eq!(client.floating_rectangle, Rectangle::new(4, 5, 20, 10));
        assert_eq!(client.honor_size_hints, HonorSizeHintsMode::Yes);
        let first_parent = app.state.world.tree.node(first).parent.unwrap();
        let second_parent = app.state.world.tree.node(third).parent.unwrap();
        assert!(app.state.world.tree.node(first_parent).vacant);
        assert!(app.state.world.tree.node(second_parent).vacant);
        assert_ne!(
            app.state.world.tree.node(first_parent).external_id,
            app.state.world.tree.node(second_parent).external_id
        );
        assert_eq!(
            app.state.world.tree.node(second_parent).split_type,
            SplitType::Horizontal
        );
        assert!((app.state.world.tree.node(second_parent).split_ratio - 0.3).abs() < f64::EPSILON);
        assert_eq!(app.state.world.desktop(desktop).tree.focus, Some(third));
        assert_eq!(app.state.world.focused_monitor, Some(monitor));
    }

    #[test]
    fn private_focused_anchor_redirects_insertion_to_a_public_leaf() {
        let (mut app, monitor, desktop) = app_with_desktop();
        let (_, _, public) = manage_window(&mut app, 10);
        let (_, _, private) = manage_window(&mut app, 11);
        let _ = arrange::arrange(&mut app.state.world, monitor, desktop, &app.state.settings);
        app.state.world.tree.node_mut(private).private = true;
        let root = app.state.world.desktop(desktop).tree.root.unwrap();
        assert_eq!(app.state.world.desktop(desktop).tree.focus, Some(private));
        assert!(app.state.world.tree.is_protected_insertion_anchor(private));
        assert_eq!(app.state.world.tree.find_public(root, 0), Some(public));

        let (_, _, inserted) = manage_window(&mut app, 12);
        assert_eq!(
            app.state.world.tree.node(public).parent,
            app.state.world.tree.node(inserted).parent
        );
        assert_ne!(
            app.state.world.tree.node(private).parent,
            app.state.world.tree.node(inserted).parent
        );
    }

    #[test]
    fn rule_split_fields_override_anchor_preselection_before_insertion() {
        let (mut app, _, _) = app_with_desktop();
        let (_, _, anchor) = manage_window(&mut app, 10);
        app.state.world.tree.node_mut(anchor).presel = Some(Presel {
            split_dir: Direction::West,
            split_ratio: 0.8,
            feedback: None,
        });
        let mut consequence = RuleConsequence::default();
        consequence.split_dir = Some(Direction::South);
        consequence.split_ratio = 0.3;

        let inserted = manage_window_with(
            &mut app,
            11,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1000,
        )
        .unwrap()
        .2;
        let parent = app.state.world.tree.node(inserted).parent.unwrap();
        assert_eq!(
            app.state.world.tree.node(parent).split_type,
            SplitType::Horizontal
        );
        assert!((app.state.world.tree.node(parent).split_ratio - 0.3).abs() < f64::EPSILON);
        assert_eq!(app.state.world.tree.node(parent).first_child, Some(anchor));
        assert_eq!(
            app.state.world.tree.node(parent).second_child,
            Some(inserted)
        );
        assert!(app.state.world.tree.node(anchor).presel.is_none());
    }

    #[test]
    fn rule_split_ratio_preserves_an_existing_preselection_direction() {
        let (mut app, _, _) = app_with_desktop();
        let (_, _, anchor) = manage_window(&mut app, 10);
        app.state.world.tree.node_mut(anchor).presel = Some(Presel {
            split_dir: Direction::West,
            split_ratio: 0.8,
            feedback: None,
        });
        let mut consequence = RuleConsequence::default();
        consequence.split_ratio = 0.25;

        let inserted = manage_window_with(
            &mut app,
            11,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1000,
        )
        .unwrap()
        .2;
        let parent = app.state.world.tree.node(inserted).parent.unwrap();
        assert_eq!(
            app.state.world.tree.node(parent).split_type,
            SplitType::Vertical
        );
        assert!((app.state.world.tree.node(parent).split_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(
            app.state.world.tree.node(parent).first_child,
            Some(inserted)
        );
        assert_eq!(app.state.world.tree.node(parent).second_child, Some(anchor));
    }

    #[test]
    fn single_monocle_tracks_the_transition_between_one_and_two_clients() {
        let (mut app, monitor, desktop) = app_with_desktop();
        app.state.settings.single_monocle = true;
        app.state.world.desktop_mut(desktop).layout = Layout::Monocle;
        app.state.world.desktop_mut(desktop).user_layout = Layout::Tiled;

        let (_, _, first) = manage_window(&mut app, 10);
        assert_eq!(app.state.world.desktop(desktop).layout, Layout::Monocle);
        let (_, _, second) = manage_window(&mut app, 11);
        assert_eq!(app.state.world.desktop(desktop).layout, Layout::Tiled);

        app.state.pending_effects.clear();
        assert!(app.forget_window(11).is_some());
        assert_eq!(app.state.world.desktop(desktop).layout, Layout::Monocle);
        assert_eq!(app.state.world.desktop(desktop).tree.focus, Some(first));
        assert!(app.state.pending_effects.iter().any(|effect| {
            matches!(
                effect,
                CommandEffect::Focus {
                    monitor: effect_monitor,
                    desktop: effect_desktop,
                    node: Some(effect_node),
                    ..
                } if *effect_monitor == monitor && *effect_desktop == desktop && *effect_node == first
            )
        }));
        assert!(!app.state.world.tree.is_live(second));
    }

    #[test]
    fn closing_the_focused_node_uses_history_then_the_first_focusable_leaf() {
        let (mut app, monitor, desktop) = app_with_desktop();
        let (_, _, first) = manage_window(&mut app, 10);
        let (_, _, second) = manage_window(&mut app, 11);
        let (_, _, third) = manage_window(&mut app, 12);
        assert!(CommandHandler::new(&mut app.state).focus_location(
            crate::query::Coordinates::node(monitor, desktop, first),
            false,
        ));

        app.state.pending_effects.clear();
        assert!(app.forget_window(10).is_some());
        assert_eq!(app.state.world.desktop(desktop).tree.focus, Some(third));

        app.state.history.clear();
        app.state.world.desktop_mut(desktop).tree.focus = Some(second);
        app.state.pending_effects.clear();
        assert!(app.forget_window(11).is_some());
        assert_eq!(app.state.world.desktop(desktop).tree.focus, Some(third));
        assert!(app.state.pending_effects.iter().any(|effect| {
            matches!(effect, CommandEffect::Focus { node: Some(node), .. } if *node == third)
        }));
    }

    #[test]
    fn initial_focus_policy_honors_zero_stale_wraparound_and_startup_time() {
        let (mut app, _, _) = app_with_desktop();
        app.last_user_time = Some(100);
        assert!(!app.initial_focus_allowed(&ClientInitial {
            user_time: Some(0),
            ..ClientInitial::default()
        }));
        assert!(!app.initial_focus_allowed(&ClientInitial {
            user_time: Some(99),
            ..ClientInitial::default()
        }));
        assert!(app.initial_focus_allowed(&ClientInitial {
            user_time: Some(100),
            ..ClientInitial::default()
        }));
        assert!(app.initial_focus_allowed(&ClientInitial {
            user_time: Some(101),
            ..ClientInitial::default()
        }));

        app.last_user_time = Some(u32::MAX - 1);
        assert!(app.initial_focus_allowed(&ClientInitial {
            user_time: Some(2),
            ..ClientInitial::default()
        }));
        app.startup.ingest(1, true, b"new: ID=launch_TIME55\0");
        app.last_user_time = Some(50);
        assert!(app.initial_focus_allowed(&ClientInitial {
            startup_id: Some("launch_TIME55".to_owned()),
            ..ClientInitial::default()
        }));
    }

    #[test]
    fn initial_hidden_and_fullscreen_clients_are_vacant() {
        let (mut app, _, _) = app_with_desktop();
        let mut consequence = RuleConsequence::default();
        consequence.hidden = true;
        let hidden = manage_window_with(
            &mut app,
            20,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1000,
        )
        .unwrap()
        .2;
        assert!(app.state.world.tree.node(hidden).vacant);

        let (mut app, _, _) = app_with_desktop();
        let mut consequence = RuleConsequence::default();
        consequence.state = Some(ClientState::Fullscreen);
        let fullscreen = manage_window_with(
            &mut app,
            21,
            &consequence,
            Rectangle::default(),
            SizeHints::default(),
            1000,
        )
        .unwrap()
        .2;
        assert!(app.state.world.tree.node(fullscreen).vacant);
    }

    #[test]
    fn rule_target_descriptors_use_node_desktop_monitor_priority() {
        let (mut app, first_monitor, first_desktop) = app_with_desktop();
        let settings = app.state.settings.clone();
        let second_monitor = app.state.world.create_monitor(
            3,
            Some("other"),
            Rectangle::new(100, 0, 100, 80),
            &settings,
        );
        let second_desktop = app.state.world.create_desktop(4, Some("II"), &settings);
        assert!(app.state.world.add_desktop(second_monitor, second_desktop));

        let mut consequence = RuleConsequence::default();
        consequence.monitor_desc = "other".into();
        consequence.focus = false;
        let monitor_target = manage_window_with(
            &mut app,
            20,
            &consequence,
            Rectangle::new(1, 1, 10, 10),
            SizeHints::default(),
            2000,
        )
        .unwrap();
        assert_eq!(monitor_target.0, second_monitor);
        assert_eq!(monitor_target.1, second_desktop);

        consequence.desktop_desc = "I".into();
        let desktop_target = manage_window_with(
            &mut app,
            21,
            &consequence,
            Rectangle::new(1, 1, 10, 10),
            SizeHints::default(),
            2001,
        )
        .unwrap();
        assert_eq!(desktop_target.0, first_monitor);
        assert_eq!(desktop_target.1, first_desktop);

        consequence.node_desc = "0x00000014".into();
        let node_target = manage_window_with(
            &mut app,
            22,
            &consequence,
            Rectangle::new(1, 1, 10, 10),
            SizeHints::default(),
            2002,
        )
        .unwrap();
        assert_eq!(node_target.0, second_monitor);
        assert_eq!(node_target.1, second_desktop);
        assert_eq!(
            app.state.world.tree.node(monitor_target.2).parent,
            app.state.world.tree.node(node_target.2).parent,
        );
    }
}
