use slotmap::SlotMap;

use crate::geometry::rect_cmp;
use crate::settings::Settings;
use crate::tree::{NodeId, StructuralError, Tree, TreeState, arena_eq};
use crate::types::{Layout, Padding, Rectangle, SMALEN};

slotmap::new_key_type! {
    /// A generational handle into [`World`]'s monitor arena.
    pub struct MonitorId;
}

slotmap::new_key_type! {
    /// A generational handle into [`World`]'s desktop arena.
    pub struct DesktopId;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Desktop {
    pub external_id: u32,
    pub name: String,
    pub layout: Layout,
    pub user_layout: Layout,
    pub tree: TreeState,
    pub padding: Padding,
    pub window_gap: i32,
    pub border_width: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub external_id: u32,
    pub randr_id: u32,
    /// The input-only X window covering this monitor. It is runtime-only state.
    pub root_id: Option<u32>,
    pub name: String,
    pub rectangle: Rectangle,
    pub desktops: Vec<DesktopId>,
    pub active_desktop: Option<DesktopId>,
    pub padding: Padding,
    pub sticky_count: u32,
    pub window_gap: i32,
    pub border_width: u32,
    pub wired: bool,
}

#[derive(Clone, Debug, Default)]
pub struct World {
    pub tree: Tree,
    monitors: SlotMap<MonitorId, Monitor>,
    desktops: SlotMap<DesktopId, Desktop>,
    monitor_order: Vec<MonitorId>,
    pub focused_monitor: Option<MonitorId>,
    pub primary_monitor: Option<MonitorId>,
}

/// A partially qualified location in the monitor, desktop, and node hierarchy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Coordinates {
    pub monitor: Option<MonitorId>,
    pub desktop: Option<DesktopId>,
    pub node: Option<NodeId>,
}

impl Coordinates {
    /// A fully qualified node location.
    #[must_use]
    pub const fn node(monitor: MonitorId, desktop: DesktopId, node: NodeId) -> Self {
        Self {
            monitor: Some(monitor),
            desktop: Some(desktop),
            node: Some(node),
        }
    }

    /// A desktop location, with no node selected.
    #[must_use]
    pub const fn desktop(monitor: MonitorId, desktop: DesktopId) -> Self {
        Self {
            monitor: Some(monitor),
            desktop: Some(desktop),
            node: None,
        }
    }

    /// A location inside `desktop`, selecting `node` when there is one.
    #[must_use]
    pub const fn in_desktop(monitor: MonitorId, desktop: DesktopId, node: Option<NodeId>) -> Self {
        Self {
            monitor: Some(monitor),
            desktop: Some(desktop),
            node,
        }
    }

    /// A monitor location, with neither desktop nor node selected.
    #[must_use]
    pub const fn monitor(monitor: MonitorId) -> Self {
        Self {
            monitor: Some(monitor),
            desktop: None,
            node: None,
        }
    }
}

impl PartialEq for World {
    fn eq(&self, other: &Self) -> bool {
        self.tree == other.tree
            && self.monitor_order == other.monitor_order
            && self.focused_monitor == other.focused_monitor
            && self.primary_monitor == other.primary_monitor
            && arena_eq(&self.monitors, &other.monitors)
            && arena_eq(&self.desktops, &other.desktops)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeMove {
    pub source_monitor: MonitorId,
    pub source_desktop: DesktopId,
    pub destination_monitor: MonitorId,
    pub destination_desktop: DesktopId,
    pub node: NodeId,
}

/// The aftermath of removing a monitor or a desktop.
///
/// `desktops` carries each removed desktop's external id beside its key
/// because the keys are already freed by the time the caller reads this: the
/// only thing left to do with the key is to purge stores that hold it.
///
/// `roots` are the trees the removal orphaned. They are still in the arena so
/// the caller can count their clients and drop them from the stacking order;
/// the caller must then hand them to [`Tree::destroy_subtree`], or they leak.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Removal {
    pub monitor: MonitorId,
    pub desktops: Vec<(DesktopId, u32)>,
    pub roots: Vec<NodeId>,
}

impl World {
    #[must_use]
    pub fn next_external_id(&self) -> u32 {
        self.next_external_id_in(&self.tree)
    }

    /// Allocates an external id that is free in `tree` and in this world's
    /// monitors and desktops.
    ///
    /// Insertions build their result in a scratch arena so that a rejected
    /// insert cannot leak nodes, and must allocate against that arena rather
    /// than against the still-published [`World::tree`].
    fn next_external_id_in(&self, tree: &Tree) -> u32 {
        // Freeing arena slots keeps this set to the entities that are actually
        // alive, so the scan is over the live world rather than over everything
        // the session ever created. The occupied ids are gathered once instead
        // of re-scanned per candidate, which is what made this quadratic.
        let mut occupied: Vec<u32> = self
            .monitors
            .values()
            .map(|monitor| monitor.external_id)
            .chain(self.desktops.values().map(|desktop| desktop.external_id))
            .chain(tree.external_ids())
            .filter(|id| *id >= 0xF000_0000)
            .collect();
        occupied.sort_unstable();
        let mut candidate = 0xF000_0000;
        for id in occupied {
            match id.cmp(&candidate) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => candidate = candidate.wrapping_add(1),
                std::cmp::Ordering::Greater => break,
            }
        }
        candidate
    }

    #[must_use]
    pub fn create_monitor(
        &mut self,
        external_id: u32,
        name: Option<&str>,
        rectangle: Rectangle,
        settings: &Settings,
    ) -> MonitorId {
        let id = self.monitors.insert(Monitor {
            external_id,
            randr_id: 0,
            root_id: None,
            name: bounded_name(name.unwrap_or("MONITOR")),
            rectangle,
            desktops: Vec::new(),
            active_desktop: None,
            padding: settings.padding,
            sticky_count: 0,
            window_gap: settings.window_gap,
            border_width: settings.border_width,
            wired: true,
        });
        let insertion = self
            .monitor_order
            .iter()
            .position(|monitor| rect_cmp(rectangle, self.monitor(*monitor).rectangle).is_le())
            .unwrap_or(self.monitor_order.len());
        self.monitor_order.insert(insertion, id);
        self.focused_monitor.get_or_insert(id);
        id
    }

    #[must_use]
    pub fn create_desktop(
        &mut self,
        external_id: u32,
        name: Option<&str>,
        settings: &Settings,
    ) -> DesktopId {
        self.desktops.insert(Desktop {
            external_id,
            name: bounded_name(name.unwrap_or("Desktop")),
            layout: if settings.single_monocle {
                Layout::Monocle
            } else {
                Layout::Tiled
            },
            user_layout: Layout::Tiled,
            tree: TreeState::default(),
            padding: settings.padding,
            window_gap: settings.window_gap,
            border_width: settings.border_width,
        })
    }

    /// # Panics
    ///
    /// Panics when `id` names a removed monitor. Use [`World::get_monitor`]
    /// where the id may legitimately be stale.
    #[must_use]
    pub fn monitor(&self, id: MonitorId) -> &Monitor {
        self.monitors
            .get(id)
            .expect("monitor id outlived its monitor")
    }

    /// # Panics
    ///
    /// Panics when `id` names a removed monitor.
    #[must_use]
    pub fn monitor_mut(&mut self, id: MonitorId) -> &mut Monitor {
        self.monitors
            .get_mut(id)
            .expect("monitor id outlived its monitor")
    }

    #[must_use]
    pub fn get_monitor(&self, id: MonitorId) -> Option<&Monitor> {
        self.monitors.get(id)
    }

    /// # Panics
    ///
    /// Panics when `id` names a removed desktop. Use [`World::get_desktop`]
    /// where the id may legitimately be stale.
    #[must_use]
    pub fn desktop(&self, id: DesktopId) -> &Desktop {
        self.desktops
            .get(id)
            .expect("desktop id outlived its desktop")
    }

    /// # Panics
    ///
    /// Panics when `id` names a removed desktop.
    #[must_use]
    pub fn desktop_mut(&mut self, id: DesktopId) -> &mut Desktop {
        self.desktops
            .get_mut(id)
            .expect("desktop id outlived its desktop")
    }

    #[must_use]
    pub fn get_desktop(&self, id: DesktopId) -> Option<&Desktop> {
        self.desktops.get(id)
    }

    /// The number of live monitors, including any not in [`World::monitor_order`].
    #[must_use]
    pub fn monitor_count(&self) -> usize {
        self.monitors.len()
    }

    /// The number of live desktops, including any not attached to a monitor.
    #[must_use]
    pub fn desktop_count(&self) -> usize {
        self.desktops.len()
    }

    #[must_use]
    pub fn monitor_order(&self) -> &[MonitorId] {
        &self.monitor_order
    }

    /// Every `(monitor, desktop)` pair, in monitor order then desktop order.
    pub fn desktops(&self) -> impl Iterator<Item = (MonitorId, DesktopId)> + '_ {
        self.monitor_order.iter().flat_map(move |monitor| {
            self.monitor(*monitor)
                .desktops
                .iter()
                .map(move |desktop| (*monitor, *desktop))
        })
    }

    /// Like [`World::desktops`], restricted to desktops that have a tree.
    pub fn roots(&self) -> impl Iterator<Item = (MonitorId, DesktopId, NodeId)> + '_ {
        self.desktops().filter_map(move |(monitor, desktop)| {
            Some((monitor, desktop, self.desktop(desktop).tree.root?))
        })
    }

    #[must_use]
    pub fn closest_monitor(&self, monitor: MonitorId, next: bool) -> Option<MonitorId> {
        if self.monitor_order.len() < 2 {
            return None;
        }
        let index = self
            .monitor_order
            .iter()
            .position(|candidate| *candidate == monitor)?;
        Some(self.monitor_order[wrapped_index(index, self.monitor_order.len(), next)])
    }

    pub fn add_desktop(&mut self, monitor: MonitorId, desktop: DesktopId) -> bool {
        if self.desktop_monitor(desktop).is_some() {
            return false;
        }
        let (gap, border) = {
            let monitor = self.monitor(monitor);
            (monitor.window_gap, monitor.border_width)
        };
        self.desktop_mut(desktop).window_gap = gap;
        self.desktop_mut(desktop).border_width = border;
        let monitor = self.monitor_mut(monitor);
        monitor.desktops.push(desktop);
        monitor.active_desktop.get_or_insert(desktop);
        true
    }

    pub fn activate_desktop(&mut self, monitor: MonitorId, desktop: DesktopId) -> bool {
        if !self.monitor(monitor).desktops.contains(&desktop)
            || self.monitor(monitor).active_desktop == Some(desktop)
        {
            return false;
        }
        self.monitor_mut(monitor).active_desktop = Some(desktop);
        true
    }

    pub fn transfer_desktop(&mut self, desktop: DesktopId, destination: MonitorId) -> bool {
        let Some(source) = self.desktop_monitor(desktop) else {
            return false;
        };
        if source == destination {
            return false;
        }
        let source_monitor = self.monitor_mut(source);
        source_monitor
            .desktops
            .retain(|candidate| *candidate != desktop);
        if source_monitor.active_desktop == Some(desktop) {
            source_monitor.active_desktop = source_monitor.desktops.first().copied();
        }
        let destination_monitor = self.monitor_mut(destination);
        destination_monitor.desktops.push(desktop);
        destination_monitor.active_desktop.get_or_insert(desktop);
        true
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn bubble_desktop(&mut self, desktop: DesktopId, next: bool) -> bool {
        let Some(monitor) = self.desktop_monitor(desktop) else {
            return false;
        };
        let desktops = &mut self.monitor_mut(monitor).desktops;
        if desktops.len() < 2 {
            return false;
        }
        let index = desktops
            .iter()
            .position(|candidate| *candidate == desktop)
            .unwrap();
        let destination = wrapped_index(index, desktops.len(), next);
        desktops.swap(index, destination);
        true
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn swap_desktops(&mut self, first: DesktopId, second: DesktopId) -> bool {
        if first == second {
            return false;
        }
        let (Some(first_monitor), Some(second_monitor)) =
            (self.desktop_monitor(first), self.desktop_monitor(second))
        else {
            return false;
        };
        let first_index = self
            .monitor(first_monitor)
            .desktops
            .iter()
            .position(|id| *id == first)
            .unwrap();
        let second_index = self
            .monitor(second_monitor)
            .desktops
            .iter()
            .position(|id| *id == second)
            .unwrap();
        let first_was_active = self.monitor_mut(first_monitor).active_desktop == Some(first);
        let second_was_active = self.monitor_mut(second_monitor).active_desktop == Some(second);
        self.monitor_mut(first_monitor).desktops[first_index] = second;
        self.monitor_mut(second_monitor).desktops[second_index] = first;
        if first_was_active {
            self.monitor_mut(first_monitor).active_desktop = Some(second);
        }
        if second_was_active {
            self.monitor_mut(second_monitor).active_desktop = Some(first);
        }
        true
    }

    pub fn reorder_desktops(&mut self, monitor: MonitorId, requested: &[DesktopId]) {
        reorder(&mut self.monitor_mut(monitor).desktops, requested);
    }

    pub fn swap_monitors(&mut self, first: MonitorId, second: MonitorId) -> bool {
        if first == second {
            return false;
        }
        let Some(first_index) = self.monitor_order.iter().position(|id| *id == first) else {
            return false;
        };
        let Some(second_index) = self.monitor_order.iter().position(|id| *id == second) else {
            return false;
        };
        self.monitor_order.swap(first_index, second_index);
        true
    }

    pub fn reorder_monitors(&mut self, requested: &[MonitorId]) {
        reorder(&mut self.monitor_order, requested);
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn update_monitor_rectangle(&mut self, monitor: MonitorId, rectangle: Rectangle) -> bool {
        if self.monitor(monitor).rectangle == rectangle {
            return false;
        }
        self.monitor_mut(monitor).rectangle = rectangle;
        let mut position = self
            .monitor_order
            .iter()
            .position(|candidate| *candidate == monitor)
            .unwrap();
        while position > 0
            && rect_cmp(
                rectangle,
                self.monitor(self.monitor_order[position - 1]).rectangle,
            )
            .is_lt()
        {
            self.monitor_order.swap(position, position - 1);
            position -= 1;
        }
        while position + 1 < self.monitor_order.len()
            && rect_cmp(
                rectangle,
                self.monitor(self.monitor_order[position + 1]).rectangle,
            )
            .is_gt()
        {
            self.monitor_order.swap(position, position + 1);
            position += 1;
        }
        true
    }

    /// The desktop whose tree holds `node`, if any.
    ///
    /// A retired node is in no tree, so this answers `None` for it rather than
    /// panicking: it is a membership question, and "gone" is a valid answer.
    #[must_use]
    pub fn node_desktop(&self, node: NodeId) -> Option<DesktopId> {
        if !self.tree.is_live(node) {
            return None;
        }
        self.roots()
            .find_map(|(_, desktop, root)| self.tree.is_descendant(node, root).then_some(desktop))
    }

    /// Allocates the branch node an insertion next to `anchor` needs, if any.
    ///
    /// A leaf receptacle without a preselection is replaced in place by
    /// [`Tree::insert`], so it needs no branch to split into.
    fn branch_for(&self, tree: &mut Tree, anchor: NodeId, split_ratio: f64) -> Option<NodeId> {
        // The unlink half of a transfer can collapse the anchor's parent, or
        // the anchor itself when it was the collapsing branch. Probing rather
        // than indexing keeps that case a rejected insert instead of a panic.
        let item = tree.get(anchor)?;
        let needs_branch = item.client.is_some() || item.presel.is_some() || !tree.is_leaf(anchor);
        needs_branch.then(|| {
            let external_id = self.next_external_id_in(tree);
            tree.add_node(external_id, split_ratio)
        })
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn swap_nodes(&mut self, first: NodeId, second: NodeId) -> Result<(), StructuralError> {
        let first_desktop = self
            .node_desktop(first)
            .ok_or(StructuralError::NotAttached)?;
        let second_desktop = self
            .node_desktop(second)
            .ok_or(StructuralError::NotAttached)?;
        if first_desktop == second_desktop {
            let mut state = self.desktop(first_desktop).tree;
            self.tree.swap_within(&mut state, first, second)?;
            self.desktop_mut(first_desktop).tree = state;
        } else {
            let mut first_state = self.desktop(first_desktop).tree;
            let mut second_state = self.desktop(second_desktop).tree;
            self.tree
                .swap_between(&mut first_state, first, &mut second_state, second)?;
            self.desktop_mut(first_desktop).tree = first_state;
            self.desktop_mut(second_desktop).tree = second_state;
        }
        Ok(())
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn transfer_node(
        &mut self,
        node: NodeId,
        destination: DesktopId,
        anchor: Option<NodeId>,
        split_ratio: f64,
    ) -> Result<NodeMove, StructuralError> {
        let source = self
            .node_desktop(node)
            .ok_or(StructuralError::NotAttached)?;
        if anchor == Some(node)
            || anchor.is_some_and(|anchor| self.tree.is_descendant(anchor, node))
        {
            return Err(StructuralError::InvalidAnchor);
        }
        if anchor.is_some_and(|anchor| self.node_desktop(anchor) != Some(destination)) {
            return Err(StructuralError::InvalidAnchor);
        }
        let source_monitor = self
            .desktop_monitor(source)
            .ok_or(StructuralError::NotAttached)?;
        let destination_monitor = self
            .desktop_monitor(destination)
            .ok_or(StructuralError::NotAttached)?;
        // `unlink` and `insert` both restructure the shared arena, so a failure
        // between them would leave `desktops[source].tree.root` naming a retired
        // node. Restructure a scratch copy of the arena and publish it only once
        // both halves have succeeded.
        let mut tree = self.tree.clone();
        let mut source_state = self.desktop(source).tree;
        tree.unlink(&mut source_state, node)?;
        if source_state.focus.is_none() {
            source_state.focus = source_state
                .root
                .and_then(|root| tree.first_focusable_leaf(root));
        }
        let mut destination_state = if source == destination {
            source_state
        } else {
            self.desktop(destination).tree
        };
        let branch = anchor.and_then(|anchor| self.branch_for(&mut tree, anchor, split_ratio));
        tree.insert(
            &mut destination_state,
            node,
            anchor,
            branch,
            crate::tree::ChildPolarity::Second,
        )?;
        self.tree = tree;
        self.desktop_mut(source).tree = if source == destination {
            destination_state
        } else {
            source_state
        };
        self.desktop_mut(destination).tree = destination_state;
        Ok(NodeMove {
            source_monitor,
            source_desktop: source,
            destination_monitor,
            destination_desktop: destination,
            node,
        })
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn insert_receptacle(
        &mut self,
        desktop: DesktopId,
        anchor: Option<NodeId>,
        split_ratio: f64,
    ) -> Result<NodeId, StructuralError> {
        // Allocate into a scratch arena so a rejected insert does not leak the
        // receptacle and branch nodes into the published tree.
        let mut tree = self.tree.clone();
        let external_id = self.next_external_id_in(&tree);
        let receptacle = tree.add_node(external_id, split_ratio);
        let branch = anchor.and_then(|anchor| self.branch_for(&mut tree, anchor, split_ratio));
        let mut state = self.desktop(desktop).tree;
        tree.insert(
            &mut state,
            receptacle,
            anchor,
            branch,
            crate::tree::ChildPolarity::Second,
        )?;
        self.tree = tree;
        self.desktop_mut(desktop).tree = state;
        Ok(receptacle)
    }

    pub fn remove_desktop(&mut self, desktop: DesktopId, split_ratio: f64) -> Option<Removal> {
        let monitor = self.desktop_monitor(desktop)?;
        if self.monitor(monitor).desktops.len() < 2 {
            return None;
        }
        let index = self
            .monitor(monitor)
            .desktops
            .iter()
            .position(|id| *id == desktop)?;
        let fallback = if index == 0 {
            self.monitor(monitor).desktops[1]
        } else {
            self.monitor(monitor).desktops[index - 1]
        };
        let root = self.desktop(desktop).tree.root;
        if let Some(root) = root {
            let anchor = self.desktop(fallback).tree.focus;
            self.transfer_node(root, fallback, anchor, split_ratio)
                .ok()?;
        }
        self.monitor_mut(monitor).desktops.remove(index);
        if self.monitor(monitor).active_desktop == Some(desktop) {
            self.monitor_mut(monitor).active_desktop = Some(fallback);
        }
        // The tree, if any, went to `fallback` above, so nothing is orphaned by
        // freeing the slot. Upstream `free`s the desktop here too.
        let external_id = self.desktops.remove(desktop)?.external_id;
        Some(Removal {
            monitor,
            desktops: vec![(desktop, external_id)],
            roots: Vec::new(),
        })
    }

    pub fn remove_monitor(&mut self, monitor: MonitorId) -> Option<Removal> {
        if self.monitor_order.len() < 2 {
            return None;
        }
        self.remove_monitor_runtime(monitor)
    }

    /// Removes `monitor` and every desktop on it.
    ///
    /// The returned [`Removal::roots`] are still in the tree arena: the caller
    /// needs them to update its own stores, and must then pass each one to
    /// [`Tree::destroy_subtree`].
    pub fn remove_monitor_runtime(&mut self, monitor: MonitorId) -> Option<Removal> {
        let index = self.monitor_order.iter().position(|id| *id == monitor)?;
        let owned = std::mem::take(&mut self.monitor_mut(monitor).desktops);
        let roots: Vec<_> = owned
            .iter()
            .filter_map(|desktop| self.desktop(*desktop).tree.root)
            .collect();
        for root in &roots {
            self.tree.cancel_subtree_presels(*root);
        }
        let mut desktops = Vec::with_capacity(owned.len());
        for desktop in owned {
            let Some(removed) = self.desktops.remove(desktop) else {
                continue;
            };
            desktops.push((desktop, removed.external_id));
        }
        self.monitor_order.remove(index);
        if self.focused_monitor == Some(monitor) {
            self.focused_monitor = self.monitor_order.first().copied();
        }
        if self.primary_monitor == Some(monitor) {
            self.primary_monitor = None;
        }
        self.monitors.remove(monitor);
        Some(Removal {
            monitor,
            desktops,
            roots,
        })
    }

    #[must_use]
    pub fn desktop_monitor(&self, desktop: DesktopId) -> Option<MonitorId> {
        self.desktops()
            .find_map(|(monitor, candidate)| (candidate == desktop).then_some(monitor))
    }

    #[must_use]
    pub fn desktop_is_urgent(&self, desktop: DesktopId) -> bool {
        let Some(root) = self.desktop(desktop).tree.root else {
            return false;
        };
        self.tree.leaves(root).any(|node| {
            self.tree
                .node(node)
                .client
                .as_ref()
                .is_some_and(|client| client.urgent)
        })
    }

    pub fn set_layout(
        &mut self,
        desktop: DesktopId,
        layout: Layout,
        user: bool,
        single_monocle: bool,
    ) -> bool {
        let tiled_count = self
            .desktop(desktop)
            .tree
            .root
            .map_or(0, |root| self.tree.tiled_count(root, true));
        let desktop = self.desktop_mut(desktop);
        let target = if user {
            &mut desktop.user_layout
        } else {
            &mut desktop.layout
        };
        if *target == layout {
            return false;
        }
        *target = layout;
        if !user || !single_monocle || tiled_count > 1 {
            desktop.layout = layout;
        }
        true
    }

    pub fn set_state(
        &mut self,
        desktop: DesktopId,
        node: NodeId,
        state: crate::types::ClientState,
        single_monocle: bool,
    ) -> bool {
        let was_tiled = self
            .tree
            .node(node)
            .client
            .as_ref()
            .is_some_and(|client| client.state.is_tiled());
        if !self.tree.set_state(node, state) {
            return false;
        }
        let Some(is_tiled) = self
            .tree
            .node(node)
            .client
            .as_ref()
            .map(|client| client.state.is_tiled())
        else {
            return false;
        };
        if single_monocle && was_tiled != is_tiled {
            let tiled_count = self
                .desktop(desktop)
                .tree
                .root
                .map_or(0, |root| self.tree.tiled_count(root, true));
            let value = self.desktop_mut(desktop);
            if was_tiled && value.layout != Layout::Monocle && tiled_count <= 1 {
                value.layout = Layout::Monocle;
            } else if !was_tiled && value.layout == Layout::Monocle && tiled_count > 1 {
                value.layout = value.user_layout;
            }
        }
        true
    }

    pub fn rename_monitor(&mut self, id: MonitorId, name: &str) {
        self.monitor_mut(id).name = bounded_name(name);
    }

    pub fn rename_desktop(&mut self, id: DesktopId, name: &str) {
        self.desktop_mut(id).name = bounded_name(name);
    }

    /// Checks the world's internal invariants, including that no stored id
    /// outlived the entity it names and that no arena slot has been orphaned.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub fn validate(&self) -> Result<(), &'static str> {
        for id in &self.monitor_order {
            if !self.monitors.contains_key(*id) {
                return Err("monitor order names a removed monitor");
            }
        }
        if self
            .focused_monitor
            .is_some_and(|id| !self.monitors.contains_key(id))
        {
            return Err("focused monitor was removed");
        }
        if self
            .primary_monitor
            .is_some_and(|id| !self.monitors.contains_key(id))
        {
            return Err("primary monitor was removed");
        }

        let mut seen = std::collections::HashSet::new();
        for monitor in &self.monitor_order {
            let monitor = self.monitor(*monitor);
            for desktop in &monitor.desktops {
                if !self.desktops.contains_key(*desktop) {
                    return Err("monitor names a removed desktop");
                }
                if !seen.insert(*desktop) {
                    return Err("desktop belongs to multiple monitors");
                }
            }
            if monitor
                .active_desktop
                .is_some_and(|active| !monitor.desktops.contains(&active))
            {
                return Err("active desktop does not belong to monitor");
            }
        }

        // Every live node must belong to exactly one desktop tree. An
        // unreachable node is a leaked arena slot, which is precisely what the
        // old `Vec` arena accumulated forever.
        let mut reachable = std::collections::HashSet::new();
        for desktop in self.desktops.values() {
            let Some(root) = desktop.tree.root else {
                if desktop.tree.focus.is_some() {
                    return Err("desktop focus is set on a rootless desktop");
                }
                continue;
            };
            if !self.tree.is_live(root) {
                return Err("desktop root outlived its node");
            }
            self.tree.validate(root)?;
            match desktop.tree.focus {
                Some(focus) if !self.tree.is_live(focus) => {
                    return Err("desktop focus outlived its node");
                }
                Some(focus) if !self.tree.is_descendant(focus, root) => {
                    return Err("desktop focus is outside its tree");
                }
                _ => {}
            }
            if self.tree.node(root).parent.is_some() {
                return Err("desktop root is not the top of its tree");
            }
            for node in self.tree.preorder(root) {
                if !reachable.insert(node) {
                    return Err("node belongs to multiple desktops");
                }
            }
        }
        if reachable.len() != self.tree.len() {
            return Err("tree arena holds nodes no desktop can reach");
        }
        Ok(())
    }
}

fn bounded_name(name: &str) -> String {
    name.chars().take(SMALEN - 1).collect()
}

/// Brings the entries of `requested` that occur in `list` to its front, in the
/// requested order, by swapping each into place. Entries `requested` does not
/// name end up wherever the swaps displaced them, as upstream's does.
fn reorder<T: Copy + Eq>(list: &mut [T], requested: &[T]) {
    let mut position = 0;
    for wanted in requested.iter().copied() {
        let Some(index) = list.iter().position(|candidate| *candidate == wanted) else {
            continue;
        };
        list.swap(position, index);
        position += 1;
        if position == list.len() {
            break;
        }
    }
}

/// Upstream's wrap-around step used by `closest_monitor` and `bubble_desktop`.
/// `len` is always at least two at both call sites.
fn wrapped_index(index: usize, len: usize, next: bool) -> usize {
    if next {
        (index + 1) % len
    } else {
        index.checked_sub(1).unwrap_or(len - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_and_ordering_match_upstream_defaults() {
        let settings = Settings::default();
        let mut world = World::default();
        let right = world.create_monitor(1, None, Rectangle::new(100, 0, 100, 100), &settings);
        let left = world.create_monitor(2, Some("left"), Rectangle::new(0, 0, 100, 100), &settings);
        assert_eq!(world.monitor_order(), &[left, right]);
        assert_eq!(world.focused_monitor, Some(right));
        assert_eq!(world.monitor(right).name, "MONITOR");
        let desktop = world.create_desktop(3, None, &settings);
        assert!(world.add_desktop(right, desktop));
        assert_eq!(world.monitor(right).active_desktop, Some(desktop));
        assert_eq!(world.desktop(desktop).name, "Desktop");
        assert_eq!(world.validate(), Ok(()));
    }

    #[test]
    fn external_ids_are_the_lowest_free_slot_and_come_back_after_a_free() {
        let settings = Settings::default();
        let mut world = World::default();
        // Growth without frees hands out the same ascending run as the old
        // linear scan over an arena that never released anything.
        let mut handed_out = Vec::new();
        for _ in 0..4 {
            let id = world.next_external_id();
            handed_out.push(id);
            let _ = world.create_desktop(id, None, &settings);
        }
        assert_eq!(
            handed_out,
            [0xF000_0000, 0xF000_0001, 0xF000_0002, 0xF000_0003]
        );

        // A client window's id sits below the reserved range and never
        // influences the allocator.
        let node = world.tree.add_node(0x0060_0001, 0.5);
        assert_eq!(world.next_external_id(), 0xF000_0004);
        world.tree.destroy_subtree(node);

        // Freeing a slot in the middle makes its id the lowest free one again.
        let monitor = world.create_monitor(0xF000_0004, None, Rectangle::default(), &settings);
        let desktop = world.create_desktop(0xF000_0005, None, &settings);
        assert!(world.add_desktop(monitor, desktop));
        let spare = world.create_desktop(0xF000_0006, None, &settings);
        assert!(world.add_desktop(monitor, spare));
        assert_eq!(world.next_external_id(), 0xF000_0007);
        assert!(world.remove_desktop(desktop, 0.5).is_some());
        assert_eq!(world.next_external_id(), 0xF000_0005);
    }

    #[test]
    fn closest_monitor_wraps_and_excludes_singleton() {
        let settings = Settings::default();
        let mut world = World::default();
        let first = world.create_monitor(1, None, Rectangle::new(0, 0, 100, 100), &settings);
        assert_eq!(world.closest_monitor(first, true), None);
        let second = world.create_monitor(2, None, Rectangle::new(100, 0, 100, 100), &settings);
        assert_eq!(world.closest_monitor(first, true), Some(second));
        assert_eq!(world.closest_monitor(first, false), Some(second));
    }

    #[test]
    fn desktop_transfer_and_swap_preserve_order_and_active_slots() {
        let settings = Settings::default();
        let mut world = World::default();
        let first_monitor =
            world.create_monitor(1, None, Rectangle::new(0, 0, 100, 100), &settings);
        let second_monitor =
            world.create_monitor(2, None, Rectangle::new(100, 0, 100, 100), &settings);
        let first = world.create_desktop(10, Some("I"), &settings);
        let second = world.create_desktop(11, Some("II"), &settings);
        let third = world.create_desktop(12, Some("III"), &settings);
        world.add_desktop(first_monitor, first);
        world.add_desktop(first_monitor, second);
        world.add_desktop(second_monitor, third);
        assert!(world.swap_desktops(first, second));
        assert_eq!(world.monitor(first_monitor).active_desktop, Some(second));
        assert!(world.swap_desktops(first, second));
        assert_eq!(world.monitor(first_monitor).active_desktop, Some(first));
        assert!(world.transfer_desktop(second, second_monitor));
        assert_eq!(world.monitor(second_monitor).desktops, [third, second]);
        assert!(world.swap_desktops(first, third));
        assert_eq!(world.monitor(first_monitor).desktops, [third]);
        assert_eq!(world.monitor(first_monitor).active_desktop, Some(third));
        assert_eq!(world.validate(), Ok(()));
    }

    #[test]
    fn user_layout_can_diverge_under_single_monocle() {
        let settings = Settings::default();
        let mut world = World::default();
        let desktop = world.create_desktop(1, None, &settings);
        world.desktop_mut(desktop).layout = Layout::Monocle;
        assert!(world.set_layout(desktop, Layout::Monocle, true, true));
        assert_eq!(world.desktop(desktop).user_layout, Layout::Monocle);
        assert_eq!(world.desktop(desktop).layout, Layout::Monocle);
    }

    #[test]
    fn desktop_urgency_follows_urgent_clients() {
        let settings = Settings::default();
        let mut world = World::default();
        let desktop = world.create_desktop(1, None, &settings);
        assert!(!world.desktop_is_urgent(desktop));

        let node = world.tree.add_node(2, 0.5);
        let mut client = crate::tree::Client::from_settings(&settings);
        client.urgent = true;
        world.tree.node_mut(node).client = Some(client);
        world.desktop_mut(desktop).tree.root = Some(node);
        assert!(world.desktop_is_urgent(desktop));
    }

    #[test]
    fn state_transition_applies_single_monocle_policy_and_user_layout() {
        let settings = Settings::default();
        let mut world = World::default();
        let desktop = world.create_desktop(1, None, &settings);
        let first = world.tree.add_node(2, 0.5);
        let second = world.tree.add_node(3, 0.5);
        let root = world.tree.add_node(4, 0.5);
        for node in [first, second] {
            world.tree.node_mut(node).client = Some(crate::tree::Client::from_settings(&settings));
        }
        world.tree.set_children(root, first, second);
        world.desktop_mut(desktop).tree = TreeState {
            root: Some(root),
            focus: Some(first),
        };

        assert!(world.set_state(desktop, first, crate::types::ClientState::Floating, true));
        assert_eq!(world.desktop(desktop).layout, Layout::Monocle);
        assert!(world.set_state(desktop, first, crate::types::ClientState::Tiled, true));
        assert_eq!(world.desktop(desktop).layout, Layout::Tiled);
    }

    #[test]
    fn structural_topology_operations_keep_world_valid() {
        let settings = Settings::default();
        let mut world = World::default();
        let left = world.create_monitor(1, Some("left"), Rectangle::new(0, 0, 100, 100), &settings);
        let right = world.create_monitor(
            2,
            Some("right"),
            Rectangle::new(100, 0, 100, 100),
            &settings,
        );
        let one = world.create_desktop(10, Some("I"), &settings);
        let two = world.create_desktop(11, Some("II"), &settings);
        let three = world.create_desktop(12, Some("III"), &settings);
        assert!(world.add_desktop(left, one));
        assert!(world.add_desktop(left, two));
        assert!(world.add_desktop(right, three));
        let first = world.tree.add_node(20, 0.5);
        let second = world.tree.add_node(21, 0.5);
        for node in [first, second] {
            world.tree.node_mut(node).client = Some(crate::tree::Client::from_settings(&settings));
        }
        world.desktop_mut(one).tree = TreeState {
            root: Some(first),
            focus: Some(first),
        };
        world.desktop_mut(three).tree = TreeState {
            root: Some(second),
            focus: Some(second),
        };

        let moved = world
            .transfer_node(first, three, Some(second), 0.5)
            .unwrap();
        assert_eq!(moved.destination_desktop, three);
        assert!(world.desktop(one).tree.root.is_none());
        assert!(world.insert_receptacle(two, None, 0.5).is_ok());
        assert!(world.swap_monitors(left, right));
        world.reorder_monitors(&[left, right]);
        world.reorder_desktops(left, &[two, one]);
        assert_eq!(world.validate(), Ok(()));
        assert!(world.remove_desktop(one, 0.5).is_some());
        let removal = world.remove_monitor(right).unwrap();
        // Removing a monitor orphans its desktops' trees; the caller owns them
        // until it hands them back, which is what `purge_removal` does.
        for root in &removal.roots {
            world.tree.destroy_subtree(*root);
        }
        assert_eq!(world.validate(), Ok(()));
    }

    #[test]
    fn a_rejected_node_transfer_leaves_the_world_untouched() {
        let settings = Settings::default();
        let mut world = World::default();
        let monitor = world.create_monitor(1, None, Rectangle::new(0, 0, 100, 100), &settings);
        let desktop = world.create_desktop(10, Some("I"), &settings);
        assert!(world.add_desktop(monitor, desktop));

        let first = world.tree.add_node(20, 0.5);
        let second = world.tree.add_node(21, 0.5);
        for node in [first, second] {
            world.tree.node_mut(node).client = Some(crate::tree::Client::from_settings(&settings));
        }
        let branch = world.tree.add_node(22, 0.5);
        let mut state = TreeState::default();
        world
            .tree
            .insert(
                &mut state,
                first,
                None,
                None,
                crate::tree::ChildPolarity::Second,
            )
            .unwrap();
        world
            .tree
            .insert(
                &mut state,
                second,
                Some(first),
                Some(branch),
                crate::tree::ChildPolarity::Second,
            )
            .unwrap();
        assert_eq!(state.root, Some(branch));
        world.desktop_mut(desktop).tree = state;
        assert_eq!(world.validate(), Ok(()));
        let before = world.clone();

        // Unlinking `second` collapses `branch`, so the anchor is gone by the
        // time the insert half runs. The failure must roll the whole operation
        // back rather than leave the desktop rooted at a retired node.
        assert_eq!(
            world.transfer_node(second, desktop, Some(branch), 0.5),
            Err(StructuralError::InvalidAnchor)
        );
        assert_eq!(world, before);
        assert_eq!(world.validate(), Ok(()));
    }

    #[test]
    fn a_rejected_receptacle_insert_does_not_leak_nodes() {
        let settings = Settings::default();
        let mut world = World::default();
        let monitor = world.create_monitor(1, None, Rectangle::new(0, 0, 100, 100), &settings);
        let desktop = world.create_desktop(10, Some("I"), &settings);
        assert!(world.add_desktop(monitor, desktop));
        let stray = world.tree.add_node(20, 0.5);
        let before = world.clone();

        // `stray` is not part of the desktop, so the insert is rejected; the
        // receptacle allocated for it must not survive in the published tree.
        assert_eq!(
            world.insert_receptacle(desktop, Some(stray), 0.5),
            Err(StructuralError::InvalidAnchor)
        );
        assert_eq!(world, before);
    }
}
