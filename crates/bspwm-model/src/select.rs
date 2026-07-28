#![allow(clippy::missing_panics_doc)]

use bspwm_core::geometry::{area, boundary_distance, on_dir_side};
use bspwm_core::types::{
    ClientState, CycleDirection, DesktopSelect, Direction, Layout, MonitorSelect, NodeSelect,
    OptionBool, Rectangle, SplitType, StackLayer, Tightness,
};

use crate::tree::NodeId;
use crate::world::{Coordinates, DesktopId, MonitorId, World};

#[must_use]
pub fn locate_leaf(world: &World, window: u32) -> Option<Coordinates> {
    desktop_scan(world, None, |monitor, desktop| {
        let root = world.desktop(desktop).tree.root?;
        world
            .tree
            .leaves(root)
            .find(|node| world.tree.node(*node).external_id == window)
            .map(|node| Coordinates::node(monitor, desktop, node))
    })
}

#[must_use]
pub fn locate_desktop(world: &World, name: &str) -> Option<Coordinates> {
    desktop_scan(world, None, |monitor, desktop| {
        (world.desktop(desktop).name == name).then(|| Coordinates::desktop(monitor, desktop))
    })
}

#[must_use]
pub fn locate_monitor(world: &World, name: &str) -> Option<Coordinates> {
    world
        .monitor_order()
        .iter()
        .copied()
        .find(|monitor| world.monitor(*monitor).name == name)
        .map(Coordinates::monitor)
}

#[must_use]
pub fn find_by_id(world: &World, external_id: u32) -> Option<Coordinates> {
    desktop_scan(world, None, |monitor, desktop| {
        world
            .desktop(desktop)
            .tree
            .root
            .and_then(|root| world.tree.find_by_external_id(root, external_id))
            .map(|node| Coordinates::node(monitor, desktop, node))
    })
}

#[must_use]
pub fn desktop_from_id(
    world: &World,
    external_id: u32,
    monitor_filter: Option<MonitorId>,
) -> Option<Coordinates> {
    desktop_scan(world, monitor_filter, |monitor, desktop| {
        (world.desktop(desktop).external_id == external_id)
            .then(|| Coordinates::desktop(monitor, desktop))
    })
}

#[must_use]
pub fn desktop_from_index(
    world: &World,
    index: u16,
    monitor_filter: Option<MonitorId>,
) -> Option<Coordinates> {
    let mut index = index;
    desktop_scan(world, monitor_filter, |monitor, desktop| {
        let found = index == 1;
        index = index.wrapping_sub(1);
        found.then(|| Coordinates::desktop(monitor, desktop))
    })
}

#[must_use]
pub fn desktop_from_name(
    world: &World,
    name: &str,
    reference: Coordinates,
    selector: &DesktopSelect,
) -> (Option<Coordinates>, usize) {
    let mut hits = 0;
    let mut result = None;
    let _: Option<()> = desktop_scan(world, None, |monitor, desktop| {
        if world.desktop(desktop).name == name {
            hits += 1;
            let loc = Coordinates::desktop(monitor, desktop);
            if result.is_none() && desktop_matches(world, loc, reference, selector) {
                result = Some(loc);
            }
        }
        None
    });
    (result, hits)
}

/// Visits every desktop in monitor then desktop order, stopping at the first hit.
fn desktop_scan<T>(
    world: &World,
    monitor_filter: Option<MonitorId>,
    mut visit: impl FnMut(MonitorId, DesktopId) -> Option<T>,
) -> Option<T> {
    world
        .desktops()
        .filter(|(monitor, _)| monitor_filter.is_none_or(|filter| filter == *monitor))
        .find_map(|(monitor, desktop)| visit(monitor, desktop))
}

#[must_use]
pub fn monitor_from_id(world: &World, external_id: u32) -> Option<Coordinates> {
    world
        .monitor_order()
        .iter()
        .copied()
        .find(|monitor| world.monitor(*monitor).external_id == external_id)
        .map(Coordinates::monitor)
}

#[must_use]
pub fn monitor_from_index(world: &World, index: i32) -> Option<Coordinates> {
    usize::try_from(index.wrapping_sub(1))
        .ok()
        .and_then(|index| world.monitor_order().get(index).copied())
        .map(Coordinates::monitor)
}

#[must_use]
pub fn find_any_monitor(world: &World, selector: &MonitorSelect) -> Option<Coordinates> {
    world
        .monitor_order()
        .iter()
        .copied()
        .map(Coordinates::monitor)
        .find(|loc| monitor_matches(world, *loc, selector))
}

#[must_use]
pub fn find_closest_monitor(
    world: &World,
    reference: Coordinates,
    direction: CycleDirection,
    selector: &MonitorSelect,
) -> Option<Coordinates> {
    let monitor = reference.monitor?;
    let order = world.monitor_order();
    let start = order.iter().position(|candidate| *candidate == monitor)?;
    (1..order.len()).find_map(|offset| {
        let loc = Coordinates::monitor(order[cycle_index(start, offset, order.len(), direction)]);
        monitor_matches(world, loc, selector).then_some(loc)
    })
}

/// Steps `offset` places away from `start` in a ring of `len` elements.
const fn cycle_index(start: usize, offset: usize, len: usize, direction: CycleDirection) -> usize {
    match direction {
        CycleDirection::Next => (start + offset) % len,
        CycleDirection::Prev => (start + len - offset) % len,
    }
}

#[must_use]
pub fn find_nearest_monitor(
    world: &World,
    reference: Coordinates,
    direction: Direction,
    selector: &MonitorSelect,
) -> Option<Coordinates> {
    let reference = reference.monitor?;
    let rectangle = world.monitor(reference).rectangle;
    world
        .monitor_order()
        .iter()
        .copied()
        .filter(|candidate| *candidate != reference)
        .filter_map(|candidate| {
            let loc = Coordinates::monitor(candidate);
            let candidate_rectangle = world.monitor(candidate).rectangle;
            (monitor_matches(world, loc, selector)
                && on_dir_side(rectangle, candidate_rectangle, direction, Tightness::High))
            .then(|| {
                (
                    boundary_distance(rectangle, candidate_rectangle, direction),
                    loc,
                )
            })
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, loc)| loc)
}

fn all_desktops(world: &World) -> Vec<Coordinates> {
    world
        .desktops()
        .map(|(monitor, desktop)| Coordinates::desktop(monitor, desktop))
        .collect()
}

#[must_use]
pub fn find_any_desktop(
    world: &World,
    reference: Coordinates,
    selector: &DesktopSelect,
) -> Option<Coordinates> {
    all_desktops(world)
        .into_iter()
        .find(|loc| desktop_matches(world, *loc, reference, selector))
}

#[must_use]
pub fn find_closest_desktop(
    world: &World,
    reference: Coordinates,
    direction: CycleDirection,
    selector: &DesktopSelect,
) -> Option<Coordinates> {
    let desktop = reference.desktop?;
    let desktops = all_desktops(world);
    let start = desktops
        .iter()
        .position(|loc| loc.desktop == Some(desktop))?;
    (1..desktops.len()).find_map(|offset| {
        let index = cycle_index(start, offset, desktops.len(), direction);
        desktop_matches(world, desktops[index], reference, selector).then_some(desktops[index])
    })
}

fn append_nodes(
    world: &World,
    monitor: MonitorId,
    desktop: DesktopId,
    nodes: &mut Vec<Coordinates>,
) {
    let Some(root) = world.desktop(desktop).tree.root else {
        return;
    };
    // In-order over `root`'s subtree, which is the order upstream cycles nodes in.
    nodes.extend(
        std::iter::successors(Some(world.tree.first_extreme(root)), |current| {
            world.tree.next_node(*current)
        })
        .map(|node| Coordinates::node(monitor, desktop, node)),
    );
}

fn all_cycle_nodes(world: &World) -> Vec<Coordinates> {
    let mut nodes = Vec::new();
    let _: Option<()> = desktop_scan(world, None, |monitor, desktop| {
        append_nodes(world, monitor, desktop, &mut nodes);
        None
    });
    nodes
}

#[must_use]
pub fn find_closest_node(
    world: &World,
    reference: Coordinates,
    direction: CycleDirection,
    selector: &NodeSelect,
) -> Option<Coordinates> {
    let nodes = all_cycle_nodes(world);
    if nodes.is_empty() {
        return None;
    }
    let start = reference
        .node
        .and_then(|node| nodes.iter().position(|loc| loc.node == Some(node)));
    let Some(start) = start else {
        let desktops = all_desktops(world);
        let desktop = reference.desktop?;
        let start = desktops
            .iter()
            .position(|loc| loc.desktop == Some(desktop))?;
        for offset in 1..desktops.len() {
            let index = cycle_index(start, offset, desktops.len(), direction);
            let mut candidates = Vec::new();
            append_nodes(
                world,
                desktops[index].monitor.expect("desktop monitor"),
                desktops[index].desktop.expect("desktop coordinates"),
                &mut candidates,
            );
            if direction == CycleDirection::Prev {
                candidates.reverse();
            }
            if let Some(loc) = candidates
                .into_iter()
                .find(|loc| node_matches(world, *loc, reference, selector))
            {
                return Some(loc);
            }
        }
        return None;
    };
    let count = nodes.len().saturating_sub(1);
    (0..count).find_map(|offset| {
        let index = cycle_index(start, offset + 1, nodes.len(), direction);
        node_matches(world, nodes[index], reference, selector).then_some(nodes[index])
    })
}

#[must_use]
pub fn find_any_node(
    world: &World,
    reference: Coordinates,
    selector: &NodeSelect,
) -> Option<Coordinates> {
    desktop_scan(world, None, |monitor, desktop| {
        let root = world.desktop(desktop).tree.root?;
        world
            .tree
            .preorder(root)
            .map(|node| Coordinates::node(monitor, desktop, node))
            .find(|loc| node_matches(world, *loc, reference, selector))
    })
}

fn node_rectangle(world: &World, loc: Coordinates) -> Rectangle {
    let node = world.tree.node(loc.node.expect("node coordinates"));
    node.client.as_ref().map_or(node.rectangle, |client| {
        if client.state == ClientState::Floating {
            client.floating_rectangle
        } else {
            client.tiled_rectangle
        }
    })
}

#[must_use]
pub fn find_nearest_node(
    world: &World,
    reference: Coordinates,
    direction: Direction,
    tightness: Tightness,
    rank: impl Fn(NodeId) -> u32,
    selector: &NodeSelect,
) -> Option<Coordinates> {
    let reference_node = reference.node?;
    let rectangle = node_rectangle(world, reference);
    let mut best: Option<(u64, u32, Coordinates)> = None;
    for monitor in world.monitor_order().iter().copied() {
        let Some(desktop) = world.monitor(monitor).active_desktop else {
            continue;
        };
        let Some(root) = world.desktop(desktop).tree.root else {
            continue;
        };
        for candidate in world.tree.leaves(root) {
            let value = world.tree.node(candidate);
            let loc = Coordinates::node(monitor, desktop, candidate);
            if candidate != reference_node
                && value.client.is_some()
                && !value.hidden
                && !world.tree.is_descendant(candidate, reference_node)
                && node_matches(world, loc, reference, selector)
                && on_dir_side(rectangle, node_rectangle(world, loc), direction, tightness)
            {
                let key = (
                    boundary_distance(rectangle, node_rectangle(world, loc), direction),
                    rank(candidate),
                );
                if best.is_none_or(|(distance, history, _)| key < (distance, history)) {
                    best = Some((key.0, key.1, loc));
                }
            }
        }
    }
    best.map(|(_, _, loc)| loc)
}

#[must_use]
pub fn find_first_ancestor(
    world: &World,
    reference: Coordinates,
    selector: &NodeSelect,
) -> Option<Coordinates> {
    let mut node = reference.node.and_then(|node| world.tree.node(node).parent);
    while let Some(candidate) = node {
        let loc = Coordinates {
            node: Some(candidate),
            ..reference
        };
        if node_matches(world, loc, reference, selector) {
            return Some(loc);
        }
        node = world.tree.node(candidate).parent;
    }
    None
}

#[must_use]
pub fn find_node_by_area(
    world: &World,
    reference: Coordinates,
    biggest: bool,
    selector: &NodeSelect,
) -> Option<Coordinates> {
    let mut best: Option<(u64, Coordinates)> = None;
    let _: Option<()> = desktop_scan(world, None, |monitor, desktop| {
        let root = world.desktop(desktop).tree.root?;
        for candidate in world.tree.leaves(root) {
            let loc = Coordinates::node(monitor, desktop, candidate);
            if !world.tree.node(candidate).vacant && node_matches(world, loc, reference, selector) {
                let area = area(node_rectangle(world, loc));
                if best.is_none_or(|(best_area, _)| {
                    if biggest {
                        area > best_area
                    } else {
                        area < best_area
                    }
                }) {
                    best = Some((area, loc));
                }
            }
        }
        None
    });
    best.map(|(_, loc)| loc)
}

#[allow(clippy::collapsible_if)]
#[must_use]
pub fn node_matches(
    world: &World,
    loc: Coordinates,
    reference: Coordinates,
    selector: &NodeSelect,
) -> bool {
    let (Some(node), Some(desktop), Some(monitor)) = (loc.node, loc.desktop, loc.monitor) else {
        return false;
    };
    let value = world.tree.node(node);
    let globally_focused = world
        .focused_monitor
        .and_then(|monitor| world.monitor(monitor).active_desktop)
        .and_then(|desktop| world.desktop(desktop).tree.focus);
    if !options_match([
        (selector.automatic, value.presel.is_none()),
        (selector.focused, globally_focused == Some(node)),
        (selector.local, reference.desktop == Some(desktop)),
        (selector.leaf, world.tree.is_leaf(node)),
        (selector.window, value.client.is_some()),
        (selector.hidden, value.hidden),
        (selector.sticky, value.sticky),
        (selector.private, value.private),
        (selector.locked, value.locked),
        (selector.marked, value.marked),
        (
            selector.horizontal,
            value.split_type == SplitType::Horizontal,
        ),
        (selector.vertical, value.split_type == SplitType::Vertical),
    ]) {
        return false;
    }
    let active = world.desktop(desktop).tree.focus == Some(node)
        && world.monitor(monitor).active_desktop == Some(desktop);
    if !option_matches(selector.active, active) {
        return false;
    }
    let descendant = reference
        .node
        .is_some_and(|ancestor| world.tree.is_descendant(node, ancestor));
    let ancestor = reference
        .node
        .is_some_and(|descendant| world.tree.is_descendant(descendant, node));
    if !options_match([
        (selector.descendant_of, descendant),
        (selector.ancestor_of, ancestor),
    ]) {
        return false;
    }
    let Some(client) = value.client.as_ref() else {
        return !has_true_client_selector(selector);
    };
    if let Some(reference_client) = reference
        .node
        .and_then(|node| world.tree.node(node).client.as_ref())
    {
        if !option_matches(
            selector.same_class,
            client.class_name == reference_client.class_name,
        ) {
            return false;
        }
    }
    options_match([
        (selector.tiled, client.state == ClientState::Tiled),
        (
            selector.pseudo_tiled,
            client.state == ClientState::PseudoTiled,
        ),
        (selector.floating, client.state == ClientState::Floating),
        (selector.fullscreen, client.state == ClientState::Fullscreen),
        (selector.below, client.layer == StackLayer::Below),
        (selector.normal, client.layer == StackLayer::Normal),
        (selector.above, client.layer == StackLayer::Above),
        (selector.urgent, client.urgent),
    ])
}

fn has_true_client_selector(selector: &NodeSelect) -> bool {
    [
        selector.same_class,
        selector.tiled,
        selector.pseudo_tiled,
        selector.floating,
        selector.fullscreen,
        selector.below,
        selector.normal,
        selector.above,
        selector.urgent,
    ]
    .contains(&OptionBool::True)
}

#[must_use]
pub fn desktop_matches(
    world: &World,
    loc: Coordinates,
    reference: Coordinates,
    selector: &DesktopSelect,
) -> bool {
    let (Some(desktop), Some(monitor)) = (loc.desktop, loc.monitor) else {
        return false;
    };
    let value = world.desktop(desktop);
    let globally_focused = world
        .focused_monitor
        .and_then(|monitor| world.monitor(monitor).active_desktop);
    options_match([
        (selector.occupied, value.tree.root.is_some()),
        (selector.focused, globally_focused == Some(desktop)),
        (
            selector.active,
            world.monitor(monitor).active_desktop == Some(desktop),
        ),
        (selector.urgent, world.desktop_is_urgent(desktop)),
        (selector.local, reference.monitor == Some(monitor)),
        (selector.tiled, value.layout == Layout::Tiled),
        (selector.monocle, value.layout == Layout::Monocle),
        (selector.user_tiled, value.user_layout == Layout::Tiled),
        (selector.user_monocle, value.user_layout == Layout::Monocle),
    ])
}

#[must_use]
pub fn monitor_matches(world: &World, loc: Coordinates, selector: &MonitorSelect) -> bool {
    let Some(monitor) = loc.monitor else {
        return false;
    };
    let value = world.monitor(monitor);
    let occupied = value
        .active_desktop
        .is_some_and(|desktop| world.desktop(desktop).tree.root.is_some());
    options_match([
        (selector.occupied, occupied),
        (selector.focused, world.focused_monitor == Some(monitor)),
    ])
}

fn options_match<const N: usize>(options: [(OptionBool, bool); N]) -> bool {
    options
        .into_iter()
        .all(|(expected, actual)| option_matches(expected, actual))
}

const fn option_matches(option: OptionBool, actual: bool) -> bool {
    match option {
        OptionBool::None => true,
        OptionBool::True => actual,
        OptionBool::False => !actual,
    }
}
