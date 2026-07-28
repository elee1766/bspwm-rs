#![allow(clippy::missing_errors_doc)]

//! Pure restoration of the state emitted by [`crate::query::query_state`].

use std::collections::HashSet;

use crate::history::{Coordinates, History};
use crate::parse::{
    parse_client_state, parse_direction, parse_layout, parse_split_type, parse_stack_layer,
};
use crate::query::{
    ClientDto, CoordinatesDto, DesktopDto, MonitorDto, NodeDto, PreselDto, StateDto, SubscriberDto,
};
use crate::settings::Settings;
use crate::stack::StackingOrder;
use crate::tree::{Client, NodeId, Presel};
use crate::world::{DesktopId, MonitorId, World};

/// A structural or value error in a saved state.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct RestoreError {
    pub path: String,
    pub message: String,
}

impl RestoreError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoredSubscriber {
    pub file_descriptor: i32,
    pub fifo_path: Option<String>,
    pub field: u32,
    pub count: i32,
}

/// Pure daemon state reconstructed from a dump.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoredState {
    pub world: World,
    pub history: History<MonitorId, DesktopId>,
    pub stacking_order: StackingOrder,
    pub clients_count: u32,
    pub event_subscribers: Vec<RestoredSubscriber>,
}

impl RestoredState {
    /// Replaces every server-allocated identity while retaining client windows.
    /// Arena handles are stable, so history and stacking references need no rewrite.
    pub fn regenerate_xids(&mut self, mut next: impl FnMut() -> u32) {
        for monitor in self.world.monitor_order().to_vec() {
            self.world.monitor_mut(monitor).external_id = next();
            for desktop in self.world.monitor(monitor).desktops.clone() {
                self.world.desktop_mut(desktop).external_id = next();
                let Some(root) = self.world.desktop(desktop).tree.root else {
                    continue;
                };
                let nodes: Vec<_> = self
                    .world
                    .tree
                    .preorder(root)
                    .filter(|node| self.world.tree.node(*node).client.is_none())
                    .collect();
                for node in nodes {
                    self.world.tree.node_mut(node).external_id = next();
                }
            }
        }
    }
}

/// Every server-allocated id already claimed by this dump.
#[derive(Default)]
struct Identities {
    monitors: HashSet<u32>,
    desktops: HashSet<u32>,
    nodes: HashSet<u32>,
}

/// Applies one of the `crate::parse` symbol parsers to `$dto.$key`, turning a
/// miss into a located error. Yields `Result<T, RestoreError>`.
macro_rules! expect_enum {
    ($parse:ident, $dto:ident.$field:ident, $path:expr, $key:literal, $what:literal) => {
        $parse(&$dto.$field).ok_or_else(|| {
            RestoreError::new(
                format!("{}.{}", $path, $key),
                format!("unknown {} '{}'", $what, $dto.$field),
            )
        })
    };
}

/// Restore fresh daemon state from the JSON produced by `query_state`.
///
/// No externally visible state is modified if restoration fails.
pub fn restore_state(json: &str, settings: &Settings) -> Result<RestoredState, RestoreError> {
    let state: StateDto = serde_json::from_str(json)
        .map_err(|error| RestoreError::new("$", format!("invalid JSON: {error}")))?;

    let expected_clients = state.clients_count;
    let event_subscribers = restore_subscribers(state.event_subscribers)?;
    let mut world = World::default();
    let mut ids = Identities::default();

    // create_monitor sorts geometrically and inserts before equal rectangles.
    // Reversing input preserves the serialized order of equal monitors.
    for (index, monitor) in state.monitors.into_iter().enumerate().rev() {
        restore_monitor(
            monitor,
            &mut world,
            settings,
            &format!("$.monitors[{index}]"),
            &mut ids,
        )?;
    }

    world.focused_monitor =
        restore_monitor_reference(&world, state.focused_monitor_id, "$.focusedMonitorId")?;
    world.primary_monitor =
        restore_monitor_reference(&world, state.primary_monitor_id, "$.primaryMonitorId")?;

    let actual_clients = world
        .roots()
        .map(|(_, _, root)| world.tree.clients_count(root))
        .sum::<u32>();
    if actual_clients != expected_clients {
        return Err(RestoreError::new(
            "$.clientsCount",
            format!("declares {expected_clients}, restored {actual_clients}"),
        ));
    }
    world
        .validate()
        .map_err(|message| RestoreError::new("$", message))?;
    let history = restore_history(&world, state.focus_history);
    let stacking_order = restore_stacking(&world, state.stacking_list)?;
    Ok(RestoredState {
        world,
        history,
        stacking_order,
        clients_count: actual_clients,
        event_subscribers,
    })
}

fn restore_subscribers(
    subscribers: Vec<SubscriberDto>,
) -> Result<Vec<RestoredSubscriber>, RestoreError> {
    subscribers
        .into_iter()
        .enumerate()
        .map(|(index, subscriber)| {
            let path = format!("$.eventSubscribers[{index}]");
            if subscriber.file_descriptor < 0 {
                return Err(RestoreError::new(
                    format!("{path}.fileDescriptor"),
                    "file descriptor must be nonnegative",
                ));
            }
            if subscriber.field & !crate::types::SubscriberMask::ALL.bits() != 0 {
                return Err(RestoreError::new(
                    format!("{path}.field"),
                    "subscriber mask contains unknown bits",
                ));
            }
            Ok(RestoredSubscriber {
                file_descriptor: subscriber.file_descriptor,
                fifo_path: subscriber.fifo_path,
                field: subscriber.field,
                count: subscriber.count,
            })
        })
        .collect()
}

fn restore_monitor(
    dto: MonitorDto<'_>,
    world: &mut World,
    settings: &Settings,
    path: &str,
    ids: &mut Identities,
) -> Result<MonitorId, RestoreError> {
    let external_id = claim(&mut ids.monitors, dto.id, path, "monitor")?;
    let monitor = world.create_monitor(external_id, Some(&dto.name), dto.rectangle, settings);
    {
        let value = world.monitor_mut(monitor);
        value.wired = dto.wired;
        value.randr_id = dto.randr_id;
        value.sticky_count = dto.sticky_count;
        value.window_gap = dto.window_gap;
        value.border_width = dto.border_width;
        value.padding = dto.padding;
    }

    let mut restored = Vec::with_capacity(dto.desktops.len());
    for (index, desktop) in dto.desktops.into_iter().enumerate() {
        restored.push(restore_desktop(
            desktop,
            world,
            settings,
            &format!("{path}.desktops[{index}]"),
            ids,
        )?);
    }
    for desktop in &restored {
        let (window_gap, border_width) = {
            let value = world.desktop(*desktop);
            (value.window_gap, value.border_width)
        };
        if !world.add_desktop(monitor, *desktop) {
            return Err(RestoreError::new(path, "desktop is already attached"));
        }
        let value = world.desktop_mut(*desktop);
        value.window_gap = window_gap;
        value.border_width = border_width;
    }
    if restored.is_empty() {
        let desktop = world.create_desktop(world.next_external_id(), None, settings);
        let added = world.add_desktop(monitor, desktop);
        debug_assert!(added);
        restored.push(desktop);
    }
    if let Some(focused_id) = dto.focused_desktop_id {
        let focused = restored
            .iter()
            .copied()
            .find(|desktop| world.desktop(*desktop).external_id == focused_id)
            .ok_or_else(|| {
                RestoreError::new(
                    format!("{path}.focusedDesktopId"),
                    format!("desktop id {focused_id} does not belong to this monitor"),
                )
            })?;
        world.monitor_mut(monitor).active_desktop = Some(focused);
    }
    Ok(monitor)
}

fn restore_desktop(
    dto: DesktopDto<'_>,
    world: &mut World,
    settings: &Settings,
    path: &str,
    ids: &mut Identities,
) -> Result<DesktopId, RestoreError> {
    let external_id = claim(&mut ids.desktops, dto.id, path, "desktop")?;
    let root = dto
        .root
        .map(|root| restore_node(root, world, settings, &format!("{path}.root"), ids))
        .transpose()?;
    let focus = if dto.focused_node_id == 0 {
        None
    } else {
        let root_id = root.ok_or_else(|| {
            RestoreError::new(
                format!("{path}.focusedNodeId"),
                "nonzero focus id with a null root",
            )
        })?;
        Some(
            world
                .tree
                .find_by_external_id(root_id, dto.focused_node_id)
                .ok_or_else(|| {
                    RestoreError::new(
                        format!("{path}.focusedNodeId"),
                        format!("node id {} is not in this desktop", dto.focused_node_id),
                    )
                })?,
        )
    };
    let layout = expect_enum!(parse_layout, dto.layout, path, "layout", "layout")?;
    let user_layout = expect_enum!(parse_layout, dto.user_layout, path, "userLayout", "layout")?;

    let desktop = world.create_desktop(external_id, Some(&dto.name), settings);
    let value = world.desktop_mut(desktop);
    value.layout = layout;
    value.user_layout = user_layout;
    value.window_gap = dto.window_gap;
    value.border_width = dto.border_width;
    value.padding = dto.padding;
    value.tree.root = root;
    value.tree.focus = focus;
    Ok(desktop)
}

fn restore_node(
    dto: NodeDto<'_>,
    world: &mut World,
    settings: &Settings,
    path: &str,
    ids: &mut Identities,
) -> Result<NodeId, RestoreError> {
    let external_id = claim(&mut ids.nodes, dto.id, path, "node")?;
    let split_ratio = finite_split_ratio(dto.split_ratio, path)?;
    if dto.first_child.is_some() != dto.second_child.is_some() {
        return Err(RestoreError::new(
            path,
            "node must have either zero or two children",
        ));
    }

    let node = world.tree.add_node(external_id, settings.split_ratio);
    let first = dto
        .first_child
        .map(|child| restore_node(*child, world, settings, &format!("{path}.firstChild"), ids))
        .transpose()?;
    let second = dto
        .second_child
        .map(|child| restore_node(*child, world, settings, &format!("{path}.secondChild"), ids))
        .transpose()?;
    if let (Some(first), Some(second)) = (first, second) {
        world.tree.set_children(node, first, second);
    }

    let split_type = expect_enum!(
        parse_split_type,
        dto.split_type,
        path,
        "splitType",
        "split type"
    )?;
    let client = dto
        .client
        .map(|client| restore_client(client, settings, &format!("{path}.client")))
        .transpose()?;
    let presel = dto
        .presel
        .map(|presel| restore_presel(&presel, &format!("{path}.presel")))
        .transpose()?;
    let value = world.tree.node_mut(node);
    value.split_type = split_type;
    value.split_ratio = split_ratio;
    value.vacant = dto.vacant;
    value.hidden = dto.hidden;
    value.sticky = dto.sticky;
    value.private = dto.private;
    value.locked = dto.locked;
    value.marked = dto.marked;
    value.presel = presel;
    value.rectangle = dto.rectangle;
    // set_children derives constraints; the dump is authoritative.
    value.constraints = dto.constraints;
    value.client = client;
    Ok(node)
}

fn restore_presel(dto: &PreselDto<'_>, path: &str) -> Result<Presel, RestoreError> {
    let split_ratio = finite_split_ratio(dto.split_ratio, path)?;
    let split_dir = expect_enum!(
        parse_direction,
        dto.split_dir,
        path,
        "splitDir",
        "direction"
    )?;
    Ok(Presel {
        split_dir,
        split_ratio,
        feedback: None,
    })
}

fn restore_client(
    dto: ClientDto<'_>,
    settings: &Settings,
    path: &str,
) -> Result<Client, RestoreError> {
    let mut client = Client::from_settings(settings);
    client.border_width = dto.border_width;
    client.state = expect_enum!(parse_client_state, dto.state, path, "state", "client state")?;
    client.last_state = expect_enum!(
        parse_client_state,
        dto.last_state,
        path,
        "lastState",
        "client state"
    )?;
    client.layer = expect_enum!(parse_stack_layer, dto.layer, path, "layer", "stack layer")?;
    client.last_layer = expect_enum!(
        parse_stack_layer,
        dto.last_layer,
        path,
        "lastLayer",
        "stack layer"
    )?;
    client.class_name = dto.class_name.into_owned();
    client.instance_name = dto.instance_name.into_owned();
    client.urgent = dto.urgent;
    client.shown = dto.shown;
    client.tiled_rectangle = dto.tiled_rectangle;
    client.floating_rectangle = dto.floating_rectangle;
    client.transient_for = dto.transient_for.filter(|id| *id != 0);
    Ok(client)
}

/// Records `id` as used, rejecting a second sighting of the same identity.
fn claim(seen: &mut HashSet<u32>, id: u32, path: &str, what: &str) -> Result<u32, RestoreError> {
    if seen.insert(id) {
        Ok(id)
    } else {
        Err(RestoreError::new(
            format!("{path}.id"),
            format!("duplicate {what} id {id}"),
        ))
    }
}

/// Rejects the split ratios that JSON can express but the tree cannot hold.
fn finite_split_ratio(value: f64, path: &str) -> Result<f64, RestoreError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(RestoreError::new(
            format!("{path}.splitRatio"),
            "number must be finite",
        ))
    }
}

fn restore_monitor_reference(
    world: &World,
    external_id: Option<u32>,
    path: &str,
) -> Result<Option<MonitorId>, RestoreError> {
    external_id
        .map(|external_id| {
            find_monitor(world, external_id).ok_or_else(|| {
                RestoreError::new(path, format!("monitor id {external_id} was not restored"))
            })
        })
        .transpose()
}

fn restore_history(world: &World, entries: Vec<CoordinatesDto>) -> History<MonitorId, DesktopId> {
    let mut history = History::default();
    for entry in entries {
        let Some(monitor) = find_monitor(world, entry.monitor_id) else {
            continue;
        };
        let Some(desktop) = world
            .monitor(monitor)
            .desktops
            .iter()
            .copied()
            .find(|desktop| world.desktop(*desktop).external_id == entry.desktop_id)
        else {
            continue;
        };
        let node = if entry.node_id == 0 {
            None
        } else {
            let Some(node) = world
                .desktop(desktop)
                .tree
                .root
                .and_then(|root| world.tree.find_by_external_id(root, entry.node_id))
            else {
                continue;
            };
            Some(node)
        };
        history.add(
            Coordinates {
                monitor,
                desktop,
                node,
            },
            true,
        );
    }
    history
}

fn restore_stacking(world: &World, ids: Vec<u32>) -> Result<StackingOrder, RestoreError> {
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    for (index, external_id) in ids.into_iter().enumerate() {
        let node = world.roots().find_map(|(_, _, root)| {
            world
                .tree
                .find_by_external_id(root, external_id)
                .filter(|node| world.tree.node(*node).client.is_some())
        });
        let Some(node) = node else {
            continue;
        };
        if !seen.insert(node) {
            return Err(RestoreError::new(
                format!("$.stackingList[{index}]"),
                format!("duplicate node id {external_id}"),
            ));
        }
        nodes.push(node);
    }
    Ok(StackingOrder::from_nodes(nodes))
}

fn find_monitor(world: &World, external_id: u32) -> Option<MonitorId> {
    world
        .monitor_order()
        .iter()
        .copied()
        .find(|monitor| world.monitor(*monitor).external_id == external_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::query_state;
    use crate::state::DaemonState;
    use crate::types::{
        ClientState, Constraints, Direction, Layout, Padding, Rectangle, SplitType, StackLayer,
    };
    use serde_json::Value;

    fn represented_state() -> DaemonState {
        let settings = Settings::default();
        let mut world = World::default();
        let monitor = world.create_monitor(
            10,
            Some("Display-1"),
            Rectangle::new(-10, 20, 800, 600),
            &settings,
        );
        {
            let value = world.monitor_mut(monitor);
            value.wired = false;
            value.sticky_count = 3;
            value.window_gap = -2;
            value.border_width = 4;
            value.padding = Padding {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4,
            };
        }
        let desktop = world.create_desktop(11, Some("I"), &settings);
        assert!(world.add_desktop(monitor, desktop));
        {
            let value = world.desktop_mut(desktop);
            value.layout = Layout::Monocle;
            value.user_layout = Layout::Tiled;
            value.window_gap = 7;
            value.border_width = 2;
            value.padding.left = 9;
        }

        let root = world.tree.add_node(256, 0.625);
        let first = world.tree.add_node(257, 0.5);
        let second = world.tree.add_node(258, 0.4);
        world.tree.set_children(root, first, second);
        {
            let value = world.tree.node_mut(root);
            value.split_type = SplitType::Horizontal;
            value.rectangle = Rectangle::new(1, 2, 300, 200);
            value.constraints = Constraints {
                min_width: 70,
                min_height: 40,
            };
            value.marked = true;
        }
        {
            let value = world.tree.node_mut(first);
            value.hidden = true;
            value.sticky = true;
            value.private = true;
            value.locked = true;
            value.vacant = true;
            value.presel = Some(Presel {
                split_dir: Direction::South,
                split_ratio: 0.25,
                feedback: Some(900),
            });
            let mut client = Client::from_settings(&settings);
            client.class_name = "Term".into();
            client.instance_name = "term".into();
            client.border_width = 3;
            client.state = ClientState::Floating;
            client.last_state = ClientState::PseudoTiled;
            client.layer = StackLayer::Above;
            client.last_layer = StackLayer::Below;
            client.urgent = true;
            client.shown = true;
            client.tiled_rectangle = Rectangle::new(3, 4, 50, 60);
            client.floating_rectangle = Rectangle::new(5, 6, 70, 80);
            value.client = Some(client);
        }
        world.desktop_mut(desktop).tree.root = Some(root);
        world.desktop_mut(desktop).tree.focus = Some(first);
        world.focused_monitor = Some(monitor);
        world.primary_monitor = Some(monitor);
        let mut state = DaemonState {
            world,
            clients_count: 1,
            ..DaemonState::default()
        };
        state.history.add(
            Coordinates {
                monitor,
                desktop,
                node: None,
            },
            true,
        );
        state.history.add(
            Coordinates {
                monitor,
                desktop,
                node: Some(first),
            },
            true,
        );
        let _ = state
            .stacking_order
            .stack(&state.world.tree, first, true, state.auto_raise);
        state
    }

    fn daemon_from_restored(restored: RestoredState) -> DaemonState {
        let mut state = DaemonState::default();
        state.apply_restored(restored);
        state
    }

    #[test]
    fn query_state_round_trips_all_represented_fields() {
        let settings = Settings::default();
        let state = query_state(&represented_state());
        let restored = restore_state(&state, &settings).unwrap();
        let restored = daemon_from_restored(restored);
        assert_eq!(query_state(&restored), state);
        let restored_first = restored
            .world
            .tree
            .find_by_external_id(
                restored
                    .world
                    .desktop(
                        restored
                            .world
                            .monitor(restored.world.focused_monitor.unwrap())
                            .active_desktop
                            .unwrap(),
                    )
                    .tree
                    .root
                    .unwrap(),
                257,
            )
            .unwrap();
        assert_eq!(
            restored
                .world
                .tree
                .node(restored_first)
                .presel
                .unwrap()
                .feedback,
            None
        );
        assert_eq!(restored.validate(), Ok(()));
    }

    #[test]
    fn empty_world_round_trips_without_fabricated_focus() {
        let settings = Settings::default();
        let state = query_state(&DaemonState::default());
        let restored = restore_state(&state, &settings).unwrap();
        let restored = daemon_from_restored(restored);
        assert_eq!(query_state(&restored), state);
        assert_eq!(restored.world.focused_monitor, None);
        assert_eq!(restored.world.primary_monitor, None);
    }

    #[test]
    fn restart_subscriber_metadata_is_restored_and_validated() {
        let mut value: Value = serde_json::from_str(&query_state(&DaemonState::default())).unwrap();
        value["eventSubscribers"] = serde_json::json!([
            {"fileDescriptor": 8, "field": 3, "count": -1},
            {"fileDescriptor": 9, "fifoPath": "/tmp/fifo", "field": 4, "count": 2}
        ]);
        let restored = restore_state(&value.to_string(), &Settings::default()).unwrap();
        assert_eq!(
            restored.event_subscribers,
            [
                RestoredSubscriber {
                    file_descriptor: 8,
                    fifo_path: None,
                    field: 3,
                    count: -1,
                },
                RestoredSubscriber {
                    file_descriptor: 9,
                    fifo_path: Some("/tmp/fifo".into()),
                    field: 4,
                    count: 2,
                },
            ]
        );

        value["eventSubscribers"][0]["field"] = serde_json::json!(u32::MAX);
        let error = restore_state(&value.to_string(), &Settings::default()).unwrap_err();
        assert_eq!(error.path, "$.eventSubscribers[0].field");
    }

    #[test]
    fn malformed_and_unresolved_inputs_have_precise_errors() {
        let settings = Settings::default();
        let error = restore_state("{\"clientsCount\":0,", &settings).unwrap_err();
        assert_eq!(error.path, "$");
        assert!(error.message.contains("EOF"));

        let error = restore_state(
            "{\"focusedMonitorId\":9,\"clientsCount\":0,\"monitors\":[]}",
            &settings,
        )
        .unwrap_err();
        assert_eq!(error.path, "$.focusedMonitorId");

        // Missing required keys are reported by serde against the whole document.
        let error = restore_state(
            "{\"clientsCount\":0,\"monitors\":[],\"focusHistory\":[{}]}",
            &settings,
        )
        .unwrap_err();
        assert_eq!(error.path, "$");
        assert!(error.message.contains("missing field `monitorId`"));

        let error = restore_state("{\"monitors\":[]}", &settings).unwrap_err();
        assert_eq!(error.path, "$");
        assert!(error.message.contains("missing field `clientsCount`"));
    }

    #[test]
    fn valid_json_is_order_independent_and_unknown_fields_are_ignored() {
        let state = query_state(&represented_state());
        let mut value: Value = serde_json::from_str(&state).unwrap();
        value["ignored"] = serde_json::json!({"nested": [1, 2]});
        let monitor = value["monitors"][0].as_object_mut().unwrap();
        let id = monitor.remove("id").unwrap();
        monitor.insert("id".into(), id);
        let restored = restore_state(&value.to_string(), &Settings::default()).unwrap();
        assert_eq!(query_state(&daemon_from_restored(restored)), state);
    }

    #[test]
    fn nullable_keys_may_be_omitted_entirely() {
        let mut value: Value = serde_json::from_str(&query_state(&represented_state())).unwrap();
        let desktop = value["monitors"][0]["desktops"][0].as_object_mut().unwrap();
        desktop.remove("root");
        desktop["focusedNodeId"] = serde_json::json!(0);
        value["clientsCount"] = serde_json::json!(0);
        let restored = restore_state(&value.to_string(), &Settings::default()).unwrap();
        let desktop = restored.world.monitor_order()[0];
        let desktop = restored.world.monitor(desktop).desktops[0];
        assert_eq!(restored.world.desktop(desktop).tree.root, None);
    }

    #[test]
    fn equal_monitor_rectangles_preserve_order_when_desktops_are_added() {
        let settings = Settings::default();
        let mut world = World::default();
        let rectangle = Rectangle::new(0, 0, 100, 100);
        let _ = world.create_monitor(1, Some("first"), rectangle, &settings);
        let _ = world.create_monitor(2, Some("second"), rectangle, &settings);
        let state = DaemonState {
            world,
            ..DaemonState::default()
        };
        let state = query_state(&state);
        let restored = restore_state(&state, &settings).unwrap();
        let names: Vec<_> = restored
            .world
            .monitor_order()
            .iter()
            .map(|monitor| restored.world.monitor(*monitor).name.as_str())
            .collect();
        assert_eq!(names, ["second", "first"]);
        assert!(
            restored.world.monitor_order().iter().all(|monitor| restored
                .world
                .monitor(*monitor)
                .desktops
                .len()
                == 1)
        );
    }

    #[test]
    fn representative_upstream_dump_restores_supported_state() {
        let original = represented_state();
        let mut dump: Value = serde_json::from_str(&query_state(&original)).unwrap();
        dump["monitors"][0]["randrId"] = serde_json::json!(77);
        dump["monitors"][0]["desktops"][0]["root"]["presel"] = Value::Null;
        dump["eventSubscribers"] = serde_json::json!([
            {"fileDescriptor": 9, "fifoPath": "/tmp/bspwm-fifo", "field": 3, "count": -1}
        ]);

        let restored = restore_state(&dump.to_string(), &Settings::default()).unwrap();
        let restored = daemon_from_restored(restored);
        assert_eq!(restored.history.entries().len(), 2);
        assert_eq!(restored.stacking_order.nodes().len(), 1);
        assert_eq!(restored.validate(), Ok(()));
        assert_eq!(
            restored
                .world
                .monitor(restored.world.monitor_order()[0])
                .randr_id,
            77
        );
    }

    #[test]
    fn empty_monitors_gain_a_desktop_and_fresh_server_ids_preserve_clients() {
        let settings = Settings::default();
        let mut value: Value = serde_json::from_str(&query_state(&represented_state())).unwrap();
        value["monitors"][0]["desktops"] = serde_json::json!([]);
        value["monitors"][0]["focusedDesktopId"] = Value::Null;
        value["clientsCount"] = serde_json::json!(0);
        value["focusHistory"] = serde_json::json!([]);
        value["stackingList"] = serde_json::json!([]);
        let restored = restore_state(&value.to_string(), &settings).unwrap();
        let monitor = restored.world.monitor_order()[0];
        assert_eq!(restored.world.monitor(monitor).desktops.len(), 1);
        assert!(restored.world.monitor(monitor).active_desktop.is_some());

        let mut restored = restore_state(&query_state(&represented_state()), &settings).unwrap();
        let old_client = restored.stacking_order.nodes()[0];
        let old_client_xid = restored.world.tree.node(old_client).external_id;
        let desktop = restored.world.monitor(monitor).desktops[0];
        let old_root = restored.world.desktop(desktop).tree.root.unwrap();
        let old_root_xid = restored.world.tree.node(old_root).external_id;
        let mut xid = 0xA000_0000;
        restored.regenerate_xids(|| {
            xid += 1;
            xid
        });
        assert_eq!(
            restored.world.tree.node(old_client).external_id,
            old_client_xid
        );
        assert_ne!(restored.world.tree.node(old_root).external_id, old_root_xid);
        assert_eq!(restored.stacking_order.nodes(), &[old_client]);
        assert!(
            restored
                .history
                .entries()
                .iter()
                .any(|entry| entry.location.node == Some(old_client))
        );
        assert_eq!(restored.world.monitor(monitor).external_id, 0xA000_0001);
        assert_eq!(restored.world.validate(), Ok(()));
    }
}
