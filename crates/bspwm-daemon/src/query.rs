#![allow(
    clippy::missing_panics_doc,
    clippy::struct_excessive_bools,
    clippy::struct_field_names
)]

use std::borrow::Cow;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::state::DaemonState;
use crate::tree::{Client, NodeId, Presel};
use crate::types::{Constraints, DesktopSelect, MonitorSelect, NodeSelect, Padding, Rectangle};
use crate::world::{Desktop, DesktopId, Monitor, MonitorId, World};

pub use crate::world::Coordinates;
pub use bspwm_model::select::{
    desktop_from_id, desktop_from_index, desktop_from_name, desktop_matches, find_any_desktop,
    find_any_monitor, find_any_node, find_by_id, find_closest_desktop, find_closest_monitor,
    find_closest_node, find_first_ancestor, find_nearest_monitor, find_nearest_node,
    find_node_by_area, locate_desktop, locate_leaf, locate_monitor, monitor_from_id,
    monitor_from_index, monitor_matches, node_matches,
};

#[must_use]
pub fn query_state(state: &DaemonState) -> String {
    let world = &state.world;
    let dto = StateDto {
        focused_monitor_id: world
            .focused_monitor
            .map(|monitor| world.monitor(monitor).external_id),
        primary_monitor_id: world
            .primary_monitor
            .map(|monitor| world.monitor(monitor).external_id),
        clients_count: state.clients_count,
        monitors: world
            .monitor_order()
            .iter()
            .map(|monitor| monitor_dto(world, *monitor))
            .collect(),
        focus_history: state
            .history
            .entries()
            .iter()
            .map(|entry| history_coordinates_dto(world, entry.location))
            .collect(),
        stacking_list: state.stacking_order.windows(),
        event_subscribers: Vec::new(),
    };
    serde_json::to_string(&dto).expect("daemon state should serialize as JSON")
}

#[must_use]
pub fn query_monitor(world: &World, monitor: MonitorId) -> String {
    serde_json::to_string(&monitor_dto(world, monitor))
        .expect("monitor state should serialize as JSON")
}

#[must_use]
pub fn query_desktop(world: &World, desktop: DesktopId) -> String {
    serde_json::to_string(&desktop_dto(world, desktop))
        .expect("desktop state should serialize as JSON")
}

pub fn query_node(world: &World, node: Option<NodeId>, rsp: &mut String) {
    let dto = node.map(|node| node_dto(world, node));
    rsp.push_str(&serde_json::to_string(&dto).expect("query value should serialize as JSON"));
}

// The DTOs below are the `--restart` wire format. They derive both directions so
// that `query_state` and `crate::restore` can never drift apart.

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StateDto<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_monitor_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_monitor_id: Option<u32>,
    pub clients_count: u32,
    pub monitors: Vec<MonitorDto<'a>>,
    #[serde(default)]
    pub focus_history: Vec<CoordinatesDto>,
    #[serde(default)]
    pub stacking_list: Vec<u32>,
    /// Appended to the dump by the daemon; `query_state` never emits it.
    #[serde(default, skip_serializing)]
    pub event_subscribers: Vec<SubscriberDto>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscriberDto {
    pub file_descriptor: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fifo_path: Option<String>,
    pub field: u32,
    pub count: i32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MonitorDto<'a> {
    pub name: Cow<'a, str>,
    pub id: u32,
    pub randr_id: u32,
    pub wired: bool,
    pub sticky_count: u32,
    pub window_gap: i32,
    pub border_width: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_desktop_id: Option<u32>,
    pub padding: Padding,
    pub rectangle: Rectangle,
    pub desktops: Vec<DesktopDto<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopDto<'a> {
    pub name: Cow<'a, str>,
    pub id: u32,
    pub layout: Cow<'a, str>,
    pub user_layout: Cow<'a, str>,
    pub window_gap: i32,
    pub border_width: u32,
    pub focused_node_id: u32,
    pub padding: Padding,
    #[serde(default)]
    pub root: Option<NodeDto<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeDto<'a> {
    pub id: u32,
    pub split_type: Cow<'a, str>,
    pub split_ratio: f64,
    pub vacant: bool,
    pub hidden: bool,
    pub sticky: bool,
    pub private: bool,
    pub locked: bool,
    pub marked: bool,
    #[serde(default)]
    pub presel: Option<PreselDto<'a>>,
    pub rectangle: Rectangle,
    pub constraints: Constraints,
    #[serde(default)]
    pub first_child: Option<Box<NodeDto<'a>>>,
    #[serde(default)]
    pub second_child: Option<Box<NodeDto<'a>>>,
    #[serde(default)]
    pub client: Option<ClientDto<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreselDto<'a> {
    pub split_dir: Cow<'a, str>,
    pub split_ratio: f64,
}

impl From<Presel> for PreselDto<'_> {
    fn from(presel: Presel) -> Self {
        Self {
            split_dir: Cow::Borrowed(presel.split_dir.protocol_name()),
            split_ratio: presel.split_ratio,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientDto<'a> {
    pub class_name: Cow<'a, str>,
    pub instance_name: Cow<'a, str>,
    pub border_width: u32,
    pub state: Cow<'a, str>,
    pub last_state: Cow<'a, str>,
    pub layer: Cow<'a, str>,
    pub last_layer: Cow<'a, str>,
    pub urgent: bool,
    pub shown: bool,
    pub tiled_rectangle: Rectangle,
    pub floating_rectangle: Rectangle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transient_for: Option<u32>,
}

impl<'a> From<&'a Client> for ClientDto<'a> {
    fn from(client: &'a Client) -> Self {
        Self {
            class_name: Cow::Borrowed(&client.class_name),
            instance_name: Cow::Borrowed(&client.instance_name),
            border_width: client.border_width,
            state: Cow::Borrowed(client.state.protocol_name()),
            last_state: Cow::Borrowed(client.last_state.protocol_name()),
            layer: Cow::Borrowed(client.layer.protocol_name()),
            last_layer: Cow::Borrowed(client.last_layer.protocol_name()),
            urgent: client.urgent,
            shown: client.shown,
            tiled_rectangle: client.tiled_rectangle,
            floating_rectangle: client.floating_rectangle,
            transient_for: client.transient_for,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoordinatesDto {
    pub monitor_id: u32,
    pub desktop_id: u32,
    pub node_id: u32,
}

fn history_coordinates_dto(
    world: &World,
    location: crate::history::Coordinates<MonitorId, DesktopId>,
) -> CoordinatesDto {
    CoordinatesDto {
        monitor_id: world.monitor(location.monitor).external_id,
        desktop_id: world.desktop(location.desktop).external_id,
        node_id: location
            .node
            .map_or(0, |node| world.tree.node(node).external_id),
    }
}

fn monitor_dto(world: &World, monitor: MonitorId) -> MonitorDto<'_> {
    let monitor = world.monitor(monitor);
    MonitorDto {
        name: Cow::Borrowed(&monitor.name),
        id: monitor.external_id,
        randr_id: monitor.randr_id,
        wired: monitor.wired,
        sticky_count: monitor.sticky_count,
        window_gap: monitor.window_gap,
        border_width: monitor.border_width,
        focused_desktop_id: monitor
            .active_desktop
            .map(|desktop| world.desktop(desktop).external_id),
        padding: monitor.padding,
        rectangle: monitor.rectangle,
        desktops: monitor
            .desktops
            .iter()
            .map(|desktop| desktop_dto(world, *desktop))
            .collect(),
    }
}

fn desktop_dto(world: &World, desktop: DesktopId) -> DesktopDto<'_> {
    let desktop = world.desktop(desktop);
    DesktopDto {
        name: Cow::Borrowed(&desktop.name),
        id: desktop.external_id,
        layout: Cow::Borrowed(desktop.layout.protocol_name()),
        user_layout: Cow::Borrowed(desktop.user_layout.protocol_name()),
        window_gap: desktop.window_gap,
        border_width: desktop.border_width,
        focused_node_id: desktop
            .tree
            .focus
            .map_or(0, |node| world.tree.node(node).external_id),
        padding: desktop.padding,
        root: desktop.tree.root.map(|node| node_dto(world, node)),
    }
}

fn node_dto(world: &World, node: NodeId) -> NodeDto<'_> {
    let node = world.tree.node(node);
    NodeDto {
        id: node.external_id,
        split_type: Cow::Borrowed(node.split_type.protocol_name()),
        split_ratio: node.split_ratio,
        vacant: node.vacant,
        hidden: node.hidden,
        sticky: node.sticky,
        private: node.private,
        locked: node.locked,
        marked: node.marked,
        presel: node.presel.map(PreselDto::from),
        rectangle: node.rectangle,
        constraints: node.constraints,
        first_child: node
            .first_child
            .map(|child| Box::new(node_dto(world, child))),
        second_child: node
            .second_child
            .map(|child| Box::new(node_dto(world, child))),
        client: node.client.as_ref().map(ClientDto::from),
    }
}

#[must_use]
pub fn print_rectangle(rectangle: Option<&Rectangle>) -> Option<String> {
    rectangle.map(ToString::to_string)
}

/// The descriptor-free constraints a `query -N` may carry.
#[derive(Clone, Copy, Default)]
pub struct NodeIdFilters<'a> {
    pub monitor: Option<&'a MonitorSelect>,
    pub desktop: Option<&'a DesktopSelect>,
    pub node: Option<&'a NodeSelect>,
}

pub fn query_node_ids(
    world: &World,
    desktop_reference: Coordinates,
    reference: Coordinates,
    target: Coordinates,
    filters: NodeIdFilters<'_>,
    rsp: &mut String,
) -> usize {
    let mut count = 0;
    for (monitor, desktop) in world.desktops() {
        if target.monitor.is_some_and(|target| target != monitor)
            || filters.monitor.is_some_and(|selector| {
                !monitor_matches(world, Coordinates::monitor(monitor), selector)
            })
        {
            continue;
        }
        let loc = Coordinates::desktop(monitor, desktop);
        if target.desktop.is_some_and(|target| target != desktop)
            || filters
                .desktop
                .is_some_and(|selector| !desktop_matches(world, loc, desktop_reference, selector))
        {
            continue;
        }
        count += query_node_ids_in(
            world,
            world.desktop(desktop).tree.root,
            loc,
            reference,
            target,
            filters.node,
            rsp,
        );
    }
    count
}

fn query_node_ids_in(
    world: &World,
    root: Option<NodeId>,
    position: Coordinates,
    reference: Coordinates,
    target: Coordinates,
    selector: Option<&NodeSelect>,
    rsp: &mut String,
) -> usize {
    let Some(root) = root else {
        return 0;
    };
    let mut count = 0;
    for node in world.tree.preorder(root) {
        let loc = Coordinates {
            node: Some(node),
            ..position
        };
        if target.node.is_none_or(|target| target == node)
            && selector.is_none_or(|selector| node_matches(world, loc, reference, selector))
        {
            let _ = writeln!(rsp, "0x{:08X}", world.tree.node(node).external_id);
            count += 1;
        }
    }
    count
}

pub fn query_desktop_ids(
    world: &World,
    reference: Coordinates,
    target: Coordinates,
    monitor_selector: Option<&MonitorSelect>,
    selector: Option<&DesktopSelect>,
    names: bool,
    rsp: &mut String,
) -> usize {
    let mut count = 0;
    for (monitor, desktop) in world.desktops() {
        if target.monitor.is_some_and(|target| target != monitor)
            || monitor_selector.is_some_and(|selector| {
                !monitor_matches(world, Coordinates::monitor(monitor), selector)
            })
        {
            continue;
        }
        let loc = Coordinates::desktop(monitor, desktop);
        if target.desktop.is_some_and(|target| target != desktop)
            || selector.is_some_and(|selector| !desktop_matches(world, loc, reference, selector))
        {
            continue;
        }
        if names {
            fprint_desktop_name(world.desktop(desktop), rsp);
        } else {
            fprint_desktop_id(world.desktop(desktop), rsp);
        }
        count += 1;
    }
    count
}

pub fn query_monitor_ids(
    world: &World,
    target: Coordinates,
    selector: Option<&MonitorSelect>,
    names: bool,
    rsp: &mut String,
) -> usize {
    let mut count = 0;
    for monitor in world.monitor_order().iter().copied() {
        let loc = Coordinates::monitor(monitor);
        if target.monitor.is_some_and(|target| target != monitor)
            || selector.is_some_and(|selector| !monitor_matches(world, loc, selector))
        {
            continue;
        }
        if names {
            fprint_monitor_name(world.monitor(monitor), rsp);
        } else {
            fprint_monitor_id(world.monitor(monitor), rsp);
        }
        count += 1;
    }
    count
}

pub fn fprint_monitor_id(monitor: &Monitor, rsp: &mut String) {
    let _ = writeln!(rsp, "0x{:08X}", monitor.external_id);
}

pub fn fprint_monitor_name(monitor: &Monitor, rsp: &mut String) {
    let _ = writeln!(rsp, "{}", monitor.name);
}

pub fn fprint_desktop_id(desktop: &Desktop, rsp: &mut String) {
    let _ = writeln!(rsp, "0x{:08X}", desktop.external_id);
}

pub fn fprint_desktop_name(desktop: &Desktop, rsp: &mut String) {
    let _ = writeln!(rsp, "{}", desktop.name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::types::{
        ButtonIndex, ClientState, Direction, OptionBool, PointerAction, SplitType, StackLayer,
        StateTransitions,
    };
    use serde_json::{Value, json};

    fn world_with_tree() -> (World, MonitorId, DesktopId, NodeId, NodeId, NodeId) {
        let settings = Settings::default();
        let mut world = World::default();
        let monitor = world.create_monitor(
            0xA,
            Some("Display-1"),
            Rectangle::new(-10, 20, 800, 600),
            &settings,
        );
        let desktop = world.create_desktop(0xB, Some("I"), &settings);
        assert!(world.add_desktop(monitor, desktop));
        let root = world.tree.add_node(0x100, 0.625);
        let first = world.tree.add_node(0x101, 0.5);
        let second = world.tree.add_node(0x102, 0.5);
        world.tree.set_children(root, first, second);
        world.tree.node_mut(root).split_type = SplitType::Horizontal;
        world.tree.update_constraints(root);
        world.tree.node_mut(root).rectangle = Rectangle::new(1, 2, 300, 200);
        world.tree.node_mut(first).hidden = true;
        let mut client = Client::from_settings(&settings);
        client.class_name = "Term".into();
        client.instance_name = "term".into();
        client.state = ClientState::Floating;
        client.last_state = ClientState::Tiled;
        client.layer = StackLayer::Above;
        client.urgent = true;
        client.shown = true;
        client.tiled_rectangle = Rectangle::new(3, 4, 50, 60);
        client.floating_rectangle = Rectangle::new(5, 6, 70, 80);
        world.tree.node_mut(first).client = Some(client);
        world.desktop_mut(desktop).tree.root = Some(root);
        world.desktop_mut(desktop).tree.focus = Some(first);
        world.primary_monitor = Some(monitor);
        (world, monitor, desktop, root, first, second)
    }

    #[test]
    fn formatting_helpers_match_upstream_text() {
        assert_eq!(StateTransitions::NONE.to_string(), "none");
        assert_eq!(StateTransitions::ALL.to_string(), "enter,exit");
        assert_eq!(ButtonIndex::Button2.to_string(), "button2");
        assert_eq!(PointerAction::ResizeCorner.to_string(), "resize_corner");
        assert_eq!(
            print_rectangle(Some(&Rectangle::new(-2, 3, 40, 50))),
            Some("40x50+-2+3".into())
        );
        assert_eq!(
            print_rectangle(Some(&Rectangle::new(32_768, -32_769, 65_536, 70_000))),
            Some("65536x70000+32768+-32769".into())
        );
        assert_eq!(print_rectangle(None), None);
    }

    #[test]
    fn restart_rectangle_preserves_wide_internal_geometry() {
        let rectangle = Rectangle::new(32_768, -32_769, 65_536, 70_000);
        assert_eq!(
            serde_json::to_string(&rectangle).unwrap(),
            "{\"x\":32768,\"y\":-32769,\"width\":65536,\"height\":70000}"
        );
        let decoded: Rectangle =
            serde_json::from_str("{\"x\":32768,\"y\":-32769,\"width\":65536,\"height\":70000}")
                .unwrap();
        assert_eq!(decoded, rectangle);
    }

    #[test]
    fn other_value_records_use_the_restart_wire_shape_directly() {
        let padding = Padding {
            top: 1,
            right: 2,
            bottom: 3,
            left: 4,
        };
        let constraints = Constraints {
            min_width: 5,
            min_height: 6,
        };
        assert_eq!(
            serde_json::to_string(&padding).unwrap(),
            "{\"top\":1,\"right\":2,\"bottom\":3,\"left\":4}"
        );
        assert_eq!(
            serde_json::to_string(&constraints).unwrap(),
            "{\"min_width\":5,\"min_height\":6}"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn state_json_serializes_every_represented_upstream_field_compactly() {
        let (world, monitor_id, desktop_id, _, first, _) = world_with_tree();
        let mut daemon = DaemonState {
            world,
            clients_count: 1,
            ..DaemonState::default()
        };
        daemon.history.add(
            crate::history::Coordinates {
                monitor: monitor_id,
                desktop: desktop_id,
                node: Some(first),
            },
            true,
        );
        {
            struct Noop;
            impl bspwm_xstack::StackBackend for Noop {
                type Error = ();
                fn stack_above(&mut self, _: u32, _: u32) -> Result<(), ()> {
                    Ok(())
                }
                fn stack_below(&mut self, _: u32, _: u32) -> Result<(), ()> {
                    Ok(())
                }
            }
            let xid = daemon.world.tree.node(first).external_id;
            let level =
                crate::stack::stack_level(daemon.world.tree.node(first).client.as_ref().unwrap());
            let _ = daemon.stacking_order.insert(&mut Noop, xid, level);
        }
        let state = query_state(&daemon);
        let value: Value = serde_json::from_str(&state).unwrap();
        assert_eq!(value["focusedMonitorId"], 10);
        assert_eq!(value["primaryMonitorId"], 10);
        assert_eq!(value["clientsCount"], 1);
        let client = json!({
            "className": "Term",
            "instanceName": "term",
            "borderWidth": 1,
            "state": "floating",
            "lastState": "tiled",
            "layer": "above",
            "lastLayer": "normal",
            "urgent": true,
            "shown": true,
            "tiledRectangle": { "x": 3, "y": 4, "width": 50, "height": 60 },
            "floatingRectangle": { "x": 5, "y": 6, "width": 70, "height": 80 }
        });
        let first_child = json!({
            "id": 257,
            "splitType": "vertical",
            "splitRatio": 0.5,
            "vacant": false,
            "hidden": true,
            "sticky": false,
            "private": false,
            "locked": false,
            "marked": false,
            "presel": null,
            "rectangle": { "x": 0, "y": 0, "width": 0, "height": 0 },
            "constraints": { "min_width": 32, "min_height": 32 },
            "firstChild": null,
            "secondChild": null,
            "client": client
        });
        let second_child = json!({
            "id": 258,
            "splitType": "vertical",
            "splitRatio": 0.5,
            "vacant": false,
            "hidden": false,
            "sticky": false,
            "private": false,
            "locked": false,
            "marked": false,
            "presel": null,
            "rectangle": { "x": 0, "y": 0, "width": 0, "height": 0 },
            "constraints": { "min_width": 32, "min_height": 32 },
            "firstChild": null,
            "secondChild": null,
            "client": null
        });
        let root = json!({
            "id": 256,
            "splitType": "horizontal",
            "splitRatio": 0.625,
            "vacant": false,
            "hidden": false,
            "sticky": false,
            "private": false,
            "locked": false,
            "marked": false,
            "presel": null,
            "rectangle": { "x": 1, "y": 2, "width": 300, "height": 200 },
            "constraints": { "min_width": 32, "min_height": 64 },
            "firstChild": first_child,
            "secondChild": second_child,
            "client": null
        });
        let desktop = json!({
            "name": "I",
            "id": 11,
            "layout": "tiled",
            "userLayout": "tiled",
            "windowGap": 6,
            "borderWidth": 1,
            "focusedNodeId": 257,
            "padding": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
            "root": root
        });
        let monitor = json!({
            "name": "Display-1",
            "id": 10,
            "randrId": 0,
            "wired": true,
            "stickyCount": 0,
            "windowGap": 6,
            "borderWidth": 1,
            "focusedDesktopId": 11,
            "padding": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
            "rectangle": { "x": -10, "y": 20, "width": 800, "height": 600 },
            "desktops": [desktop]
        });
        assert_eq!(value["monitors"][0], monitor);
        assert!(!state.chars().any(char::is_whitespace));
        assert_eq!(value["monitors"][0]["randrId"], 0);
        assert!(value["monitors"][0]["desktops"][0]["root"]["presel"].is_null());
        assert_eq!(
            value["focusHistory"],
            json!([{"monitorId": 10, "desktopId": 11, "nodeId": 257}])
        );
        assert_eq!(value["stackingList"], json!([257]));
    }

    #[test]
    fn empty_optional_runtime_slots_are_omitted_not_fabricated() {
        let settings = Settings::default();
        let mut world = World::default();
        let monitor = world.create_monitor(1, Some("empty"), Rectangle::default(), &settings);
        world.focused_monitor = None;
        let daemon = DaemonState {
            world: world.clone(),
            ..DaemonState::default()
        };
        let state: Value = serde_json::from_str(&query_state(&daemon)).unwrap();
        let monitor: Value = serde_json::from_str(&query_monitor(&world, monitor)).unwrap();
        assert!(state.get("focusedMonitorId").is_none());
        assert!(state.get("eventSubscribers").is_none());
        assert!(monitor.get("focusedDesktopId").is_none());
    }

    #[test]
    fn global_lookup_obeys_monitor_and_tree_order() {
        let (mut world, monitor, desktop, root, first, second) = world_with_tree();
        let settings = Settings::default();
        let other_monitor = world.create_monitor(
            0xC,
            Some("Display-2"),
            Rectangle::new(900, 0, 100, 100),
            &settings,
        );
        let other_desktop = world.create_desktop(0xD, Some("I"), &settings);
        assert!(world.add_desktop(other_monitor, other_desktop));
        assert_eq!(
            locate_monitor(&world, "Display-1").unwrap().monitor,
            Some(monitor)
        );
        assert_eq!(locate_desktop(&world, "I").unwrap().desktop, Some(desktop));
        assert_eq!(
            monitor_from_id(&world, 0xC).unwrap().monitor,
            Some(other_monitor)
        );
        assert_eq!(
            monitor_from_index(&world, 2).unwrap().monitor,
            Some(other_monitor)
        );
        assert_eq!(
            desktop_from_id(&world, 0xD, None).unwrap().desktop,
            Some(other_desktop)
        );
        assert_eq!(
            desktop_from_index(&world, 2, None).unwrap().desktop,
            Some(other_desktop)
        );
        assert_eq!(find_by_id(&world, 0x100).unwrap().node, Some(root));
        assert_eq!(locate_leaf(&world, 0x101).unwrap().node, Some(first));
        assert_eq!(locate_leaf(&world, 0x102).unwrap().node, Some(second));
        assert_eq!(locate_leaf(&world, 0x999), None);
    }

    #[test]
    fn selectors_match_available_monitor_desktop_and_node_state() {
        let (world, monitor, desktop, root, first, second) = world_with_tree();
        let reference = Coordinates::node(monitor, desktop, first);
        let first_loc = reference;
        let second_loc = Coordinates {
            node: Some(second),
            ..reference
        };
        let monitor_select = MonitorSelect {
            focused: OptionBool::True,
            occupied: OptionBool::True,
        };
        assert!(monitor_matches(
            &world,
            Coordinates::monitor(monitor),
            &monitor_select
        ));
        let desktop_select = DesktopSelect {
            focused: OptionBool::True,
            active: OptionBool::True,
            urgent: OptionBool::True,
            ..DesktopSelect::default()
        };
        assert!(desktop_matches(
            &world,
            first_loc,
            reference,
            &desktop_select
        ));
        let mut node_select = NodeSelect {
            focused: OptionBool::True,
            active: OptionBool::True,
            window: OptionBool::True,
            floating: OptionBool::True,
            above: OptionBool::True,
            hidden: OptionBool::True,
            urgent: OptionBool::True,
            descendant_of: OptionBool::True,
            ..NodeSelect::default()
        };
        assert!(node_matches(&world, first_loc, reference, &node_select));
        node_select.active = OptionBool::False;
        assert!(!node_matches(&world, second_loc, reference, &node_select));
        node_select = NodeSelect {
            ancestor_of: OptionBool::True,
            ..NodeSelect::default()
        };
        assert!(node_matches(
            &world,
            Coordinates {
                node: Some(root),
                ..reference
            },
            reference,
            &node_select
        ));
        node_select.automatic = OptionBool::True;
        assert!(node_matches(&world, first_loc, reference, &node_select));
        let mut world = world;
        world.tree.set_presel_direction(first, Direction::West, 0.6);
        assert!(!node_matches(&world, first_loc, reference, &node_select));
        node_select.automatic = OptionBool::False;
        assert!(node_matches(&world, first_loc, reference, &node_select));
        let value: Value = serde_json::from_str(&query_state(&DaemonState {
            world,
            clients_count: 1,
            ..DaemonState::default()
        }))
        .unwrap();
        assert_eq!(
            value["monitors"][0]["desktops"][0]["root"]["firstChild"]["presel"],
            json!({"splitDir": "west", "splitRatio": 0.6})
        );
    }

    #[test]
    fn id_and_name_queries_preserve_upstream_order_and_format() {
        let (world, monitor, desktop, _, first, _) = world_with_tree();
        let reference = Coordinates::node(monitor, desktop, first);
        let mut rsp = String::new();
        assert_eq!(
            query_monitor_ids(&world, Coordinates::default(), None, false, &mut rsp),
            1
        );
        assert_eq!(rsp, "0x0000000A\n");
        rsp.clear();
        assert_eq!(
            query_desktop_ids(
                &world,
                reference,
                Coordinates::default(),
                None,
                None,
                true,
                &mut rsp
            ),
            1
        );
        assert_eq!(rsp, "I\n");
        rsp.clear();
        assert_eq!(
            query_node_ids(
                &world,
                reference,
                reference,
                Coordinates::default(),
                NodeIdFilters::default(),
                &mut rsp
            ),
            3
        );
        assert_eq!(rsp, "0x00000100\n0x00000101\n0x00000102\n");
    }

    #[test]
    fn duplicate_desktop_names_report_hits_before_selector_filtering() {
        let (mut world, monitor, desktop, _, first, _) = world_with_tree();
        let settings = Settings::default();
        let duplicate = world.create_desktop(99, Some("I"), &settings);
        assert!(world.add_desktop(monitor, duplicate));
        let reference = Coordinates::node(monitor, desktop, first);
        let selector = DesktopSelect {
            active: OptionBool::False,
            ..DesktopSelect::default()
        };
        let (result, hits) = desktop_from_name(&world, "I", reference, &selector);
        assert_eq!(hits, 2);
        assert_eq!(result.unwrap().desktop, Some(duplicate));
    }
}
