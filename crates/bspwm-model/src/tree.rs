use std::collections::HashSet;

use slotmap::SlotMap;

use crate::settings::Settings;
use crate::types::{
    AutomaticScheme, CirculateDirection, ClientState, Constraints, Direction, Flip,
    HonorSizeHintsMode, Rectangle, SplitType, StackLayer, WmFlags,
};

pub const MIN_WIDTH: u16 = 32;
pub const MIN_HEIGHT: u16 = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SizeHints {
    pub flags: u32,
    pub min_width: i32,
    pub min_height: i32,
    pub max_width: i32,
    pub max_height: i32,
    pub width_inc: i32,
    pub height_inc: i32,
    pub min_aspect_num: i32,
    pub min_aspect_den: i32,
    pub max_aspect_num: i32,
    pub max_aspect_den: i32,
    pub base_width: i32,
    pub base_height: i32,
}

impl SizeHints {
    pub const MIN_SIZE: u32 = 1 << 4;
    pub const MAX_SIZE: u32 = 1 << 5;
    pub const RESIZE_INC: u32 = 1 << 6;
    pub const ASPECT: u32 = 1 << 7;
    pub const BASE_SIZE: u32 = 1 << 8;

    #[must_use]
    pub const fn is_fixed(self) -> bool {
        self.flags & (Self::MIN_SIZE | Self::MAX_SIZE) != 0
            && self.min_width == self.max_width
            && self.min_height == self.max_height
    }
}

slotmap::new_key_type! {
    /// A generational handle into [`Tree`]'s node arena.
    ///
    /// Retiring a node frees its slot and bumps that slot's generation, so a
    /// handle kept past its node's lifetime resolves to `None` rather than to
    /// whatever later took the slot. [`Tree::node`] turns that into a panic on
    /// purpose: every id holder is expected to be purged when a node dies, and
    /// a silent fallback would hide the bug instead of surfacing it.
    pub struct NodeId;
}

/// Set equality for two arenas, which `SlotMap` does not provide itself.
///
/// Keys are part of the identity: two arenas holding equal nodes under
/// different keys are not interchangeable, because every id holder outside the
/// arena names nodes by key.
pub(crate) fn arena_eq<K: slotmap::Key, V: PartialEq>(
    left: &SlotMap<K, V>,
    right: &SlotMap<K, V>,
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .all(|(key, value)| right.get(key) == Some(value))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TreeState {
    pub root: Option<NodeId>,
    pub focus: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildPolarity {
    First,
    Second,
}

impl ChildPolarity {
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::First => Self::Second,
            Self::Second => Self::First,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StructuralError {
    #[error("node is already attached")]
    AlreadyAttached,
    #[error("invalid anchor")]
    InvalidAnchor,
    #[error("invalid branch")]
    InvalidBranch,
    #[error("invalid swap")]
    InvalidSwap,
    #[error("node is not attached")]
    NotAttached,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnlinkResult {
    pub detached: NodeId,
    pub collapsed: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeFlag {
    Hidden,
    Sticky,
    Private,
    Locked,
    Marked,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    pub class_name: String,
    pub instance_name: String,
    pub name: String,
    pub border_width: u32,
    pub urgent: bool,
    pub shown: bool,
    pub state: ClientState,
    pub last_state: ClientState,
    pub layer: StackLayer,
    pub last_layer: StackLayer,
    pub floating_rectangle: Rectangle,
    pub tiled_rectangle: Rectangle,
    pub honor_size_hints: HonorSizeHintsMode,
    pub size_hints: SizeHints,
    pub icccm: IcccmProps,
    pub wm_flags: WmFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IcccmProps {
    pub input_hint: bool,
    pub take_focus: bool,
    pub delete_window: bool,
}

impl Default for IcccmProps {
    fn default() -> Self {
        Self {
            input_hint: true,
            take_focus: false,
            delete_window: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presel {
    pub split_dir: Direction,
    pub split_ratio: f64,
    pub feedback: Option<u32>,
}

impl Presel {
    #[must_use]
    pub const fn new(split_ratio: f64) -> Self {
        Self {
            split_dir: Direction::East,
            split_ratio,
            feedback: None,
        }
    }
}

impl Client {
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            class_name: "N/A".into(),
            instance_name: "N/A".into(),
            name: String::new(),
            border_width: settings.border_width,
            urgent: false,
            shown: false,
            state: ClientState::Tiled,
            last_state: ClientState::Tiled,
            layer: StackLayer::Normal,
            last_layer: StackLayer::Normal,
            floating_rectangle: Rectangle::default(),
            tiled_rectangle: Rectangle::default(),
            honor_size_hints: settings.honor_size_hints,
            size_hints: SizeHints::default(),
            icccm: IcccmProps::default(),
            wm_flags: WmFlags::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Node {
    pub external_id: u32,
    pub split_type: SplitType,
    pub split_ratio: f64,
    pub presel: Option<Presel>,
    pub rectangle: Rectangle,
    pub constraints: Constraints,
    pub vacant: bool,
    pub hidden: bool,
    pub sticky: bool,
    pub private: bool,
    pub locked: bool,
    pub marked: bool,
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub second_child: Option<NodeId>,
    pub client: Option<Client>,
}

impl Node {
    #[must_use]
    pub const fn new(external_id: u32, split_ratio: f64) -> Self {
        Self {
            external_id,
            split_type: SplitType::Vertical,
            split_ratio,
            presel: None,
            rectangle: Rectangle::new(0, 0, 0, 0),
            constraints: Constraints {
                min_width: MIN_WIDTH,
                min_height: MIN_HEIGHT,
            },
            vacant: false,
            hidden: false,
            sticky: false,
            private: false,
            locked: false,
            marked: false,
            parent: None,
            first_child: None,
            second_child: None,
            client: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: SlotMap<NodeId, Node>,
    retired_feedbacks: Vec<u32>,
    /// Nodes freed since the last [`Tree::take_retired_nodes`].
    ///
    /// Structural operations free nodes the caller never named -- `unlink`
    /// collapses a branch, `insert` consumes a receptacle -- so the ids of
    /// those nodes have to reach the stores that might still hold them.
    /// Upstream does the same work inline (`unlink_node` calls
    /// `history_remove` on the collapsing parent before `free`ing it).
    retired_nodes: Vec<NodeId>,
}

impl PartialEq for Tree {
    fn eq(&self, other: &Self) -> bool {
        self.retired_feedbacks == other.retired_feedbacks
            && self.retired_nodes == other.retired_nodes
            && arena_eq(&self.nodes, &other.nodes)
    }
}

impl Tree {
    #[must_use]
    pub fn add_node(&mut self, external_id: u32, split_ratio: f64) -> NodeId {
        self.nodes.insert(Node::new(external_id, split_ratio))
    }

    /// The number of live nodes in the arena.
    ///
    /// Retired nodes are freed, so this tracks the nodes reachable from some
    /// desktop plus the ones a caller has detached but not yet destroyed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// # Panics
    ///
    /// Panics when `id` names a node that has already been retired. Callers
    /// that legitimately probe a possibly-dead id must use [`Tree::get`].
    #[must_use]
    pub fn node(&self, id: NodeId) -> &Node {
        self.nodes.get(id).expect("node id outlived its node")
    }

    /// # Panics
    ///
    /// Panics when `id` names a node that has already been retired.
    #[must_use]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes.get_mut(id).expect("node id outlived its node")
    }

    /// The node `id` names, or `None` once it has been retired.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Reports whether `id` still names a live node.
    #[must_use]
    pub fn is_live(&self, id: NodeId) -> bool {
        self.nodes.contains_key(id)
    }

    #[must_use]
    pub fn external_id_exists(&self, external_id: u32) -> bool {
        self.nodes
            .values()
            .any(|node| node.external_id == external_id)
    }

    /// The external id of every live node, in unspecified order.
    pub fn external_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.nodes.values().map(|node| node.external_id)
    }

    /// Adapts every client rectangle in `root` from one containing area to another.
    pub fn adapt_client_geometry(
        &mut self,
        root: NodeId,
        source: Rectangle,
        destination: Rectangle,
    ) {
        let nodes: Vec<_> = self.preorder(root).collect();
        for node in nodes {
            if let Some(client) = self.node_mut(node).client.as_mut() {
                client.floating_rectangle = bspwm_core::geometry::adapt_geometry(
                    client.floating_rectangle,
                    source,
                    destination,
                );
            }
        }
    }

    /// The topmost sticky nodes under `root`, in first-child-first order.
    #[must_use]
    pub fn sticky_roots(&self, root: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut nodes = vec![root];
        while let Some(node) = nodes.pop() {
            let item = self.node(node);
            if item.sticky {
                result.push(node);
                continue;
            }
            if let Some(second) = item.second_child {
                nodes.push(second);
            }
            if let Some(first) = item.first_child {
                nodes.push(first);
            }
        }
        result
    }

    pub fn set_presel_direction(
        &mut self,
        id: NodeId,
        direction: Direction,
        default_ratio: f64,
    ) -> bool {
        let created = self.node_mut(id).presel.is_none();
        let presel = self
            .node_mut(id)
            .presel
            .get_or_insert_with(|| Presel::new(default_ratio));
        let changed = created || presel.split_dir != direction;
        presel.split_dir = direction;
        changed
    }

    pub fn set_presel_ratio(&mut self, id: NodeId, ratio: f64, default_ratio: f64) -> bool {
        let created = self.node_mut(id).presel.is_none();
        let presel = self
            .node_mut(id)
            .presel
            .get_or_insert_with(|| Presel::new(default_ratio));
        let changed = created || presel.split_ratio.to_bits() != ratio.to_bits();
        presel.split_ratio = ratio;
        changed
    }

    /// Removes preselection and returns its state so the I/O layer can destroy feedback.
    pub fn cancel_presel(&mut self, id: NodeId) -> Option<Presel> {
        let presel = self.node_mut(id).presel.take();
        if let Some(feedback) = presel.and_then(|presel| presel.feedback) {
            self.retired_feedbacks.push(feedback);
        }
        presel
    }

    pub fn take_retired_feedbacks(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.retired_feedbacks)
    }

    pub fn cancel_subtree_presels(&mut self, root: NodeId) {
        for node in self.preorder(root).collect::<Vec<_>>() {
            self.cancel_presel(node);
        }
    }

    #[must_use]
    pub fn feedback_windows(&self) -> Vec<u32> {
        let mut windows: Vec<u32> = self
            .nodes
            .values()
            .filter_map(|node| node.presel.and_then(|presel| presel.feedback))
            .collect();
        // Arena order is unspecified, and this list reaches X as a stacking
        // request, so give it a deterministic order of its own.
        windows.sort_unstable();
        windows
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn set_children(&mut self, parent: NodeId, first: NodeId, second: NodeId) {
        assert_ne!(parent, first);
        assert_ne!(parent, second);
        assert_ne!(first, second);
        assert!(self.node_mut(first).parent.is_none());
        assert!(self.node_mut(second).parent.is_none());
        assert!(self.node_mut(parent).first_child.is_none());
        assert!(self.node_mut(parent).second_child.is_none());
        self.node_mut(parent).first_child = Some(first);
        self.node_mut(parent).second_child = Some(second);
        self.node_mut(first).parent = Some(parent);
        self.node_mut(second).parent = Some(parent);
        self.update_constraints(parent);
    }

    /// Reports whether `id` sits in `state`'s tree.
    ///
    /// A retired id is in no tree, so this answers `false` rather than
    /// panicking: it is the guard [`Tree::insert`] uses to reject an anchor
    /// that an earlier half of the same operation collapsed.
    #[must_use]
    pub fn contains(&self, state: &TreeState, id: NodeId) -> bool {
        self.is_live(id) && state.root.is_some_and(|root| self.is_descendant(id, root))
    }

    /// Inserts a newly managed node using bspwm's configured automatic scheme.
    #[allow(clippy::missing_errors_doc)]
    pub fn insert_automatic(
        &mut self,
        state: &mut TreeState,
        subtree: NodeId,
        anchor: NodeId,
        branch: NodeId,
        polarity: ChildPolarity,
        scheme: AutomaticScheme,
    ) -> Result<Option<NodeId>, StructuralError> {
        if !self.contains(state, anchor)
            || self.node(anchor).presel.is_some()
            || self.is_leaf(anchor) && self.node(anchor).client.is_none()
        {
            return self.insert(state, subtree, Some(anchor), Some(branch), polarity);
        }
        let Some(root) = state.root else {
            return Err(StructuralError::InvalidAnchor);
        };
        let parent = self.node(anchor).parent;
        let single_tiled = self
            .node(anchor)
            .client
            .as_ref()
            .is_some_and(|client| client.state.is_tiled())
            && self.tiled_count(root, true) == 1;

        if let Some(parent) = parent.filter(|_| scheme == AutomaticScheme::Spiral && !single_tiled)
        {
            if self.node(subtree).parent.is_some() || self.contains(state, subtree) {
                return Err(StructuralError::AlreadyAttached);
            }
            if self.node(branch).parent.is_some()
                || !self.is_leaf(branch)
                || self.contains(state, branch)
                || branch == subtree
                || branch == anchor
            {
                return Err(StructuralError::InvalidBranch);
            }
            let anchor_is_first = self.node(parent).first_child == Some(anchor);
            if !anchor_is_first && self.node(parent).second_child != Some(anchor) {
                return Err(StructuralError::InvalidAnchor);
            }
            let grandparent = self.node(parent).parent;
            let split_type = self.node(parent).split_type;
            let split_ratio = self.node(parent).split_ratio;
            self.replace_parent_edge(state, grandparent, parent, branch);
            {
                let value = self.node_mut(branch);
                value.parent = grandparent;
                value.split_type = split_type;
                value.split_ratio = split_ratio;
            }
            if anchor_is_first {
                self.attach_children(branch, subtree, parent);
                if !self.node(subtree).vacant {
                    self.rotate_rec(parent, 90);
                }
            } else {
                self.attach_children(branch, parent, subtree);
                if !self.node(subtree).vacant {
                    self.rotate_rec(parent, 270);
                }
            }
            self.rebuild_constraints_from_leaves(branch);
            self.rebuild_constraints_towards_root(branch);
            self.focus_if_unfocused(state, subtree);
            return Ok(None);
        }

        let longest_side = || {
            let rectangle = self.node(anchor).rectangle;
            if rectangle.width > rectangle.height {
                SplitType::Vertical
            } else {
                SplitType::Horizontal
            }
        };
        let split_type = match parent {
            None => longest_side(),
            Some(_) if scheme == AutomaticScheme::LongestSide || single_tiled => longest_side(),
            Some(parent) => {
                let mut candidate = Some(parent);
                while let Some(node) = candidate {
                    let value = self.node(node);
                    let (Some(first), Some(second)) = (value.first_child, value.second_child)
                    else {
                        return Err(StructuralError::InvalidAnchor);
                    };
                    if !self.node(first).vacant && !self.node(second).vacant {
                        break;
                    }
                    candidate = value.parent;
                }
                match self.node(candidate.unwrap_or(parent)).split_type {
                    SplitType::Horizontal => SplitType::Vertical,
                    SplitType::Vertical => SplitType::Horizontal,
                }
            }
        };
        self.node_mut(branch).split_type = split_type;
        self.insert(state, subtree, Some(anchor), Some(branch), polarity)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn insert(
        &mut self,
        state: &mut TreeState,
        subtree: NodeId,
        anchor: Option<NodeId>,
        branch: Option<NodeId>,
        polarity: ChildPolarity,
    ) -> Result<Option<NodeId>, StructuralError> {
        if self.node(subtree).parent.is_some() || self.contains(state, subtree) {
            return Err(StructuralError::AlreadyAttached);
        }
        let Some(anchor) = anchor else {
            if state.root.is_some() {
                return Err(StructuralError::InvalidAnchor);
            }
            state.root = Some(subtree);
            self.focus_if_unfocused(state, subtree);
            return Ok(None);
        };
        if !self.contains(state, anchor) {
            return Err(StructuralError::InvalidAnchor);
        }
        if self.is_leaf(anchor)
            && self.node(anchor).client.is_none()
            && self.node(anchor).presel.is_none()
        {
            let parent = self.node(anchor).parent;
            self.replace_parent_edge(state, parent, anchor, subtree);
            self.node_mut(subtree).parent = parent;
            self.retire(anchor);
            if state.focus == Some(anchor) {
                state.focus = None;
            }
            self.rebuild_constraints_towards_root(subtree);
            // The retired receptacle may have held the focus; the replacement
            // subtree inherits it, exactly as on the branching path below.
            self.focus_if_unfocused(state, subtree);
            return Ok(Some(anchor));
        }
        let branch = branch.ok_or(StructuralError::InvalidBranch)?;
        if self.node(branch).parent.is_some()
            || !self.is_leaf(branch)
            || self.contains(state, branch)
            || branch == subtree
            || branch == anchor
        {
            return Err(StructuralError::InvalidBranch);
        }
        let parent = self.node(anchor).parent;
        let presel = self.node(anchor).presel;
        self.replace_parent_edge(state, parent, anchor, branch);
        self.node_mut(branch).parent = parent;
        let polarity = presel.map_or(polarity, |presel| match presel.split_dir {
            Direction::North | Direction::West => ChildPolarity::First,
            Direction::South | Direction::East => ChildPolarity::Second,
        });
        if let Some(presel) = presel {
            let branch = self.node_mut(branch);
            branch.split_ratio = presel.split_ratio;
            branch.split_type = match presel.split_dir {
                Direction::North | Direction::South => SplitType::Horizontal,
                Direction::West | Direction::East => SplitType::Vertical,
            };
            self.cancel_presel(anchor);
        }
        match polarity {
            ChildPolarity::First => self.attach_children(branch, subtree, anchor),
            ChildPolarity::Second => self.attach_children(branch, anchor, subtree),
        }
        self.rebuild_constraints_from_leaves(branch);
        self.rebuild_constraints_towards_root(branch);
        self.focus_if_unfocused(state, subtree);
        Ok(None)
    }

    /// Gives `subtree` the focus when the desktop has none and `subtree` can hold it.
    fn focus_if_unfocused(&self, state: &mut TreeState, subtree: NodeId) {
        if state.focus.is_none() && self.is_focusable(subtree) {
            state.focus = Some(subtree);
        }
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn unlink(
        &mut self,
        state: &mut TreeState,
        subtree: NodeId,
    ) -> Result<UnlinkResult, StructuralError> {
        if !self.contains(state, subtree) {
            return Err(StructuralError::NotAttached);
        }
        let Some(parent) = self.node(subtree).parent else {
            state.root = None;
            state.focus = None;
            return Ok(UnlinkResult {
                detached: subtree,
                collapsed: None,
            });
        };
        let sibling = self
            .sibling(subtree)
            .ok_or(StructuralError::InvalidAnchor)?;
        let grandparent = self.node(parent).parent;
        self.replace_parent_edge(state, grandparent, parent, sibling);
        self.node_mut(sibling).parent = grandparent;
        self.node_mut(subtree).parent = None;
        if state
            .focus
            .is_some_and(|focus| focus == parent || self.is_descendant(focus, subtree))
        {
            state.focus = None;
        }
        self.retire(parent);
        self.rebuild_constraints_towards_root(sibling);
        Ok(UnlinkResult {
            detached: subtree,
            collapsed: Some(parent),
        })
    }

    #[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
    pub fn swap_within(
        &mut self,
        state: &mut TreeState,
        first: NodeId,
        second: NodeId,
    ) -> Result<(), StructuralError> {
        if first == second
            || !self.contains(state, first)
            || !self.contains(state, second)
            || self.is_descendant(first, second)
            || self.is_descendant(second, first)
        {
            return Err(StructuralError::InvalidSwap);
        }
        self.swap_edges(state, first, second);
        self.rebuild_constraints_from_leaves(state.root.expect("nonempty tree"));
        Ok(())
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn swap_between(
        &mut self,
        first_state: &mut TreeState,
        first: NodeId,
        second_state: &mut TreeState,
        second: NodeId,
    ) -> Result<(), StructuralError> {
        if first == second
            || !self.contains(first_state, first)
            || !self.contains(second_state, second)
        {
            return Err(StructuralError::InvalidSwap);
        }
        let first_parent = self.node(first).parent;
        let second_parent = self.node(second).parent;
        let first_held_focus = first_state
            .focus
            .is_some_and(|focus| self.is_descendant(focus, first));
        let second_held_focus = second_state
            .focus
            .is_some_and(|focus| self.is_descendant(focus, second));
        let first_focus = first_state.focus;
        let second_focus = second_state.focus;
        self.replace_parent_edge(first_state, first_parent, first, second);
        self.replace_parent_edge(second_state, second_parent, second, first);
        self.node_mut(first).parent = second_parent;
        self.node_mut(second).parent = first_parent;
        if first_held_focus {
            first_state.focus = if second_held_focus {
                second_focus
            } else {
                Some(second)
            };
        }
        if second_held_focus {
            second_state.focus = if first_held_focus {
                first_focus
            } else {
                Some(first)
            };
        }
        if let Some(root) = first_state.root {
            self.rebuild_constraints_from_leaves(root);
        }
        if let Some(root) = second_state.root {
            self.rebuild_constraints_from_leaves(root);
        }
        Ok(())
    }

    pub fn set_layer(&mut self, node: NodeId, layer: StackLayer) -> bool {
        let Some(client) = self.node_mut(node).client.as_mut() else {
            return false;
        };
        if client.layer == layer {
            return false;
        }
        client.last_layer = client.layer;
        client.layer = layer;
        true
    }

    pub fn set_state(&mut self, node: NodeId, state: ClientState) -> bool {
        let Some(client) = self.node(node).client.as_ref() else {
            return false;
        };
        if client.state == state {
            return false;
        }
        let previous = client.state;
        // Upstream `set_state` reaches vacancy only through `set_floating` /
        // `set_fullscreen`, so a Tiled <-> PseudoTiled move calls neither and
        // leaves vacancy alone; both setters are guarded by `if (!n->hidden)`,
        // so a hidden node also keeps whatever vacancy hiding gave it.
        let touches_vacancy = !previous.is_tiled() || !state.is_tiled();
        if touches_vacancy {
            self.cancel_presel(node);
        }
        let Some(client) = self.node_mut(node).client.as_mut() else {
            return false;
        };
        client.last_state = client.state;
        client.state = state;
        if touches_vacancy && !self.node(node).hidden {
            self.set_vacant(node, !state.is_tiled());
        }
        true
    }

    #[must_use]
    pub fn flag(&self, node: NodeId, flag: NodeFlag) -> bool {
        let node = self.node(node);
        match flag {
            NodeFlag::Hidden => node.hidden,
            NodeFlag::Sticky => node.sticky,
            NodeFlag::Private => node.private,
            NodeFlag::Locked => node.locked,
            NodeFlag::Marked => node.marked,
        }
    }

    pub fn set_flag(&mut self, node: NodeId, flag: NodeFlag, value: bool) -> bool {
        if self.flag(node, flag) == value {
            return false;
        }
        match flag {
            NodeFlag::Hidden => self.set_hidden(node, value),
            NodeFlag::Sticky => self.node_mut(node).sticky = value,
            NodeFlag::Private => self.node_mut(node).private = value,
            NodeFlag::Locked => self.node_mut(node).locked = value,
            NodeFlag::Marked => self.node_mut(node).marked = value,
        }
        true
    }

    /// Recomputes `field` on every ancestor of `node` as the conjunction of its
    /// two children, which is how upstream propagates vacancy and hiddenness.
    fn propagate_upward(&mut self, node: NodeId, field: fn(&mut Node) -> &mut bool) {
        let mut current = node;
        while let Some(parent) = self.node(current).parent {
            let first = self.node(parent).first_child.expect("internal node");
            let second = self.node(parent).second_child.expect("internal node");
            let first = *field(self.node_mut(first));
            let second = *field(self.node_mut(second));
            *field(self.node_mut(parent)) = first && second;
            current = parent;
        }
    }

    fn set_vacant(&mut self, node: NodeId, value: bool) {
        self.set_vacant_downward(node, value);
        self.propagate_vacancy_upward(node);
    }

    pub fn sync_vacancy(&mut self, node: NodeId) {
        let item = self.node(node);
        let vacant = item.hidden
            || item
                .client
                .as_ref()
                .is_some_and(|client| !client.state.is_tiled());
        self.set_vacant(node, vacant);
    }

    fn set_vacant_downward(&mut self, node: NodeId, value: bool) {
        for id in self.preorder(node).collect::<Vec<_>>() {
            self.set_vacant_local(id, value);
        }
    }

    fn set_vacant_local(&mut self, node: NodeId, value: bool) {
        if self.node(node).vacant == value {
            return;
        }
        self.node_mut(node).vacant = value;
        if value {
            self.cancel_presel(node);
        }
    }

    fn propagate_vacancy_upward(&mut self, node: NodeId) {
        let mut current = node;
        while let Some(parent) = self.node(current).parent {
            let first = self.node(parent).first_child.expect("internal node");
            let second = self.node(parent).second_child.expect("internal node");
            self.set_vacant_local(parent, self.node(first).vacant && self.node(second).vacant);
            current = parent;
        }
    }

    fn set_hidden(&mut self, node: NodeId, value: bool) {
        self.set_hidden_downward(node, value);
        self.propagate_upward(node, |node| &mut node.hidden);
    }

    fn set_hidden_downward(&mut self, node: NodeId, value: bool) {
        let subtree: Vec<_> = self.preorder(node).collect();
        for id in subtree.iter().copied() {
            self.node_mut(id).hidden = value;
            if self
                .node(id)
                .client
                .as_ref()
                .is_some_and(|client| client.state.is_tiled())
            {
                self.set_vacant_local(id, value);
            }
        }
        // The recursion this replaces recomputed each branch's vacancy after
        // both of its subtrees were done: reversed preorder is that same order.
        for id in subtree.into_iter().rev() {
            if let (Some(first), Some(second)) =
                (self.node(id).first_child, self.node(id).second_child)
            {
                self.set_vacant_local(id, self.node(first).vacant && self.node(second).vacant);
            }
        }
        self.propagate_vacancy_upward(node);
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn circulate(
        &mut self,
        state: &mut TreeState,
        root: NodeId,
        direction: CirculateDirection,
    ) -> bool {
        // Upstream `circulate_leaves` walks with `prev_tiled_leaf`/`next_tiled_leaf`,
        // which skip receptacles *and* vacant leaves, so a hidden tiled client
        // (vacant) does not take part.
        let tiled: Vec<_> = self.tiled_leaves(root).collect();
        if tiled.len() < 2 {
            return false;
        }
        let focus_slot = state
            .focus
            .map(|focus| (self.node(focus).parent, self.is_first_child(focus), focus));
        // `swap_within` rebuilds the whole tree's constraints on every call, which
        // makes circulating quadratic; the leaf constraints never change here, so
        // one rebuild after the last swap is equivalent. The root is resolved
        // first so a caller passing a foreign tree still fails before mutating.
        let tree_root = state.root.expect("nonempty tree");
        let pairs: Vec<_> = match direction {
            CirculateDirection::Forward => (1..tiled.len()).rev().collect(),
            CirculateDirection::Backward => (1..tiled.len()).collect(),
        };
        for index in pairs {
            self.swap_edges(state, tiled[index - 1], tiled[index]);
        }
        self.rebuild_constraints_from_leaves(tree_root);
        if let Some((parent, first, old_focus)) = focus_slot {
            state.focus = parent.map_or(state.root, |parent| {
                if first {
                    self.node(parent).first_child
                } else {
                    self.node(parent).second_child
                }
            });
            if state.focus.is_some_and(|focus| !self.is_leaf(focus)) {
                state.focus = Some(old_focus);
            }
        }
        true
    }

    fn swap_edges(&mut self, state: &mut TreeState, first: NodeId, second: NodeId) {
        let first_parent = self.node(first).parent;
        let second_parent = self.node(second).parent;
        // Both sides are resolved before either edge moves: for two siblings the
        // second lookup would otherwise see the first replacement.
        let first_polarity = self.polarity_of(first);
        let second_polarity = self.polarity_of(second);
        self.replace_edge(state, first_parent, first_polarity, second);
        self.replace_edge(state, second_parent, second_polarity, first);
        self.node_mut(first).parent = second_parent;
        self.node_mut(second).parent = first_parent;
    }

    /// Points `parent`'s `polarity` edge at `new`, or reroots `state`.
    fn replace_edge(
        &mut self,
        state: &mut TreeState,
        parent: Option<NodeId>,
        polarity: ChildPolarity,
        new: NodeId,
    ) {
        let Some(parent) = parent else {
            state.root = Some(new);
            return;
        };
        match polarity {
            ChildPolarity::First => self.node_mut(parent).first_child = Some(new),
            ChildPolarity::Second => self.node_mut(parent).second_child = Some(new),
        }
    }

    fn attach_children(&mut self, parent: NodeId, first: NodeId, second: NodeId) {
        self.node_mut(parent).first_child = Some(first);
        self.node_mut(parent).second_child = Some(second);
        self.node_mut(first).parent = Some(parent);
        self.node_mut(second).parent = Some(parent);
    }

    fn replace_parent_edge(
        &mut self,
        state: &mut TreeState,
        parent: Option<NodeId>,
        old: NodeId,
        new: NodeId,
    ) {
        let polarity = parent.map_or(ChildPolarity::First, |parent| {
            if self.node(parent).first_child == Some(old) {
                ChildPolarity::First
            } else {
                ChildPolarity::Second
            }
        });
        self.replace_edge(state, parent, polarity, new);
    }

    /// Frees `id`'s slot. Upstream `free()`s the node here.
    ///
    /// Only ever called on a node the caller has already unlinked from every
    /// tree edge, so no surviving node can still point at it.
    fn retire(&mut self, id: NodeId) {
        self.cancel_presel(id);
        if self.nodes.remove(id).is_some() {
            self.retired_nodes.push(id);
        }
    }

    /// Every node freed since the last call.
    ///
    /// The daemon drains this and drops history and stacking entries naming
    /// those nodes.
    pub fn take_retired_nodes(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.retired_nodes)
    }

    /// Whether any freed node still has to be swept out of the id holders.
    #[must_use]
    pub fn has_retired_nodes(&self) -> bool {
        !self.retired_nodes.is_empty()
    }

    /// Frees `root` and every node beneath it.
    ///
    /// This is the counterpart of [`Tree::unlink`], which detaches a subtree
    /// but leaves it in the arena so the caller can still read it: once the
    /// caller is done, it must hand the subtree back here or the nodes leak.
    /// Every id holder naming a node in the subtree must already have been
    /// purged; the daemon validates that invariant.
    pub fn destroy_subtree(&mut self, root: NodeId) {
        for id in self.preorder(root).collect::<Vec<_>>() {
            self.retire(id);
        }
    }

    #[must_use]
    pub fn is_leaf(&self, id: NodeId) -> bool {
        let node = self.node(id);
        node.first_child.is_none() && node.second_child.is_none()
    }

    /// Reads the child of `id` that sits on `polarity`'s side.
    #[must_use]
    pub fn child(&self, id: NodeId, polarity: ChildPolarity) -> Option<NodeId> {
        match polarity {
            ChildPolarity::First => self.node(id).first_child,
            ChildPolarity::Second => self.node(id).second_child,
        }
    }

    /// Reports whether `id` is its parent's child on `polarity`'s side.
    #[must_use]
    pub fn is_child_on(&self, id: NodeId, polarity: ChildPolarity) -> bool {
        self.node(id)
            .parent
            .is_some_and(|parent| self.child(parent, polarity) == Some(id))
    }

    #[must_use]
    pub fn is_first_child(&self, id: NodeId) -> bool {
        self.is_child_on(id, ChildPolarity::First)
    }

    #[must_use]
    pub fn is_second_child(&self, id: NodeId) -> bool {
        self.is_child_on(id, ChildPolarity::Second)
    }

    /// The side `id` occupies under its parent, defaulting to
    /// [`ChildPolarity::Second`] whenever `id` is not its parent's first child
    /// -- which is the fallback both callers used before they were merged.
    fn polarity_of(&self, id: NodeId) -> ChildPolarity {
        if self.is_first_child(id) {
            ChildPolarity::First
        } else {
            ChildPolarity::Second
        }
    }

    #[must_use]
    pub fn sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.node(id).parent?;
        self.child(parent, self.polarity_of(id).opposite())
    }

    /// Descends `polarity`'s edge until a leaf is reached.
    #[must_use]
    pub fn extreme(&self, mut id: NodeId, polarity: ChildPolarity) -> NodeId {
        while let Some(next) = self.child(id, polarity) {
            id = next;
        }
        id
    }

    #[must_use]
    pub fn first_extreme(&self, id: NodeId) -> NodeId {
        self.extreme(id, ChildPolarity::First)
    }

    #[must_use]
    pub fn second_extreme(&self, id: NodeId) -> NodeId {
        self.extreme(id, ChildPolarity::Second)
    }

    /// The in-order neighbour of `id` on `polarity`'s side, over the whole arena.
    #[must_use]
    pub fn adjacent_node(&self, id: NodeId, polarity: ChildPolarity) -> Option<NodeId> {
        if let Some(child) = self.child(id, polarity) {
            return Some(self.extreme(child, polarity.opposite()));
        }
        let mut current = id;
        while self.is_child_on(current, polarity) {
            current = self.node(current).parent?;
        }
        self.is_child_on(current, polarity.opposite())
            .then(|| self.node(current).parent)
            .flatten()
    }

    #[must_use]
    pub fn next_node(&self, id: NodeId) -> Option<NodeId> {
        self.adjacent_node(id, ChildPolarity::Second)
    }

    #[must_use]
    pub fn prev_node(&self, id: NodeId) -> Option<NodeId> {
        self.adjacent_node(id, ChildPolarity::First)
    }

    /// The neighbouring leaf of `id` on `polarity`'s side, within `root`.
    #[must_use]
    pub fn adjacent_leaf(
        &self,
        id: NodeId,
        root: NodeId,
        polarity: ChildPolarity,
    ) -> Option<NodeId> {
        let mut current = id;
        while self.is_child_on(current, polarity) && current != root {
            current = self.node(current).parent?;
        }
        if current == root {
            return None;
        }
        let parent = self.node(current).parent?;
        Some(self.extreme(self.child(parent, polarity)?, polarity.opposite()))
    }

    #[must_use]
    pub fn next_leaf(&self, id: NodeId, root: NodeId) -> Option<NodeId> {
        self.adjacent_leaf(id, root, ChildPolarity::Second)
    }

    #[must_use]
    pub fn prev_leaf(&self, id: NodeId, root: NodeId) -> Option<NodeId> {
        self.adjacent_leaf(id, root, ChildPolarity::First)
    }

    /// Like [`Tree::adjacent_leaf`], skipping receptacles and vacant leaves.
    #[must_use]
    pub fn adjacent_tiled_leaf(
        &self,
        id: NodeId,
        root: NodeId,
        polarity: ChildPolarity,
    ) -> Option<NodeId> {
        let mut candidate = self.adjacent_leaf(id, root, polarity);
        while candidate.is_some_and(|id| !self.is_tiled_leaf(id)) {
            candidate = self.adjacent_leaf(candidate?, root, polarity);
        }
        candidate
    }

    #[must_use]
    pub fn next_tiled_leaf(&self, id: NodeId, root: NodeId) -> Option<NodeId> {
        self.adjacent_tiled_leaf(id, root, ChildPolarity::Second)
    }

    #[must_use]
    pub fn prev_tiled_leaf(&self, id: NodeId, root: NodeId) -> Option<NodeId> {
        self.adjacent_tiled_leaf(id, root, ChildPolarity::First)
    }

    /// The predicate upstream's `*_tiled_leaf` walks apply: a leaf takes part in
    /// tiling only when it holds a client and is not vacant.
    #[must_use]
    pub fn is_tiled_leaf(&self, id: NodeId) -> bool {
        let node = self.node(id);
        node.client.is_some() && !node.vacant
    }

    /// Every leaf of `root`, in upstream's first-child-first order.
    pub fn leaves(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(self.first_extreme(root)), move |current| {
            self.next_leaf(*current, root)
        })
    }

    /// Every node of `root` in preorder: the node, then its first subtree, then
    /// its second. This is the order the recursive upstream walks visit.
    pub fn preorder(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut stack = vec![root];
        std::iter::from_fn(move || {
            let id = stack.pop()?;
            let node = self.node(id);
            stack.extend(node.second_child);
            stack.extend(node.first_child);
            Some(id)
        })
    }

    /// The leaves of `root` that take part in tiling, in leaf order.
    pub fn tiled_leaves(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.leaves(root).filter(move |id| self.is_tiled_leaf(*id))
    }

    #[must_use]
    pub fn first_focusable_leaf(&self, root: NodeId) -> Option<NodeId> {
        self.leaves(root).find(|id| {
            let node = self.node(*id);
            node.client.is_some() && !node.hidden
        })
    }

    #[must_use]
    pub fn is_focusable(&self, root: NodeId) -> bool {
        self.first_focusable_leaf(root).is_some()
    }

    #[must_use]
    pub fn clients_count(&self, root: NodeId) -> u32 {
        self.flag_count(root, |node| node.client.is_some())
    }

    #[must_use]
    pub fn tiled_count(&self, root: NodeId, include_receptacles: bool) -> i32 {
        self.leaves(root)
            .filter(|id| {
                let node = self.node(*id);
                !node.hidden
                    && (include_receptacles && node.client.is_none()
                        || node
                            .client
                            .as_ref()
                            .is_some_and(|client| client.state.is_tiled()))
            })
            .fold(0_i32, |count, _| count.wrapping_add(1))
    }

    fn flag_count(&self, root: NodeId, predicate: fn(&Node) -> bool) -> u32 {
        self.preorder(root).fold(0_u32, |count, id| {
            count.wrapping_add(u32::from(predicate(self.node(id))))
        })
    }

    #[must_use]
    pub fn sticky_count(&self, root: NodeId) -> u32 {
        self.flag_count(root, |node| node.sticky)
    }

    #[must_use]
    pub fn private_count(&self, root: NodeId) -> u32 {
        self.flag_count(root, |node| node.private)
    }

    #[must_use]
    pub fn placement_rectangle(&self, node: NodeId, window_gap: i32) -> Rectangle {
        let value = self.node(node);
        if let Some(client) = value.client.as_ref() {
            if client.state == ClientState::Floating {
                client.floating_rectangle
            } else {
                client.tiled_rectangle
            }
        } else {
            Rectangle {
                width: value.rectangle.width.saturating_sub(window_gap),
                height: value.rectangle.height.saturating_sub(window_gap),
                ..value.rectangle
            }
        }
    }

    /// Finds upstream's preferred non-private insertion leaf.
    #[must_use]
    pub fn find_public(&self, root: NodeId, window_gap: i32) -> Option<NodeId> {
        let mut manual = None;
        let mut manual_area = 0;
        let mut automatic = None;
        let mut automatic_area = 0;
        for node in self.leaves(root) {
            let value = self.node(node);
            if value.vacant {
                continue;
            }
            let area = crate::geometry::area(self.placement_rectangle(node, window_gap));
            if area > manual_area && (value.presel.is_some() || !value.private) {
                manual = Some(node);
                manual_area = area;
            }
            let private_in_parent = value
                .parent
                .is_some_and(|parent| self.private_count(parent) > 0);
            if area > automatic_area
                && value.presel.is_none()
                && !value.private
                && !private_in_parent
            {
                automatic = Some(node);
                automatic_area = area;
            }
        }
        automatic.or(manual)
    }

    #[must_use]
    pub fn is_protected_insertion_anchor(&self, node: NodeId) -> bool {
        let value = self.node(node);
        value.presel.is_none()
            && (value.private
                || value
                    .parent
                    .is_some_and(|parent| self.private_count(parent) > 0))
    }

    #[must_use]
    pub fn locked_count(&self, root: NodeId) -> u32 {
        self.flag_count(root, |node| node.locked)
    }

    #[must_use]
    pub fn is_child(&self, child: NodeId, parent: NodeId) -> bool {
        self.node(child).parent == Some(parent)
    }

    #[must_use]
    pub fn is_descendant(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            let Some(parent) = self.node(node).parent else {
                return false;
            };
            node = parent;
        }
    }

    #[must_use]
    pub fn find_by_external_id(&self, root: NodeId, external_id: u32) -> Option<NodeId> {
        self.preorder(root)
            .find(|id| self.node(*id).external_id == external_id)
    }

    #[must_use]
    pub fn is_adjacent(&self, first: NodeId, second: NodeId, direction: Direction) -> bool {
        let first = self.node(first).rectangle;
        let second = self.node(second).rectangle;
        match direction {
            Direction::East => first.right() == second.x,
            Direction::South => first.bottom() == second.y,
            Direction::West => second.right() == first.x,
            Direction::North => second.bottom() == first.y,
        }
    }

    #[must_use]
    pub fn find_fence(&self, id: NodeId, direction: Direction) -> Option<NodeId> {
        let rectangle = self.node(id).rectangle;
        let mut parent = self.node(id).parent;
        while let Some(ancestor) = parent {
            let node = self.node(ancestor);
            let matches = match direction {
                Direction::North => {
                    node.split_type == SplitType::Horizontal && node.rectangle.y < rectangle.y
                }
                Direction::West => {
                    node.split_type == SplitType::Vertical && node.rectangle.x < rectangle.x
                }
                Direction::South => {
                    node.split_type == SplitType::Horizontal
                        && node.rectangle.bottom() > rectangle.bottom()
                }
                Direction::East => {
                    node.split_type == SplitType::Vertical
                        && node.rectangle.right() > rectangle.right()
                }
            };
            if matches {
                return Some(ancestor);
            }
            parent = node.parent;
        }
        None
    }

    /// Keeps every non-vacant descendant fence at its old absolute position
    /// after the containing rectangle or an ancestor split ratio changes.
    #[allow(clippy::cast_possible_truncation)]
    pub fn adjust_ratios(&mut self, id: NodeId, rectangle: Rectangle) {
        if self.node(id).vacant {
            return;
        }
        let old = self.node(id).rectangle;
        let split_type = self.node(id).split_type;
        let ratio = self.node(id).split_ratio;
        let next_ratio = match split_type {
            SplitType::Vertical if rectangle.width != 0 => {
                let position = f64::from(old.x) + ratio * f64::from(old.width);
                (position - f64::from(rectangle.x)) / f64::from(rectangle.width)
            }
            SplitType::Horizontal if rectangle.height != 0 => {
                let position = f64::from(old.y) + ratio * f64::from(old.height);
                (position - f64::from(rectangle.y)) / f64::from(rectangle.height)
            }
            _ => ratio,
        }
        .clamp(0.0, 1.0);
        self.node_mut(id).split_ratio = next_ratio;

        let (first, second) = (self.node(id).first_child, self.node(id).second_child);
        match (first, second) {
            (Some(first), Some(second)) if self.node(first).vacant => {
                self.adjust_ratios(second, rectangle);
            }
            (Some(first), Some(second)) if self.node(second).vacant => {
                self.adjust_ratios(first, rectangle);
            }
            (Some(first), Some(second)) => {
                let (first_rectangle, second_rectangle) = match split_type {
                    SplitType::Vertical => {
                        let fence = (f64::from(rectangle.width) * next_ratio) as i32;
                        (
                            Rectangle::new(rectangle.x, rectangle.y, fence, rectangle.height),
                            Rectangle::new(
                                rectangle.x.saturating_add(fence),
                                rectangle.y,
                                rectangle.width.wrapping_sub(fence),
                                rectangle.height,
                            ),
                        )
                    }
                    SplitType::Horizontal => {
                        let fence = (f64::from(rectangle.height) * next_ratio) as i32;
                        (
                            Rectangle::new(rectangle.x, rectangle.y, rectangle.width, fence),
                            Rectangle::new(
                                rectangle.x,
                                rectangle.y.saturating_add(fence),
                                rectangle.width,
                                rectangle.height.wrapping_sub(fence),
                            ),
                        )
                    }
                };
                self.adjust_ratios(first, first_rectangle);
                self.adjust_ratios(second, second_rectangle);
            }
            _ => {}
        }
    }

    pub fn update_constraints(&mut self, id: NodeId) {
        let (Some(first), Some(second)) = (self.node(id).first_child, self.node(id).second_child)
        else {
            return;
        };
        let first = self.node(first).constraints;
        let second = self.node(second).constraints;
        self.node_mut(id).constraints = match self.node(id).split_type {
            SplitType::Vertical => Constraints {
                min_width: first.min_width.wrapping_add(second.min_width),
                min_height: first.min_height.max(second.min_height),
            },
            SplitType::Horizontal => Constraints {
                min_width: first.min_width.max(second.min_width),
                min_height: first.min_height.wrapping_add(second.min_height),
            },
        };
    }

    pub fn rebuild_constraints_from_leaves(&mut self, id: NodeId) {
        // Reversed preorder visits every child before its parent, which is what
        // the bottom-up recursion needs. `update_constraints` ignores leaves.
        for node in self.preorder(id).collect::<Vec<_>>().into_iter().rev() {
            self.update_constraints(node);
        }
    }

    pub fn rebuild_constraints_towards_root(&mut self, id: NodeId) {
        let mut parent = self.node(id).parent;
        while let Some(ancestor) = parent {
            self.update_constraints(ancestor);
            parent = self.node(ancestor).parent;
        }
    }

    pub fn rotate(&mut self, root: NodeId, degree: i32) {
        self.rotate_rec(root, degree);
        self.rebuild_constraints_from_leaves(root);
        self.rebuild_constraints_towards_root(root);
    }

    fn rotate_rec(&mut self, id: NodeId, degree: i32) {
        if degree == 0 || self.is_leaf(id) {
            return;
        }
        let split_type = self.node(id).split_type;
        let swap = degree == 180
            || degree == 90 && split_type == SplitType::Horizontal
            || degree == 270 && split_type == SplitType::Vertical;
        if swap {
            self.swap_children(id);
        }
        if degree != 180 {
            self.node_mut(id).split_type = match split_type {
                SplitType::Horizontal => SplitType::Vertical,
                SplitType::Vertical => SplitType::Horizontal,
            };
        }
        let first = self.node(id).first_child.expect("internal node");
        let second = self.node(id).second_child.expect("internal node");
        self.rotate_rec(first, degree);
        self.rotate_rec(second, degree);
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn flip(&mut self, root: NodeId, flip: Flip) {
        if self.is_leaf(root) {
            return;
        }
        let should_swap = matches!(
            (flip, self.node(root).split_type),
            (Flip::Horizontal, SplitType::Horizontal) | (Flip::Vertical, SplitType::Vertical)
        );
        if should_swap {
            self.swap_children(root);
        }
        let first = self.node(root).first_child.expect("internal node");
        let second = self.node(root).second_child.expect("internal node");
        self.flip(first, flip);
        self.flip(second, flip);
    }

    fn swap_children(&mut self, id: NodeId) {
        let node = self.node_mut(id);
        std::mem::swap(&mut node.first_child, &mut node.second_child);
        node.split_ratio = 1.0 - node.split_ratio;
    }

    pub fn equalize(&mut self, root: NodeId, split_ratio: f64) {
        // Preorder, but pruned at vacant nodes, so it cannot use `preorder`.
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if self.node(id).vacant {
                continue;
            }
            self.node_mut(id).split_ratio = split_ratio;
            stack.extend(self.node(id).second_child);
            stack.extend(self.node(id).first_child);
        }
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn balance(&mut self, root: NodeId) -> i32 {
        if self.node(root).vacant {
            return 0;
        }
        if self.is_leaf(root) {
            return 1;
        }
        let first = self.node(root).first_child.expect("internal node");
        let second = self.node(root).second_child.expect("internal node");
        let first_count = self.balance(first);
        let second_count = self.balance(second);
        if first_count > 0 && second_count > 0 {
            self.node_mut(root).split_ratio =
                f64::from(first_count) / f64::from(first_count + second_count);
        }
        first_count + second_count
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn apply_layout(&mut self, root: NodeId, rectangle: Rectangle, monocle: bool) {
        self.node_mut(root).rectangle = rectangle;
        let (Some(first), Some(second)) =
            (self.node(root).first_child, self.node(root).second_child)
        else {
            return;
        };
        let node = self.node(root);
        let (mut first_rectangle, mut second_rectangle) = (rectangle, rectangle);
        if !monocle && !self.node(first).vacant && !self.node(second).vacant {
            match node.split_type {
                SplitType::Vertical => {
                    let mut fence = (f64::from(rectangle.width) * node.split_ratio) as i32;
                    let first_min = i32::from(self.node(first).constraints.min_width);
                    let second_min = i32::from(self.node(second).constraints.min_width);
                    if first_min + second_min <= rectangle.width {
                        if fence < first_min {
                            fence = first_min;
                            self.node_mut(root).split_ratio =
                                f64::from(fence) / f64::from(rectangle.width);
                        } else if fence > rectangle.width - second_min {
                            fence = rectangle.width - second_min;
                            self.node_mut(root).split_ratio =
                                f64::from(fence) / f64::from(rectangle.width);
                        }
                    }
                    first_rectangle.width = fence;
                    second_rectangle.x = rectangle.x.saturating_add(fence);
                    second_rectangle.width = rectangle.width.wrapping_sub(fence);
                }
                SplitType::Horizontal => {
                    let mut fence = (f64::from(rectangle.height) * node.split_ratio) as i32;
                    let first_min = i32::from(self.node(first).constraints.min_height);
                    let second_min = i32::from(self.node(second).constraints.min_height);
                    if first_min + second_min <= rectangle.height {
                        if fence < first_min {
                            fence = first_min;
                            self.node_mut(root).split_ratio =
                                f64::from(fence) / f64::from(rectangle.height);
                        } else if fence > rectangle.height - second_min {
                            fence = rectangle.height - second_min;
                            self.node_mut(root).split_ratio =
                                f64::from(fence) / f64::from(rectangle.height);
                        }
                    }
                    first_rectangle.height = fence;
                    second_rectangle.y = rectangle.y.saturating_add(fence);
                    second_rectangle.height = rectangle.height.wrapping_sub(fence);
                }
            }
        }
        self.apply_layout(first, first_rectangle, monocle);
        self.apply_layout(second, second_rectangle, monocle);
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn validate(&self, root: NodeId) -> Result<(), &'static str> {
        let mut visited = HashSet::new();
        self.validate_node(root, None, &mut visited)
    }

    fn validate_node(
        &self,
        id: NodeId,
        expected_parent: Option<NodeId>,
        visited: &mut HashSet<NodeId>,
    ) -> Result<(), &'static str> {
        if !visited.insert(id) {
            return Err("node is reachable more than once");
        }
        let node = self.node(id);
        if node.parent != expected_parent {
            return Err("parent link does not match child link");
        }
        match (node.first_child, node.second_child) {
            (None, None) => Ok(()),
            (Some(first), Some(second)) => {
                self.validate_node(first, Some(id), visited)?;
                self.validate_node(second, Some(id), visited)
            }
            _ => Err("internal node does not have two children"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balanced_tree() -> (Tree, NodeId, [NodeId; 4], [NodeId; 2]) {
        let mut tree = Tree::default();
        let root = tree.add_node(100, 0.5);
        let left = tree.add_node(101, 0.5);
        let right = tree.add_node(102, 0.5);
        let a = tree.add_node(1, 0.5);
        let b = tree.add_node(2, 0.5);
        let c = tree.add_node(3, 0.5);
        let d = tree.add_node(4, 0.5);
        tree.set_children(left, a, b);
        tree.set_children(right, c, d);
        tree.set_children(root, left, right);
        (tree, root, [a, b, c, d], [left, right])
    }

    #[test]
    fn validates_full_tree_and_traverses_in_upstream_order() {
        let (tree, root, [a, b, c, d], [left, right]) = balanced_tree();
        assert_eq!(tree.validate(root), Ok(()));
        let mut order = vec![a];
        while let Some(next) = tree.next_node(*order.last().unwrap()) {
            order.push(next);
        }
        assert_eq!(order, [a, left, b, root, c, right, d]);
    }

    #[test]
    fn leaf_iteration_respects_subtree_boundaries() {
        let (tree, root, [a, b, c, d], [left, _]) = balanced_tree();
        assert_eq!(tree.next_leaf(a, root), Some(b));
        assert_eq!(tree.next_leaf(b, root), Some(c));
        assert_eq!(tree.next_leaf(c, root), Some(d));
        assert_eq!(tree.next_leaf(d, root), None);
        assert_eq!(tree.next_leaf(b, left), None);
    }

    #[test]
    fn descendant_is_reflexive_and_id_lookup_is_preorder() {
        let (mut tree, root, [a, _, _, _], [left, _]) = balanced_tree();
        tree.node_mut(a).external_id = 100;
        assert!(tree.is_descendant(a, root));
        assert!(tree.is_descendant(root, root));
        assert!(tree.is_child(a, left));
        assert_eq!(tree.find_by_external_id(root, 100), Some(root));
    }

    #[test]
    fn focus_and_counts_match_leaf_filtering_rules() {
        let (mut tree, root, [a, b, c, d], _) = balanced_tree();
        let settings = Settings::default();
        tree.node_mut(a).client = Some(Client::from_settings(&settings));
        tree.node_mut(b).client = Some(Client::from_settings(&settings));
        tree.node_mut(b).hidden = true;
        tree.node_mut(c).client = Some(Client::from_settings(&settings));
        tree.node_mut(c).client.as_mut().unwrap().state = ClientState::Floating;
        tree.node_mut(d).vacant = true;
        assert_eq!(tree.clients_count(root), 3);
        assert_eq!(tree.tiled_count(root, false), 1);
        assert_eq!(tree.tiled_count(root, true), 2);
        assert_eq!(tree.first_focusable_leaf(root), Some(a));
    }

    #[test]
    fn adjacency_only_checks_one_axis_like_upstream() {
        let (mut tree, _, [a, b, _, _], _) = balanced_tree();
        tree.node_mut(a).rectangle = Rectangle::new(0, 0, 10, 10);
        tree.node_mut(b).rectangle = Rectangle::new(10, 100, 10, 10);
        assert!(tree.is_adjacent(a, b, Direction::East));
    }

    #[test]
    fn constrained_and_unconstrained_layouts_clamp_or_preserve_ratios() {
        {
            let (mut tree, root, [a, b, _, _], [left, _]) = balanced_tree();
            tree.apply_layout(root, Rectangle::new(0, 0, 101, 101), false);
            assert_eq!(tree.node(left).rectangle.width, 50);
            tree.node_mut(left).split_ratio = 0.01;
            tree.apply_layout(left, Rectangle::new(0, 0, 101, 100), false);
            assert_eq!(tree.node(a).rectangle.width, 32);
            assert_eq!(tree.node(b).rectangle.width, 69);
            assert!((tree.node(left).split_ratio - 32.0 / 101.0).abs() < f64::EPSILON);
        }

        {
            let (mut tree, root, _, _) = balanced_tree();
            tree.node_mut(root).split_ratio = 0.333;
            tree.apply_layout(root, Rectangle::new(0, 0, 200, 200), false);
            assert!((tree.node(root).split_ratio - 0.333).abs() < f64::EPSILON);

            tree.node_mut(root).split_type = SplitType::Horizontal;
            tree.apply_layout(root, Rectangle::new(0, 0, 200, 200), false);
            assert!((tree.node(root).split_ratio - 0.333).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn vacancy_and_monocle_assign_the_full_rectangle_to_both_children() {
        let (mut tree, root, [a, b, _, _], [left, _]) = balanced_tree();
        tree.node_mut(a).vacant = true;
        let rectangle = Rectangle::new(5, 6, 100, 80);
        tree.apply_layout(left, rectangle, false);
        assert_eq!(tree.node(a).rectangle, rectangle);
        assert_eq!(tree.node(b).rectangle, rectangle);
        tree.node_mut(a).vacant = false;
        tree.apply_layout(root, rectangle, true);
        assert_eq!(tree.node(a).rectangle, rectangle);
    }

    #[test]
    fn rotate_flip_equalize_and_balance_preserve_tree_invariants() {
        let (mut tree, root, [a, b, _, d], [left, _]) = balanced_tree();
        tree.node_mut(left).split_type = SplitType::Horizontal;
        tree.rotate(root, 90);
        assert_eq!(tree.validate(root), Ok(()));
        assert_eq!(tree.first_extreme(root), b);
        tree.flip(root, Flip::Vertical);
        assert_eq!(tree.validate(root), Ok(()));
        tree.equalize(root, 0.6);
        assert!((tree.node(root).split_ratio - 0.6).abs() < f64::EPSILON);
        tree.node_mut(a).vacant = true;
        assert_eq!(tree.balance(root), 3);
        assert_eq!(tree.clients_count(root), 0);
        assert!(tree.is_descendant(b, root));
        assert!(tree.is_descendant(d, root));
    }

    #[test]
    fn insert_unlink_and_receptacle_replacement_preserve_stable_ids() {
        let mut tree = Tree::default();
        let anchor = tree.add_node(1, 0.5);
        let incoming = tree.add_node(2, 0.5);
        let branch = tree.add_node(3, 0.5);
        tree.node_mut(anchor).client = Some(Client::from_settings(&Settings::default()));
        tree.node_mut(incoming).client = Some(Client::from_settings(&Settings::default()));
        let mut state = TreeState::default();
        tree.insert(&mut state, anchor, None, None, ChildPolarity::Second)
            .unwrap();
        tree.insert(
            &mut state,
            incoming,
            Some(anchor),
            Some(branch),
            ChildPolarity::Second,
        )
        .unwrap();
        assert_eq!(state.root, Some(branch));
        assert_eq!(tree.validate(branch), Ok(()));
        let result = tree.unlink(&mut state, incoming).unwrap();
        assert_eq!(result.collapsed, Some(branch));
        assert_eq!(state.root, Some(anchor));
        assert_eq!(tree.node(incoming).parent, None);

        let receptacle = tree.add_node(4, 0.5);
        let replacement = tree.add_node(5, 0.5);
        tree.node_mut(replacement).client = Some(Client::from_settings(&Settings::default()));
        tree.unlink(&mut state, anchor).unwrap();
        tree.insert(&mut state, receptacle, None, None, ChildPolarity::Second)
            .unwrap();
        // A receptacle holds no client, so it is never focused automatically;
        // reaching this state requires an explicit focus command.
        state.focus = Some(receptacle);
        assert_eq!(
            tree.insert(
                &mut state,
                replacement,
                Some(receptacle),
                None,
                ChildPolarity::Second
            ),
            Ok(Some(receptacle))
        );
        assert_eq!(state.root, Some(replacement));
        // Replacing the focused receptacle hands the focus to its replacement
        // instead of leaving a populated tree with nothing focused.
        assert_eq!(state.focus, Some(replacement));
    }

    #[test]
    fn automatic_insertion_honors_longest_side_and_alternate_schemes() {
        let settings = Settings::default();
        for (scheme, expected) in [
            (AutomaticScheme::LongestSide, SplitType::Horizontal),
            (AutomaticScheme::Alternate, SplitType::Horizontal),
        ] {
            let mut tree = Tree::default();
            let first = tree.add_node(1, 0.5);
            let second = tree.add_node(2, 0.5);
            let root = tree.add_node(3, 0.5);
            for leaf in [first, second] {
                tree.node_mut(leaf).client = Some(Client::from_settings(&settings));
            }
            tree.node_mut(first).rectangle = Rectangle::new(0, 0, 800, 600);
            let mut state = TreeState::default();
            tree.insert(&mut state, first, None, None, ChildPolarity::Second)
                .unwrap();
            tree.insert_automatic(
                &mut state,
                second,
                first,
                root,
                ChildPolarity::Second,
                AutomaticScheme::LongestSide,
            )
            .unwrap();
            assert_eq!(tree.node(root).split_type, SplitType::Vertical);

            tree.node_mut(second).rectangle = Rectangle::new(400, 0, 400, 600);
            let third = tree.add_node(4, 0.5);
            let branch = tree.add_node(5, 0.5);
            tree.node_mut(third).client = Some(Client::from_settings(&settings));
            tree.insert_automatic(
                &mut state,
                third,
                second,
                branch,
                ChildPolarity::Second,
                scheme,
            )
            .unwrap();
            assert_eq!(tree.node(branch).split_type, expected);
            assert_eq!(tree.validate(state.root.unwrap()), Ok(()));
        }
    }

    #[test]
    fn find_public_prefers_clean_automatic_leaves_over_larger_manual_ones() {
        let settings = Settings::default();
        let (mut tree, root, [automatic, other, manual, private], _) = balanced_tree();
        for leaf in [automatic, other, manual, private] {
            tree.node_mut(leaf).client = Some(Client::from_settings(&settings));
        }
        tree.node_mut(automatic)
            .client
            .as_mut()
            .unwrap()
            .tiled_rectangle = Rectangle::new(0, 0, 10, 10);
        tree.node_mut(other)
            .client
            .as_mut()
            .unwrap()
            .tiled_rectangle = Rectangle::new(0, 0, 5, 10);
        tree.node_mut(manual)
            .client
            .as_mut()
            .unwrap()
            .tiled_rectangle = Rectangle::new(0, 0, 100, 100);
        tree.node_mut(private).private = true;

        assert_eq!(tree.find_public(root, 0), Some(automatic));
        tree.node_mut(automatic).vacant = true;
        assert_eq!(tree.find_public(root, 0), Some(other));
    }

    #[test]
    fn making_a_subtree_vacant_cancels_its_preselections() {
        let settings = Settings::default();
        let (mut tree, root, leaves @ [first, second, _, _], _) = balanced_tree();
        for leaf in leaves {
            tree.node_mut(leaf).client = Some(Client::from_settings(&settings));
        }
        tree.node_mut(root).presel = Some(Presel {
            split_dir: Direction::East,
            split_ratio: 0.5,
            feedback: Some(10),
        });
        tree.node_mut(first).presel = Some(Presel {
            split_dir: Direction::South,
            split_ratio: 0.3,
            feedback: Some(11),
        });
        tree.node_mut(second).presel = Some(Presel {
            split_dir: Direction::North,
            split_ratio: 0.7,
            feedback: Some(12),
        });

        assert!(tree.set_flag(root, NodeFlag::Hidden, true));
        assert!(tree.node(root).presel.is_none());
        assert!(tree.node(first).presel.is_none());
        assert!(tree.node(second).presel.is_none());
        let mut retired = tree.take_retired_feedbacks();
        retired.sort_unstable();
        assert_eq!(retired, vec![10, 11, 12]);
    }

    #[test]
    fn spiral_insertion_promotes_and_rotates_the_anchors_parent() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let parent = tree.add_node(10, 0.4);
        let first = tree.add_node(1, 0.5);
        let anchor = tree.add_node(2, 0.5);
        let incoming = tree.add_node(3, 0.5);
        let branch = tree.add_node(11, 0.5);
        for leaf in [first, anchor, incoming] {
            tree.node_mut(leaf).client = Some(Client::from_settings(&settings));
        }
        tree.set_children(parent, first, anchor);
        tree.node_mut(parent).split_type = SplitType::Vertical;
        tree.node_mut(parent).split_ratio = 0.4;
        let mut state = TreeState {
            root: Some(parent),
            focus: Some(anchor),
        };

        tree.insert_automatic(
            &mut state,
            incoming,
            anchor,
            branch,
            ChildPolarity::Second,
            AutomaticScheme::Spiral,
        )
        .unwrap();

        assert_eq!(state.root, Some(branch));
        assert_eq!(tree.node(branch).first_child, Some(parent));
        assert_eq!(tree.node(branch).second_child, Some(incoming));
        assert_eq!(tree.node(branch).split_type, SplitType::Vertical);
        assert!((tree.node(branch).split_ratio - 0.4).abs() < f64::EPSILON);
        assert_eq!(tree.node(parent).split_type, SplitType::Horizontal);
        assert_eq!(tree.node(parent).first_child, Some(anchor));
        assert_eq!(tree.node(parent).second_child, Some(first));
        assert_eq!(tree.validate(branch), Ok(()));
    }

    /// `unlink` collapses the parent branch and `insert` consumes a bare
    /// receptacle; both used to leave a blanked node in the arena forever.
    #[test]
    fn collapsed_branches_and_consumed_receptacles_leave_the_arena() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let anchor = tree.add_node(1, 0.5);
        let incoming = tree.add_node(2, 0.5);
        let branch = tree.add_node(3, 0.5);
        tree.node_mut(anchor).client = Some(Client::from_settings(&settings));
        tree.node_mut(incoming).client = Some(Client::from_settings(&settings));
        let mut state = TreeState::default();
        tree.insert(&mut state, anchor, None, None, ChildPolarity::Second)
            .unwrap();
        tree.insert(
            &mut state,
            incoming,
            Some(anchor),
            Some(branch),
            ChildPolarity::Second,
        )
        .unwrap();
        assert_eq!(tree.len(), 3);

        assert_eq!(
            tree.unlink(&mut state, incoming).unwrap().collapsed,
            Some(branch)
        );
        assert_eq!(tree.len(), 2);
        assert!(!tree.is_live(branch));
        assert_eq!(tree.get(branch), None);
        assert!(!tree.contains(&state, branch));
        assert_eq!(tree.take_retired_nodes(), vec![branch]);

        // The detached subtree is still the caller's to destroy.
        tree.destroy_subtree(incoming);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.take_retired_nodes(), vec![incoming]);

        let receptacle = tree.add_node(4, 0.5);
        let replacement = tree.add_node(5, 0.5);
        tree.node_mut(replacement).client = Some(Client::from_settings(&settings));
        tree.destroy_subtree(anchor);
        state = TreeState::default();
        tree.insert(&mut state, receptacle, None, None, ChildPolarity::Second)
            .unwrap();
        tree.insert(
            &mut state,
            replacement,
            Some(receptacle),
            None,
            ChildPolarity::Second,
        )
        .unwrap();
        assert!(!tree.is_live(receptacle));
        assert_eq!(tree.len(), 1);
    }

    /// A retired slot's key must never resolve to whatever later reuses it.
    #[test]
    fn a_reused_slot_does_not_answer_to_the_retired_key() {
        let mut tree = Tree::default();
        let first = tree.add_node(1, 0.5);
        tree.destroy_subtree(first);
        let second = tree.add_node(2, 0.5);
        assert_ne!(first, second);
        assert_eq!(tree.get(first), None);
        assert_eq!(tree.node(second).external_id, 2);
    }

    #[test]
    #[should_panic(expected = "node id outlived its node")]
    fn resolving_a_retired_node_panics_with_a_named_cause() {
        let mut tree = Tree::default();
        let node = tree.add_node(1, 0.5);
        tree.destroy_subtree(node);
        let _ = tree.node(node);
    }

    #[test]
    fn preselected_receptacle_is_split_and_preselection_is_consumed() {
        let mut tree = Tree::default();
        let receptacle = tree.add_node(1, 0.5);
        let incoming = tree.add_node(2, 0.5);
        let branch = tree.add_node(3, 0.5);
        tree.node_mut(incoming).client = Some(Client::from_settings(&Settings::default()));
        tree.set_presel_direction(receptacle, Direction::North, 0.5);
        tree.set_presel_ratio(receptacle, 0.3, 0.5);
        tree.node_mut(receptacle).presel.as_mut().unwrap().feedback = Some(99);
        let mut state = TreeState::default();
        tree.insert(&mut state, receptacle, None, None, ChildPolarity::Second)
            .unwrap();

        assert_eq!(
            tree.insert(
                &mut state,
                incoming,
                Some(receptacle),
                Some(branch),
                ChildPolarity::Second,
            ),
            Ok(None)
        );
        assert_eq!(state.root, Some(branch));
        assert_eq!(tree.node(branch).first_child, Some(incoming));
        assert_eq!(tree.node(branch).second_child, Some(receptacle));
        assert_eq!(tree.node(branch).split_type, SplitType::Horizontal);
        assert!((tree.node(branch).split_ratio - 0.3).abs() < f64::EPSILON);
        assert_eq!(tree.node(receptacle).presel, None);
        assert_eq!(tree.take_retired_feedbacks(), vec![99]);
        assert!(tree.take_retired_feedbacks().is_empty());
        assert_eq!(tree.validate(branch), Ok(()));
    }

    #[test]
    fn within_tree_swaps_reject_ancestors_and_preserve_edges_and_focus() {
        {
            let (mut tree, root, [a, b, c, _], _) = balanced_tree();
            let mut state = TreeState {
                root: Some(root),
                focus: Some(a),
            };
            assert_eq!(
                tree.swap_within(&mut state, root, a),
                Err(StructuralError::InvalidSwap)
            );
            tree.swap_within(&mut state, a, c).unwrap();
            assert_eq!(state.focus, Some(a));
            assert_eq!(tree.validate(root), Ok(()));
            assert!(tree.is_descendant(b, root));
        }

        {
            let (mut tree, root, [a, b, _, _], [left, _]) = balanced_tree();
            let mut state = TreeState {
                root: Some(root),
                focus: Some(a),
            };
            tree.swap_within(&mut state, a, b).unwrap();
            assert_eq!(tree.node(left).first_child, Some(b));
            assert_eq!(tree.node(left).second_child, Some(a));
            assert_eq!(state.focus, Some(a));
            assert_eq!(tree.validate(root), Ok(()));
        }
    }

    #[test]
    fn recursive_ratio_adjustment_preserves_descendant_fences() {
        let mut tree = Tree::default();
        let root = tree.add_node(1, 0.5);
        let left = tree.add_node(2, 0.5);
        let right = tree.add_node(3, 0.5);
        let top_left = tree.add_node(4, 0.5);
        let bottom_left = tree.add_node(5, 0.5);
        tree.set_children(left, top_left, bottom_left);
        tree.node_mut(left).split_type = SplitType::Vertical;
        tree.set_children(root, left, right);
        tree.node_mut(root).split_type = SplitType::Vertical;
        tree.apply_layout(root, Rectangle::new(0, 0, 200, 100), false);

        tree.node_mut(root).split_ratio = 0.75;
        tree.adjust_ratios(root, Rectangle::new(0, 0, 200, 100));

        assert!((tree.node(left).split_ratio - (1.0 / 3.0)).abs() < f64::EPSILON);
        tree.apply_layout(root, Rectangle::new(0, 0, 200, 100), false);
        assert_eq!(tree.node(top_left).rectangle.width, 50);
    }

    #[test]
    fn cross_tree_swap_maps_focus_like_upstream() {
        let (mut tree, first_root, [a, _, _, _], [first, _]) = balanced_tree();
        let second_root = tree.add_node(200, 0.5);
        let second = tree.add_node(5, 0.5);
        let other = tree.add_node(6, 0.5);
        tree.set_children(second_root, second, other);
        let mut first_state = TreeState {
            root: Some(first_root),
            focus: Some(a),
        };
        let mut second_state = TreeState {
            root: Some(second_root),
            focus: Some(other),
        };

        tree.swap_between(&mut first_state, first, &mut second_state, second)
            .unwrap();

        assert_eq!(first_state.focus, Some(second));
        assert_eq!(second_state.focus, Some(other));
        assert!(tree.is_descendant(second, first_root));
        assert!(tree.is_descendant(first, second_root));
        assert_eq!(tree.validate(first_root), Ok(()));
        assert_eq!(tree.validate(second_root), Ok(()));
    }

    #[test]
    fn state_changes_cancel_preselection_and_model_updates_preserve_invariants() {
        let (mut tree, root, [a, b, c, _], _) = balanced_tree();
        for node in [a, b, c] {
            tree.node_mut(node).client = Some(Client::from_settings(&Settings::default()));
        }
        let mut state = TreeState {
            root: Some(root),
            focus: Some(a),
        };
        tree.set_presel_direction(a, Direction::East, 0.5);
        assert!(tree.set_state(a, ClientState::Floating));
        assert!(tree.node(a).vacant);
        assert!(tree.node(a).presel.is_none());
        assert_eq!(
            tree.node(a).client.as_ref().unwrap().last_state,
            ClientState::Tiled
        );
        assert!(tree.set_layer(a, StackLayer::Above));
        assert!(tree.set_flag(a, NodeFlag::Hidden, true));
        assert!(tree.node(a).hidden);
        assert!(tree.set_flag(a, NodeFlag::Hidden, false));
        assert!(tree.circulate(&mut state, root, CirculateDirection::Forward));
        assert_eq!(tree.validate(root), Ok(()));
        assert!(
            state
                .focus
                .is_some_and(|focus| tree.is_descendant(focus, root))
        );
    }

    #[test]
    fn state_changes_leave_hidden_and_tiled_to_pseudo_tiled_vacancy_alone() {
        let settings = Settings::default();
        let (mut tree, _, [a, b, _, _], _) = balanced_tree();
        for node in [a, b] {
            tree.node_mut(node).client = Some(Client::from_settings(&settings));
        }

        // Hiding a tiled client makes it vacant. Upstream guards both vacancy
        // setters with `if (!n->hidden)`, so no later state change may hand a
        // hidden client tiling space back.
        assert!(tree.set_flag(a, NodeFlag::Hidden, true));
        assert!(tree.node(a).vacant);
        assert!(tree.set_state(a, ClientState::Floating));
        assert!(tree.node(a).vacant);
        assert!(tree.set_state(a, ClientState::Tiled));
        assert!(tree.node(a).vacant);

        // A Tiled <-> PseudoTiled move calls neither `set_floating` nor
        // `set_fullscreen` upstream, so it must not touch vacancy either.
        tree.node_mut(b).vacant = true;
        assert!(tree.set_state(b, ClientState::PseudoTiled));
        assert!(tree.node(b).vacant);
        assert!(tree.set_state(b, ClientState::Tiled));
        assert!(tree.node(b).vacant);
    }

    #[test]
    fn circulate_skips_vacant_leaves_like_upstream_tiled_walks() {
        let settings = Settings::default();
        let (mut tree, root, [a, b, c, d], _) = balanced_tree();
        for node in [a, b, c, d] {
            tree.node_mut(node).client = Some(Client::from_settings(&settings));
        }
        // `d` becomes a hidden tiled client, hence vacant. Upstream circulates
        // with `prev_tiled_leaf`/`next_tiled_leaf`, which step over vacant
        // leaves, so `d` must keep its slot while the rest rotate.
        assert!(tree.set_flag(d, NodeFlag::Hidden, true));
        let mut state = TreeState {
            root: Some(root),
            focus: Some(a),
        };

        assert!(tree.circulate(&mut state, root, CirculateDirection::Forward));

        assert_eq!(tree.leaves(root).collect::<Vec<_>>(), [b, c, a, d]);
        assert_eq!(tree.validate(root), Ok(()));
    }

    #[test]
    fn preorder_and_leaf_iterators_match_the_recursive_walks() {
        let (tree, root, [a, b, c, d], [left, right]) = balanced_tree();
        assert_eq!(
            tree.preorder(root).collect::<Vec<_>>(),
            [root, left, a, b, right, c, d]
        );
        assert_eq!(tree.leaves(root).collect::<Vec<_>>(), [a, b, c, d]);
        assert_eq!(tree.leaves(left).collect::<Vec<_>>(), [a, b]);
        assert_eq!(tree.preorder(a).collect::<Vec<_>>(), [a]);
    }
}
