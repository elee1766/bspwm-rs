use std::collections::{HashMap, HashSet};

use crate::history::History;
use crate::rule::RuleList;
use crate::settings::Settings;
use crate::tree::NodeId;
use crate::types::Rectangle;
use crate::world::{DesktopId, MonitorId, World};

use bspwm_xstack::StackMirror;

/// The focus-policy knobs one focus operation honours.
///
/// Pointer- and focus-driven corrections have to run without some of them, and
/// they say so by passing a suppressed policy down the focus path. The flags
/// are values rather than temporary writes to [`DaemonState`] because
/// `settings.pointer_follows_*` is persisted through `query_state` and would
/// survive a `--restart` if a suppressed focus never restored it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusPolicy {
    pub pointer_follows_focus: bool,
    pub pointer_follows_monitor: bool,
    pub auto_raise: bool,
}

impl FocusPolicy {
    /// The policy the user configured.
    #[must_use]
    pub const fn configured(state: &DaemonState) -> Self {
        Self {
            pointer_follows_focus: state.settings.pointer_follows_focus,
            pointer_follows_monitor: state.settings.pointer_follows_monitor,
            auto_raise: state.auto_raise,
        }
    }

    /// The configured policy without the knobs a corrective focus must not
    /// honour. `pointer_follows_focus` is always suppressed, matching the
    /// upstream event handlers this stands in for.
    #[must_use]
    pub const fn suppressed(
        state: &DaemonState,
        suppress_monitor: bool,
        suppress_auto_raise: bool,
    ) -> Self {
        Self {
            pointer_follows_focus: false,
            pointer_follows_monitor: !suppress_monitor && state.settings.pointer_follows_monitor,
            auto_raise: !suppress_auto_raise && state.auto_raise,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandEffect {
    Arrange {
        monitor: MonitorId,
        desktop: DesktopId,
    },
    Restack {
        node: NodeId,
        auto_raise: bool,
    },
    SyncEwmh,
    SetWindowVisibility {
        node: NodeId,
        visible: bool,
    },
    SetDesktopVisibility {
        desktop: DesktopId,
        visible: bool,
        preserve_sticky: bool,
    },
    CreateMonitorRoot {
        monitor: MonitorId,
    },
    UpdateMonitorRoot {
        monitor: MonitorId,
    },
    /// Destroys a monitor's input-only root window.
    ///
    /// This carries the window rather than the [`MonitorId`] because it is
    /// queued by `monitor --remove`, which frees the monitor before the effect
    /// ever runs.
    DestroyMonitorRoot {
        root: u32,
    },
    RegrabButtons,
    RefreshColors,
    RefreshBorders,
    RefreshMonitors,
    RefreshFocusFollowsPointer,
    RefreshEwmhAllowedActions,
    Focus {
        monitor: MonitorId,
        previous_monitor: Option<MonitorId>,
        desktop: DesktopId,
        previous_desktop: Option<DesktopId>,
        node: Option<NodeId>,
        previous_node: Option<NodeId>,
        activate: bool,
        auto_raise: bool,
    },
    MoveResize {
        node: NodeId,
        rectangle: Rectangle,
    },
    WarpPointer {
        rectangle: Rectangle,
    },
    Broadcast {
        mask: crate::types::SubscriberMask,
        status: String,
        report: bool,
    },
    AdoptOrphans,
    Close {
        node: NodeId,
    },
    Kill {
        node: NodeId,
    },
    SyncWindowState {
        node: NodeId,
    },
    SyncPreselFeedback {
        node: NodeId,
        include_receptacle: bool,
    },
    LoadState {
        restored: Box<crate::restore::RestoredState>,
    },
}

impl CommandEffect {
    /// Whether every arena id this effect names still resolves.
    ///
    /// The queue has no upstream counterpart -- upstream performs this work
    /// inline -- so an effect can outlive its subject: one command in a message
    /// queues a restack for a node that a later command in the same message
    /// closes, or a monitor's desktops go away before its arrange runs. While
    /// the arenas never freed anything, such an effect silently found a blanked
    /// node and did nothing; now that slots are freed it has to be dropped
    /// explicitly, which reproduces exactly that no-op.
    #[must_use]
    pub fn references_live(&self, state: &DaemonState) -> bool {
        let world = &state.world;
        let node = |node: NodeId| world.tree.is_live(node);
        let desktop = |desktop: DesktopId| world.get_desktop(desktop).is_some();
        let monitor = |monitor: MonitorId| world.get_monitor(monitor).is_some();
        match self {
            Self::Arrange {
                monitor: id,
                desktop: on,
            } => monitor(*id) && desktop(*on),
            Self::Restack { node: id, .. }
            | Self::SetWindowVisibility { node: id, .. }
            | Self::MoveResize { node: id, .. }
            | Self::Close { node: id }
            | Self::Kill { node: id }
            | Self::SyncWindowState { node: id }
            | Self::SyncPreselFeedback { node: id, .. } => node(*id),
            Self::SetDesktopVisibility { desktop: id, .. } => desktop(*id),
            Self::CreateMonitorRoot { monitor: id } | Self::UpdateMonitorRoot { monitor: id } => {
                monitor(*id)
            }
            Self::Focus {
                monitor: id,
                desktop: on,
                node: at,
                ..
            } => monitor(*id) && desktop(*on) && at.is_none_or(node),
            Self::DestroyMonitorRoot { .. }
            | Self::SyncEwmh
            | Self::RegrabButtons
            | Self::RefreshColors
            | Self::RefreshBorders
            | Self::RefreshMonitors
            | Self::RefreshFocusFollowsPointer
            | Self::RefreshEwmhAllowedActions
            | Self::WarpPointer { .. }
            | Self::Broadcast { .. }
            | Self::AdoptOrphans
            | Self::LoadState { .. } => true,
        }
    }
}

/// The in-memory state owned by the window-manager process.
///
/// Subscriber streams remain in `subscribe::Subscribers<W>` because their
/// concrete writer type is an I/O concern rather than daemon state.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct DaemonState {
    pub world: World,
    pub settings: Settings,
    pub history: History<MonitorId, DesktopId>,
    pub stacking_order: StackMirror,
    pub rules: RuleList,
    pub clients_count: u32,
    pub auto_raise: bool,
    pub sticky_still: bool,
    pub hide_sticky: bool,
    pub record_history: bool,
    pub running: bool,
    pub restart: bool,
    pub exit_status: i32,
    pub randr_base: u8,
    /// X/EWMH work required to complete accepted pure command mutations.
    pub pending_effects: Vec<CommandEffect>,
}

impl DaemonState {
    pub fn set_record_history(&mut self, record_history: bool) {
        self.record_history = record_history;
        self.history.recording = record_history;
    }

    pub fn apply_restored(&mut self, restored: crate::restore::RestoredState) {
        self.world = restored.world;
        self.history = restored.history;
        self.history.recording = self.record_history;
        self.stacking_order = restored.stacking_order;
        self.clients_count = restored.clients_count;
    }

    /// Drops history and stacking references to nodes the arena has freed.
    ///
    /// Structural tree operations free nodes their caller never named: `unlink`
    /// collapses the parent branch, `insert` consumes a bare receptacle. Since
    /// [`crate::tree::Tree`] frees the slot outright, an id holder that still
    /// named one of those nodes would panic on its next lookup, so the ids are
    /// queued by the tree and swept out here. Upstream does the same purge
    /// inline, at each `free()`.
    pub fn forget_retired_nodes(&mut self) {
        if !self.world.tree.has_retired_nodes() {
            return;
        }
        let retired = self.world.tree.take_retired_nodes();
        self.history.forget_nodes(&retired);
        // StackMirror stores XIDs, not NodeIds. Client windows are removed from
        // the mirror at their specific removal sites (forget_window,
        // remove_subtree equivalents). Retired nodes here are branches and
        // receptacles that were never in the stacking mirror.
    }

    /// Checks invariants spanning the independently implemented state stores.
    #[allow(clippy::missing_errors_doc)]
    pub fn validate(&self) -> Result<(), &'static str> {
        self.world.validate()?;

        // A pending sweep means some caller freed nodes and did not run
        // `forget_retired_nodes`, which is how a stale id survives at all.
        if self.world.tree.has_retired_nodes() {
            return Err("retired nodes have not been swept out of the id holders");
        }

        if self.history.recording != self.record_history {
            return Err("history recording flag differs from record_history");
        }

        let monitors: HashSet<_> = self.world.monitor_order().iter().copied().collect();
        if monitors.len() != self.world.monitor_order().len() {
            return Err("monitor order contains a duplicate monitor");
        }
        if self
            .world
            .focused_monitor
            .is_some_and(|monitor| !monitors.contains(&monitor))
        {
            return Err("focused monitor is not in monitor order");
        }
        if self
            .world
            .primary_monitor
            .is_some_and(|monitor| !monitors.contains(&monitor))
        {
            return Err("primary monitor is not in monitor order");
        }

        let mut desktop_monitors = HashMap::new();
        let mut desktop_nodes = HashMap::new();
        let mut all_nodes = HashSet::new();
        let mut client_nodes = HashSet::new();
        let mut clients_count = 0_u32;

        for (monitor_id, desktop_id) in self.world.desktops() {
            if desktop_monitors.insert(desktop_id, monitor_id).is_some() {
                return Err("desktop belongs to multiple monitors");
            }

            let mut nodes = HashSet::new();
            if let Some(root) = self.world.desktop(desktop_id).tree.root {
                collect_tree(
                    &self.world,
                    root,
                    &mut nodes,
                    &mut client_nodes,
                    &mut clients_count,
                )?;
                if nodes.iter().any(|node| !all_nodes.insert(*node)) {
                    return Err("node belongs to multiple desktops");
                }
            }
            desktop_nodes.insert(desktop_id, nodes);
        }

        if clients_count != self.clients_count {
            return Err("clients_count differs from the number of managed clients");
        }

        for entry in self.history.entries() {
            let location = entry.location;
            if desktop_monitors.get(&location.desktop) != Some(&location.monitor) {
                return Err("history location does not belong to its monitor");
            }
            if location.node.is_some_and(|node| {
                !desktop_nodes
                    .get(&location.desktop)
                    .is_some_and(|nodes| nodes.contains(&node))
            }) {
                return Err("history node does not belong to its desktop");
            }
        }

        let mut stacked = HashSet::new();
        let client_xids: HashSet<u32> = client_nodes
            .iter()
            .map(|node| self.world.tree.node(*node).external_id)
            .collect();
        for xid in self.stacking_order.windows() {
            if !stacked.insert(xid) {
                return Err("stacking order contains a duplicate window");
            }
            if !client_xids.contains(&xid) {
                return Err("stacking order contains a window without a managed client");
            }
        }

        Ok(())
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            world: World::default(),
            settings: Settings::default(),
            history: History::default(),
            stacking_order: StackMirror::new(),
            rules: RuleList::default(),
            clients_count: 0,
            auto_raise: true,
            sticky_still: true,
            hide_sticky: true,
            record_history: true,
            running: false,
            restart: false,
            exit_status: 0,
            randr_base: 0,
            pending_effects: Vec::new(),
        }
    }
}

/// Walks `root` in preorder, rejecting the first node reached twice -- which is
/// also what stops the walk on a cyclic tree.
fn collect_tree(
    world: &World,
    root: NodeId,
    nodes: &mut HashSet<NodeId>,
    client_nodes: &mut HashSet<NodeId>,
    clients_count: &mut u32,
) -> Result<(), &'static str> {
    for node_id in world.tree.preorder(root) {
        if !nodes.insert(node_id) {
            return Err("desktop tree contains a duplicate node");
        }
        if world.tree.node(node_id).client.is_some() {
            client_nodes.insert(node_id);
            *clients_count = clients_count
                .checked_add(1)
                .ok_or("managed client count exceeds u32")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Client;
    use crate::types::Rectangle;

    /// No-op backend for unit tests that don't talk to X.
    struct NoopBackend;
    impl bspwm_xstack::StackBackend for NoopBackend {
        type Error = ();
        fn stack_above(&mut self, _window: u32, _sibling: u32) -> Result<(), ()> {
            Ok(())
        }
        fn stack_below(&mut self, _window: u32, _sibling: u32) -> Result<(), ()> {
            Ok(())
        }
    }

    #[test]
    fn init_matches_upstream_defaults() {
        let state = DaemonState::default();
        assert_eq!(state.clients_count, 0);
        assert!(state.auto_raise);
        assert!(state.sticky_still);
        assert!(state.hide_sticky);
        assert!(state.record_history);
        assert!(state.history.recording);
        assert!(!state.running);
        assert!(!state.restart);
        assert_eq!(state.exit_status, 0);
        assert_eq!(state.randr_base, 0);
        assert_eq!(state.validate(), Ok(()));
    }

    #[test]
    fn validation_coordinates_world_history_stack_and_client_count() {
        let mut state = DaemonState::default();
        let monitor =
            state
                .world
                .create_monitor(1, None, Rectangle::new(0, 0, 1920, 1080), &state.settings);
        let desktop = state.world.create_desktop(2, None, &state.settings);
        assert!(state.world.add_desktop(monitor, desktop));

        let node = state.world.tree.add_node(3, state.settings.split_ratio);
        state.world.tree.node_mut(node).client = Some(Client::from_settings(&state.settings));
        state.world.desktop_mut(desktop).tree.root = Some(node);
        state.clients_count = 1;
        state.history.add(
            crate::history::Coordinates {
                monitor,
                desktop,
                node: Some(node),
            },
            true,
        );
        let level = crate::stack::stack_level(state.world.tree.node(node).client.as_ref().unwrap());
        let _ = state.stacking_order.insert(&mut NoopBackend, 3, level);

        assert_eq!(state.validate(), Ok(()));
        state.clients_count = 0;
        assert_eq!(
            state.validate(),
            Err("clients_count differs from the number of managed clients")
        );
    }

    #[test]
    fn recording_setter_keeps_history_and_global_flag_in_sync() {
        let mut state = DaemonState::default();
        state.set_record_history(false);
        assert!(!state.record_history);
        assert!(!state.history.recording);
        assert_eq!(state.validate(), Ok(()));

        state.history.recording = true;
        assert_eq!(
            state.validate(),
            Err("history recording flag differs from record_history")
        );
    }
}
