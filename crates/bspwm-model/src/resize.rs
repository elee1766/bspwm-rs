use bspwm_core::pointer::ResizeInput;
use bspwm_core::types::{Direction, ResizeHandle};

use crate::tree::{NodeId, Tree};

/// A split ratio change. Applying it and arranging the desktop is a runtime task.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RatioUpdate {
    pub node: NodeId,
    pub ratio: f64,
}

/// Pure tiled-resize output, with at most one fence for each axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TiledResizePlan {
    pub vertical: Option<RatioUpdate>,
    pub horizontal: Option<RatioUpdate>,
}

/// Plans ratio changes for the fences selected by a tiled resize handle.
#[must_use]
pub fn plan_tiled_resize(
    tree: &Tree,
    node: NodeId,
    handle: ResizeHandle,
    input: ResizeInput,
) -> TiledResizePlan {
    let vertical = if handle.contains(ResizeHandle::LEFT) {
        tree.find_fence(node, Direction::West)
    } else if handle.contains(ResizeHandle::RIGHT) {
        tree.find_fence(node, Direction::East)
    } else {
        None
    };
    let horizontal = if handle.contains(ResizeHandle::TOP) {
        tree.find_fence(node, Direction::North)
    } else if handle.contains(ResizeHandle::BOTTOM) {
        tree.find_fence(node, Direction::South)
    } else {
        None
    };
    TiledResizePlan {
        vertical: vertical.map(|fence| ratio_update(tree, fence, input, true)),
        horizontal: horizontal.map(|fence| ratio_update(tree, fence, input, false)),
    }
}

/// Applies planned ratios. Call `Tree::apply_layout` afterwards to arrange windows.
pub fn apply_tiled_resize_plan(tree: &mut Tree, plan: TiledResizePlan) {
    for update in plan.vertical.into_iter().chain(plan.horizontal) {
        tree.node_mut(update.node).split_ratio = update.ratio;
    }
}

fn ratio_update(tree: &Tree, fence: NodeId, input: ResizeInput, vertical: bool) -> RatioUpdate {
    let node = tree.node(fence);
    let ratio = match input {
        ResizeInput::Relative { dx, dy } => {
            let delta = if vertical { dx } else { dy };
            let extent = if vertical {
                node.rectangle.width
            } else {
                node.rectangle.height
            };
            node.split_ratio + f64::from(delta) / f64::from(extent)
        }
        ResizeInput::Absolute(position) => {
            let (coordinate, origin, extent) = if vertical {
                (position.x, node.rectangle.x, node.rectangle.width)
            } else {
                (position.y, node.rectangle.y, node.rectangle.height)
            };
            f64::from(coordinate - origin) / f64::from(extent)
        }
    };
    RatioUpdate {
        node: fence,
        ratio: ratio.clamp(0.0, 1.0),
    }
}
