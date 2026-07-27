use std::io;

use super::CommandHandler;
use crate::messages::{Response, handle_failure};
use crate::parse::{
    parse_cycle_direction, parse_desktop_modifiers, parse_direction, parse_history_direction,
    parse_id, parse_index, parse_monitor_modifiers, parse_node_modifiers,
};
use crate::query::{
    Coordinates, desktop_from_id, desktop_from_index, desktop_from_name, desktop_matches,
    find_any_desktop, find_any_monitor, find_any_node, find_by_id, find_closest_desktop,
    find_closest_monitor, find_closest_node, find_first_ancestor, find_nearest_monitor,
    find_nearest_node, find_node_by_area, locate_desktop, locate_monitor, monitor_from_id,
    monitor_from_index, monitor_matches, node_matches,
};
use crate::rule::RuleConsequence;
use crate::types::{DesktopSelect, HistoryDirection, MonitorSelect};
use crate::world::{DesktopId, MonitorId, World};

/// `last` is the same lookup as `older`, so both feed the history branch.
fn history_direction(descriptor: &str) -> Option<HistoryDirection> {
    parse_history_direction(descriptor)
        .or_else(|| (descriptor == "last").then_some(HistoryDirection::Older))
}

impl CommandHandler<'_> {
    pub(super) fn reference(&self) -> Coordinates {
        let monitor = self.state.world.focused_monitor;
        let desktop = monitor.and_then(|id| self.state.world.monitor(id).active_desktop);
        let node = desktop.and_then(|id| self.state.world.desktop(id).tree.focus);
        Coordinates {
            monitor,
            desktop,
            node,
        }
    }

    /// Resolves placement descriptors in the same priority order as upstream.
    pub(crate) fn resolve_rule_target(&mut self, consequence: &RuleConsequence) -> Coordinates {
        let reference = self.reference();
        if !consequence.node_desc.is_empty()
            && let Resolve::Ok(location) =
                self.resolve_node(consequence.node_desc.as_bytes(), reference)
        {
            return location;
        }
        if !consequence.desktop_desc.is_empty()
            && let Resolve::Ok(location) = self.resolve_desktop(
                consequence.desktop_desc.as_bytes(),
                Coordinates {
                    node: None,
                    ..reference
                },
            )
        {
            return Coordinates {
                node: location
                    .desktop
                    .and_then(|desktop| self.state.world.desktop(desktop).tree.focus),
                ..location
            };
        }
        if !consequence.monitor_desc.is_empty()
            && let Resolve::Ok(location) = self.resolve_monitor(
                consequence.monitor_desc.as_bytes(),
                Coordinates {
                    desktop: None,
                    node: None,
                    ..reference
                },
            )
            && let Some(monitor) = location.monitor
        {
            let desktop = self.state.world.monitor(monitor).active_desktop;
            return Coordinates {
                monitor: Some(monitor),
                desktop,
                node: desktop.and_then(|desktop| self.state.world.desktop(desktop).tree.focus),
            };
        }
        reference
    }

    /// Replaces valid rule placement selectors with the concrete IDs passed to
    /// an external rule command. Invalid selectors become empty, as upstream.
    pub(crate) fn resolve_rule_consequence(&mut self, consequence: &mut RuleConsequence) {
        let reference = self.reference();
        let monitor = match self.resolve_monitor(consequence.monitor_desc.as_bytes(), reference) {
            Resolve::Ok(location) => location.monitor,
            _ => None,
        };
        let desktop = match self.resolve_desktop(consequence.desktop_desc.as_bytes(), reference) {
            Resolve::Ok(location) => location.desktop,
            _ => None,
        };
        let node = match self.resolve_node(consequence.node_desc.as_bytes(), reference) {
            Resolve::Ok(location) => location.node,
            _ => None,
        };
        consequence.monitor_desc = monitor.map_or_else(String::new, |id| {
            format!("0x{:08X}", self.state.world.monitor(id).external_id)
        });
        consequence.desktop_desc = desktop.map_or_else(String::new, |id| {
            format!("0x{:08X}", self.state.world.desktop(id).external_id)
        });
        consequence.node_desc = node.map_or_else(String::new, |id| {
            format!("0x{:08X}", self.state.world.tree.node(id).external_id)
        });
    }

    pub(super) fn resolve_monitor(&mut self, value: &[u8], mut reference: Coordinates) -> Resolve {
        let Ok(value) = std::str::from_utf8(value) else {
            return Resolve::BadDescriptor;
        };
        if let Some(name) = value.strip_prefix('%') {
            return locate_monitor(&self.state.world, name).map_or(Resolve::Invalid, Resolve::Ok);
        }
        let mut descriptor = value;
        if let Some(hash) = descriptor.rfind('#') {
            let (reference_descriptor, rest) = (&descriptor[..hash], &descriptor[hash + 1..]);
            let focused = Coordinates {
                monitor: self.state.world.focused_monitor,
                ..Coordinates::default()
            };
            let result = self.resolve_monitor(reference_descriptor.as_bytes(), focused);
            let Resolve::Ok(loc) = result else {
                return result;
            };
            reference = loc;
            descriptor = rest;
        }
        let Some((descriptor, selector)) = parse_monitor_modifiers(descriptor) else {
            return Resolve::BadModifiers;
        };
        let result = if let Some(direction) = parse_direction(descriptor) {
            find_nearest_monitor(&self.state.world, reference, direction, &selector)
        } else if let Some(direction) = parse_cycle_direction(descriptor) {
            find_closest_monitor(&self.state.world, reference, direction, &selector)
        } else if let Some(direction) = history_direction(descriptor) {
            let Some(reference_monitor) = reference.monitor else {
                return Resolve::Invalid;
            };
            let matches = monitor_predicate(&self.state.world, &selector);
            self.state
                .history
                .find_monitor(direction, reference_monitor, matches)
                .map(history_coordinates)
        } else {
            match descriptor {
                "focused" => reference.monitor.map(Coordinates::monitor),
                "primary" => self.state.world.primary_monitor.map(Coordinates::monitor),
                "pointed" => None,
                "any" => find_any_monitor(&self.state.world, &selector),
                "newest" => self
                    .state
                    .history
                    .find_newest_monitor(monitor_predicate(&self.state.world, &selector))
                    .map(history_coordinates),
                name if name.starts_with('^') => {
                    let result = parse_index(name)
                        .and_then(|index| monitor_from_index(&self.state.world, i32::from(index)))
                        .or_else(|| locate_monitor(&self.state.world, name));
                    if result.is_none() {
                        return Resolve::BadDescriptor;
                    }
                    result
                }
                name => {
                    let result = parse_id(name)
                        .and_then(|id| monitor_from_id(&self.state.world, id))
                        .or_else(|| locate_monitor(&self.state.world, name));
                    if result.is_none() {
                        return Resolve::BadDescriptor;
                    }
                    result
                }
            }
        };
        result
            .filter(|loc| monitor_matches(&self.state.world, *loc, &selector))
            .map_or(Resolve::Invalid, Resolve::Ok)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_desktop(&mut self, value: &[u8], mut reference: Coordinates) -> Resolve {
        let Ok(value) = std::str::from_utf8(value) else {
            return Resolve::BadDescriptor;
        };
        if let Some(name) = value.strip_prefix('%') {
            return locate_desktop(&self.state.world, name).map_or(Resolve::Invalid, Resolve::Ok);
        }
        let mut descriptor = value;
        let colon = descriptor.rfind(':');
        if let Some(hash) = descriptor
            .rfind('#')
            .filter(|hash| colon.is_none_or(|colon| *hash > colon))
        {
            let focused = self.reference();
            let result = self.resolve_desktop(
                &descriptor.as_bytes()[..hash],
                Coordinates {
                    node: None,
                    ..focused
                },
            );
            let Resolve::Ok(loc) = result else {
                return result;
            };
            reference = loc;
            descriptor = &descriptor[hash + 1..];
        }
        let modifier_source = descriptor
            .rsplit_once(':')
            .map_or(descriptor, |(_, desktop)| desktop);
        let Some((_, selector)) = parse_desktop_modifiers(modifier_source) else {
            return Resolve::BadModifiers;
        };
        let plain_descriptor =
            parse_desktop_modifiers(descriptor).map_or(descriptor, |(plain, _)| plain);
        let result = if let Some(direction) = parse_cycle_direction(plain_descriptor) {
            find_closest_desktop(&self.state.world, reference, direction, &selector)
        } else if let Some(direction) = history_direction(plain_descriptor) {
            let Some(reference_desktop) = reference.desktop else {
                return Resolve::Invalid;
            };
            let matches = desktop_predicate(&self.state.world, reference, &selector);
            self.state
                .history
                .find_desktop(direction, reference_desktop, matches)
                .map(history_coordinates)
        } else {
            match plain_descriptor {
                "focused" => reference.desktop.map(|desktop| Coordinates {
                    monitor: self.state.world.desktop_monitor(desktop),
                    desktop: Some(desktop),
                    node: None,
                }),
                "any" => find_any_desktop(&self.state.world, reference, &selector),
                "newest" => self
                    .state
                    .history
                    .find_newest_desktop(desktop_predicate(&self.state.world, reference, &selector))
                    .map(history_coordinates),
                name if name.starts_with('^') => {
                    let result = parse_index(name)
                        .and_then(|index| desktop_from_index(&self.state.world, index, None));
                    if result.is_some() {
                        result
                    } else {
                        let (named, hits) =
                            desktop_from_name(&self.state.world, name, reference, &selector);
                        if named.is_some() {
                            named
                        } else if hits > 0 {
                            None
                        } else {
                            return Resolve::BadDescriptor;
                        }
                    }
                }
                name if name.contains(':') => {
                    let (monitor_descriptor, desktop_descriptor) =
                        name.rsplit_once(':').expect("contains colon");
                    let monitor_result =
                        self.resolve_monitor(monitor_descriptor.as_bytes(), reference);
                    let Resolve::Ok(monitor_loc) = monitor_result else {
                        return monitor_result;
                    };
                    let Some(monitor) = monitor_loc.monitor else {
                        return Resolve::Invalid;
                    };
                    if desktop_descriptor == "focused" {
                        self.state
                            .world
                            .monitor(monitor)
                            .active_desktop
                            .map(|desktop| Coordinates::desktop(monitor, desktop))
                    } else if desktop_descriptor.starts_with('^') {
                        parse_index(desktop_descriptor).and_then(|index| {
                            desktop_from_index(&self.state.world, index, Some(monitor))
                        })
                    } else {
                        return Resolve::BadDescriptor;
                    }
                }
                name => {
                    let (named, hits) =
                        desktop_from_name(&self.state.world, name, reference, &selector);
                    if let Some(id) = parse_id(name) {
                        let result = desktop_from_id(&self.state.world, id, None).or(named);
                        if result.is_none() {
                            return Resolve::BadDescriptor;
                        }
                        result
                    } else if named.is_some() {
                        named
                    } else if hits > 0 {
                        None
                    } else {
                        return Resolve::BadDescriptor;
                    }
                }
            }
        };
        result
            .filter(|loc| desktop_matches(&self.state.world, *loc, reference, &selector))
            .map_or(Resolve::Invalid, Resolve::Ok)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_node(&mut self, value: &[u8], mut reference: Coordinates) -> Resolve {
        let Ok(value) = std::str::from_utf8(value) else {
            return Resolve::BadDescriptor;
        };
        let mut descriptor = value;
        let mut hash = descriptor.rfind('#');
        if let (Some(candidate), Some(path), Some(colon)) =
            (hash, descriptor.rfind('@'), descriptor.rfind(':'))
            && path < candidate
            && candidate < colon
        {
            hash = (path > 0 && descriptor.as_bytes()[path - 1] == b'#').then_some(path - 1);
        }
        if let Some(hash) = hash {
            let (reference_descriptor, rest) = (&descriptor[..hash], &descriptor[hash + 1..]);
            let focused = self.reference();
            let result = self.resolve_node(reference_descriptor.as_bytes(), focused);
            let Resolve::Ok(loc) = result else {
                return result;
            };
            reference = loc;
            descriptor = rest;
        }
        let Some((descriptor, selector)) = parse_node_modifiers(descriptor) else {
            return Resolve::BadModifiers;
        };
        let result = if let Some(direction) = parse_direction(descriptor) {
            find_nearest_node(
                &self.state.world,
                reference,
                direction,
                self.state.settings.directional_focus_tightness,
                |node| self.state.history.rank(node),
                &selector,
            )
        } else if let Some(direction) = parse_cycle_direction(descriptor) {
            find_closest_node(&self.state.world, reference, direction, &selector)
        } else if let Some(direction) = history_direction(descriptor) {
            let world = &self.state.world;
            self.state
                .history
                .find_node(
                    &self.state.world.tree,
                    direction,
                    reference.node,
                    |location| {
                        let loc = history_coordinates(location);
                        loc.node.is_some_and(|node| !world.tree.node(node).hidden)
                            && node_matches(world, loc, reference, &selector)
                    },
                )
                .map(history_coordinates)
        } else {
            match descriptor {
                "focused" => Some(reference),
                "pointed" => None,
                "any" => find_any_node(&self.state.world, reference, &selector),
                "first_ancestor" => find_first_ancestor(&self.state.world, reference, &selector),
                "biggest" => find_node_by_area(&self.state.world, reference, true, &selector),
                "smallest" => find_node_by_area(&self.state.world, reference, false, &selector),
                "newest" => self
                    .state
                    .history
                    .find_newest_node(&self.state.world.tree, |location| {
                        node_matches(
                            &self.state.world,
                            history_coordinates(location),
                            reference,
                            &selector,
                        )
                    })
                    .map(history_coordinates),
                path if path.starts_with('@') => {
                    let body = &path[1..];
                    let (desktop_descriptor, moves) = body
                        .rsplit_once(':')
                        .map_or((None, body), |(desktop, moves)| (Some(desktop), moves));
                    let mut loc = reference;
                    if let Some(desktop_descriptor) = desktop_descriptor {
                        let desktop_result =
                            self.resolve_desktop(desktop_descriptor.as_bytes(), reference);
                        let Resolve::Ok(desktop_loc) = desktop_result else {
                            return desktop_result;
                        };
                        loc = desktop_loc;
                        loc.node = loc
                            .desktop
                            .and_then(|desktop| self.state.world.desktop(desktop).tree.focus);
                    }
                    if moves.starts_with('/') {
                        loc.node = loc
                            .desktop
                            .and_then(|desktop| self.state.world.desktop(desktop).tree.root);
                    }
                    for movement in moves.split('/').filter(|movement| !movement.is_empty()) {
                        let Some(node) = loc.node else {
                            break;
                        };
                        loc.node = match movement {
                            "first" | "1" => self.state.world.tree.node(node).first_child,
                            "second" | "2" => self.state.world.tree.node(node).second_child,
                            "parent" => self.state.world.tree.node(node).parent,
                            "brother" => self.state.world.tree.sibling(node),
                            movement => {
                                let Some(direction) = parse_direction(movement) else {
                                    return Resolve::BadDescriptor;
                                };
                                self.state.world.tree.find_fence(node, direction)
                            }
                        };
                    }
                    if loc.node.is_none()
                        && loc.desktop.is_some_and(|desktop| {
                            self.state.world.desktop(desktop).tree.root.is_some()
                        })
                    {
                        None
                    } else if loc.node.is_none() {
                        return Resolve::Ok(loc);
                    } else {
                        Some(loc)
                    }
                }
                descriptor => {
                    let Some(id) = parse_id(descriptor) else {
                        return Resolve::BadDescriptor;
                    };
                    return find_by_id(&self.state.world, id)
                        .filter(|loc| node_matches(&self.state.world, *loc, reference, &selector))
                        .map_or(Resolve::Invalid, Resolve::Ok);
                }
            }
        };
        result
            .filter(|loc| node_matches(&self.state.world, *loc, reference, &selector))
            .map_or(Resolve::Invalid, Resolve::Ok)
    }

    /// The opening shared by `handle_node`, `handle_desktop`, and
    /// `handle_monitor`: reject an empty argument list, resolve the optional
    /// leading selector against the focused location, and reject a selector
    /// that is not followed by a command.
    ///
    /// Yields `(reference, target, index)`; `Ok(None)` means the diagnostic has
    /// already been written to `rsp` and the handler must stop.
    pub(super) fn domain_preamble(
        &mut self,
        args: &[&[u8]],
        domain: &[u8],
        resolve: fn(&mut Self, &[u8], Coordinates) -> Resolve,
        rsp: &mut dyn Response,
    ) -> io::Result<Option<(Coordinates, Coordinates, usize)>> {
        let Some(first) = args.first() else {
            crate::messages::fail_parts(rsp, &[domain, b": Missing arguments.\n"])?;
            return Ok(None);
        };
        let reference = self.reference();
        let mut target = reference;
        let mut index = 0;
        if !first.starts_with(b"-") {
            let Some(loc) =
                Self::selector_failure(resolve(self, first, reference), domain, first, rsp)?
            else {
                return Ok(None);
            };
            target = loc;
            index = 1;
        }
        if index == args.len() {
            crate::messages::fail_parts(rsp, &[domain, b": Missing commands.\n"])?;
            return Ok(None);
        }
        Ok(Some((reference, target, index)))
    }

    pub(super) fn selector_failure(
        result: Resolve,
        source: &[u8],
        value: &[u8],
        rsp: &mut dyn Response,
    ) -> io::Result<Option<Coordinates>> {
        match result {
            Resolve::Ok(loc) => Ok(Some(loc)),
            Resolve::Invalid => {
                handle_failure(crate::messages::SELECTOR_INVALID, source, value, rsp)?;
                Ok(None)
            }
            Resolve::BadModifiers => {
                handle_failure(crate::messages::SELECTOR_BAD_MODIFIERS, source, value, rsp)?;
                Ok(None)
            }
            Resolve::BadDescriptor => {
                handle_failure(crate::messages::SELECTOR_BAD_DESCRIPTOR, source, value, rsp)?;
                Ok(None)
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Resolve {
    Ok(Coordinates),
    Invalid,
    BadModifiers,
    BadDescriptor,
}

type HistoryLocation = crate::history::Coordinates<MonitorId, DesktopId>;

fn history_coordinates(location: HistoryLocation) -> Coordinates {
    Coordinates::in_desktop(location.monitor, location.desktop, location.node)
}

fn monitor_predicate<'a>(
    world: &'a World,
    selector: &'a MonitorSelect,
) -> impl Fn(HistoryLocation) -> bool + 'a {
    move |location| monitor_matches(world, history_coordinates(location), selector)
}

fn desktop_predicate<'a>(
    world: &'a World,
    reference: Coordinates,
    selector: &'a DesktopSelect,
) -> impl Fn(HistoryLocation) -> bool + 'a {
    move |location| desktop_matches(world, history_coordinates(location), reference, selector)
}
