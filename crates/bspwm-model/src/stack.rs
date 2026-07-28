use crate::tree::{Client, NodeId, Tree};
use crate::types::{ClientState, StackLayer};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StackingOrder {
    nodes: Vec<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestackAction {
    Below { node: NodeId, sibling: NodeId },
    Above { node: NodeId, sibling: NodeId },
}

impl StackingOrder {
    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }

    #[must_use]
    pub fn from_nodes(nodes: Vec<NodeId>) -> Self {
        Self { nodes }
    }

    pub fn remove_subtree(&mut self, tree: &Tree, root: NodeId) {
        self.nodes.retain(|node| !tree.is_descendant(*node, root));
    }

    /// Drops every entry naming one of `dead`.
    ///
    /// Only branches and receptacles reach this in practice -- the stacking
    /// order holds client leaves, and those are freed through
    /// [`StackingOrder::remove_subtree`] -- but the arena frees slots, so
    /// nothing may be left pointing at one.
    pub fn forget_nodes(&mut self, dead: &[NodeId]) {
        if dead.is_empty() {
            return;
        }
        self.nodes.retain(|node| !dead.contains(node));
    }

    pub fn stack(
        &mut self,
        tree: &Tree,
        root: NodeId,
        focused: bool,
        auto_raise: bool,
    ) -> Vec<RestackAction> {
        let mut actions = Vec::new();
        for node in tree.leaves(root) {
            if let Some(client) = tree.node(node).client.as_ref()
                && (auto_raise || client.state != ClientState::Floating)
            {
                let was_empty = self.nodes.is_empty();
                self.place(node, stack_level(client), tree, focused);
                if !was_empty && let Some(action) = self.restack_node(tree, node) {
                    actions.push(action);
                }
            }
        }
        // Enforce transient-above-parent: after placing the requested subtree,
        // pull each managed transient child immediately above its parent.
        self.enforce_transient_order(tree, &mut actions);
        actions
    }

    fn place(&mut self, node: NodeId, level: u8, tree: &Tree, focused: bool) {
        self.nodes.retain(|candidate| *candidate != node);
        let index = if focused {
            self.nodes
                .iter()
                .rposition(|candidate| node_level(tree, *candidate) <= level)
                .map_or(0, |index| index + 1)
        } else {
            self.nodes
                .iter()
                .position(|candidate| node_level(tree, *candidate) >= level)
                .unwrap_or(self.nodes.len())
        };
        self.nodes.insert(index, node);
    }

    /// Ensures every transient child in the stacking order appears above its
    /// managed parent. Iterates until no repairs are needed (handles chains).
    fn enforce_transient_order(&mut self, tree: &Tree, actions: &mut Vec<RestackAction>) {
        // Build a map from parent external-XID to stacking-order index, then
        // walk the list moving children that violate the constraint.
        // Limit iterations to avoid cycles from malformed WM_TRANSIENT_FOR.
        for _ in 0..self.nodes.len().saturating_add(1) {
            let mut moved = false;
            // Scan for the first child whose parent appears *after* it.
            let mut i = 0;
            while i < self.nodes.len() {
                let child = self.nodes[i];
                let Some(parent_xid) = tree
                    .get(child)
                    .and_then(|n| n.client.as_ref())
                    .and_then(|c| c.transient_for)
                else {
                    i += 1;
                    continue;
                };
                // Find the parent in the stacking order by external_id.
                let parent_pos = self.nodes.iter().position(|candidate| {
                    *candidate != child
                        && tree
                            .get(*candidate)
                            .is_some_and(|n| n.external_id == parent_xid)
                });
                let Some(parent_pos) = parent_pos else {
                    i += 1;
                    continue;
                };
                if i > parent_pos {
                    // Child is already above parent (higher index = higher).
                    i += 1;
                    continue;
                }
                // Child is below its parent: move it immediately after the parent.
                let child_node = self.nodes.remove(i);
                // After removal, parent_pos shifted down by 1 if it was after i.
                let insert_at = if parent_pos > i {
                    parent_pos // was parent_pos-1+1
                } else {
                    parent_pos + 1
                };
                self.nodes.insert(insert_at, child_node);
                if let Some(action) = self.restack_node(tree, child_node) {
                    actions.push(action);
                }
                moved = true;
                break; // restart scan after each move
            }
            if !moved {
                break;
            }
        }
    }

    fn restack_node(&self, tree: &Tree, node: NodeId) -> Option<RestackAction> {
        if !is_visible(tree, node) {
            return None;
        }
        let index = self.nodes.iter().position(|candidate| *candidate == node)?;
        if let Some(sibling) = self.nodes[index + 1..]
            .iter()
            .copied()
            .find(|candidate| is_visible(tree, *candidate))
        {
            return Some(RestackAction::Below { node, sibling });
        }
        self.nodes[..index]
            .iter()
            .rev()
            .copied()
            .find(|candidate| is_visible(tree, *candidate))
            .map(|sibling| RestackAction::Above { node, sibling })
    }
}

#[must_use]
pub const fn stack_level(client: &Client) -> u8 {
    let layer = match client.layer {
        StackLayer::Below => 0,
        StackLayer::Normal => 1,
        StackLayer::Above => 2,
    };
    let state = if client.state.is_tiled() {
        0
    } else if matches!(client.state, ClientState::Floating) {
        1
    } else {
        2
    };
    3 * layer + state
}

fn node_level(tree: &Tree, node: NodeId) -> u8 {
    tree.node(node).client.as_ref().map_or(0, stack_level)
}

fn is_visible(tree: &Tree, node: NodeId) -> bool {
    let node = tree.node(node);
    !node.hidden && node.client.as_ref().is_some_and(|client| client.shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn stack_levels_preserve_layer_before_state_order() {
        let mut client = Client::from_settings(&Settings::default());
        assert_eq!(stack_level(&client), 3);
        client.state = ClientState::Fullscreen;
        assert_eq!(stack_level(&client), 5);
        client.layer = StackLayer::Above;
        client.state = ClientState::Tiled;
        assert_eq!(stack_level(&client), 6);
    }

    #[test]
    fn focused_and_unfocused_move_within_the_same_level_class() {
        let mut tree = Tree::default();
        let first = tree.add_node(1, 0.5);
        let second = tree.add_node(2, 0.5);
        let root = tree.add_node(3, 0.5);
        tree.node_mut(first).client = Some(Client::from_settings(&Settings::default()));
        tree.node_mut(second).client = Some(Client::from_settings(&Settings::default()));
        tree.set_children(root, first, second);
        let mut stack = StackingOrder::default();
        let _ = stack.stack(&tree, root, true, true);
        assert_eq!(stack.nodes(), &[first, second]);
        let _ = stack.stack(&tree, first, true, true);
        assert_eq!(stack.nodes(), &[second, first]);
        let _ = stack.stack(&tree, first, false, true);
        assert_eq!(stack.nodes(), &[first, second]);
    }

    #[test]
    fn restacking_skips_hidden_and_unshown_siblings() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let below = tree.add_node(1, 0.5);
        let hidden = tree.add_node(2, 0.5);
        let unshown = tree.add_node(3, 0.5);
        let above = tree.add_node(4, 0.5);
        for node in [below, hidden, unshown, above] {
            tree.node_mut(node).client = Some(Client::from_settings(&settings));
        }
        tree.node_mut(below).client.as_mut().unwrap().shown = true;
        tree.node_mut(hidden).client.as_mut().unwrap().shown = true;
        tree.node_mut(hidden).hidden = true;
        tree.node_mut(above).client.as_mut().unwrap().shown = true;

        let stack = StackingOrder {
            nodes: vec![below, hidden, unshown, above],
        };
        assert_eq!(
            stack.restack_node(&tree, below),
            Some(RestackAction::Below {
                node: below,
                sibling: above,
            })
        );
        assert_eq!(
            stack.restack_node(&tree, above),
            Some(RestackAction::Above {
                node: above,
                sibling: below,
            })
        );
        assert_eq!(stack.restack_node(&tree, hidden), None);
        assert_eq!(stack.restack_node(&tree, unshown), None);
    }

    #[test]
    fn transient_child_placed_above_parent() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let parent = tree.add_node(0x100, 0.5);
        let child = tree.add_node(0x200, 0.5);
        let root = tree.add_node(0x300, 0.5);
        for node in [parent, child] {
            let mut c = Client::from_settings(&settings);
            c.state = ClientState::Floating;
            c.shown = true;
            tree.node_mut(node).client = Some(c);
        }
        // child is transient for parent
        tree.node_mut(child).client.as_mut().unwrap().transient_for = Some(0x100);
        tree.set_children(root, parent, child);

        let mut stack = StackingOrder::default();
        let _ = stack.stack(&tree, root, true, true);
        let pi = stack.nodes().iter().position(|n| *n == parent).unwrap();
        let ci = stack.nodes().iter().position(|n| *n == child).unwrap();
        assert!(
            ci > pi,
            "transient child must be above parent: parent@{pi} child@{ci}"
        );
    }

    #[test]
    fn raising_parent_carries_transient_child() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let other = tree.add_node(0x10, 0.5);
        let parent = tree.add_node(0x100, 0.5);
        let child = tree.add_node(0x200, 0.5);
        let root = tree.add_node(0x300, 0.5);
        for node in [other, parent, child] {
            let mut c = Client::from_settings(&settings);
            c.state = ClientState::Floating;
            c.shown = true;
            tree.node_mut(node).client = Some(c);
        }
        tree.node_mut(child).client.as_mut().unwrap().transient_for = Some(0x100);
        // Build a tree with all three under root.
        let branch = tree.add_node(0x400, 0.5);
        tree.set_children(branch, parent, child);
        tree.set_children(root, other, branch);

        let mut stack = StackingOrder::default();
        // Initially stack everyone unfocused, so 'other' ends up at bottom.
        let _ = stack.stack(&tree, root, false, true);
        // Now raise the parent (focused).
        let _ = stack.stack(&tree, parent, true, true);
        let oi = stack.nodes().iter().position(|n| *n == other).unwrap();
        let pi = stack.nodes().iter().position(|n| *n == parent).unwrap();
        let ci = stack.nodes().iter().position(|n| *n == child).unwrap();
        assert!(
            pi > oi,
            "parent must be above other: other@{oi} parent@{pi}"
        );
        assert!(
            ci > pi,
            "transient child must remain above parent: parent@{pi} child@{ci}"
        );
    }

    #[test]
    fn transient_chain_nested_order() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let grandparent = tree.add_node(0x10, 0.5);
        let parent = tree.add_node(0x20, 0.5);
        let child = tree.add_node(0x30, 0.5);
        for node in [grandparent, parent, child] {
            let mut c = Client::from_settings(&settings);
            c.state = ClientState::Floating;
            c.shown = true;
            tree.node_mut(node).client = Some(c);
        }
        tree.node_mut(parent).client.as_mut().unwrap().transient_for = Some(0x10);
        tree.node_mut(child).client.as_mut().unwrap().transient_for = Some(0x20);

        let root = tree.add_node(0x40, 0.5);
        let b1 = tree.add_node(0x50, 0.5);
        tree.set_children(b1, parent, child);
        tree.set_children(root, grandparent, b1);

        let mut stack = StackingOrder::default();
        let _ = stack.stack(&tree, root, true, true);
        let gi = stack
            .nodes()
            .iter()
            .position(|n| *n == grandparent)
            .unwrap();
        let pi = stack.nodes().iter().position(|n| *n == parent).unwrap();
        let ci = stack.nodes().iter().position(|n| *n == child).unwrap();
        assert!(pi > gi, "parent above grandparent");
        assert!(ci > pi, "child above parent");
    }

    #[test]
    fn unmanaged_parent_degrades_gracefully() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let node = tree.add_node(0x100, 0.5);
        let mut c = Client::from_settings(&settings);
        c.state = ClientState::Floating;
        c.shown = true;
        // Reference a non-existent parent.
        c.transient_for = Some(0xDEAD);
        tree.node_mut(node).client = Some(c);

        let mut stack = StackingOrder::default();
        // Must not panic.
        let _ = stack.stack(&tree, node, true, true);
        assert_eq!(stack.nodes(), &[node]);
    }

    #[test]
    fn self_cycle_does_not_loop() {
        let settings = Settings::default();
        let mut tree = Tree::default();
        let node = tree.add_node(0x100, 0.5);
        let mut c = Client::from_settings(&settings);
        c.state = ClientState::Floating;
        c.shown = true;
        c.transient_for = Some(0x100); // self-reference
        tree.node_mut(node).client = Some(c);

        let mut stack = StackingOrder::default();
        let _ = stack.stack(&tree, node, true, true);
        assert_eq!(stack.nodes(), &[node]);
    }
}
