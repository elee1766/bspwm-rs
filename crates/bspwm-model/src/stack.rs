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
}
