#![allow(clippy::collapsible_if, clippy::too_many_lines)]

use crate::geometry::{contains, is_inside};
use crate::types::{Point, Rectangle};
use crate::world::MonitorId;

/// A monitor reported by a screen-discovery mechanism.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorInfo {
    pub output: Option<u32>,
    pub name: String,
    pub rectangle: Rectangle,
    pub connected: bool,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconciliationInput<'a> {
    pub monitors: &'a [MonitorInfo],
    pub primary_output: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MonitorHandle {
    Existing(MonitorId),
    Added(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExistingMonitor {
    pub id: MonitorId,
    pub output: Option<u32>,
    pub rectangle: Rectangle,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReconcileSettings {
    pub remove_disabled_monitors: bool,
    pub remove_unplugged_monitors: bool,
    pub merge_overlapping_monitors: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorUpdate {
    pub id: MonitorId,
    pub rectangle: Rectangle,
    pub wired: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorAddition {
    pub handle: MonitorHandle,
    pub info: MonitorInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitorRemoval {
    pub source: MonitorHandle,
    pub merge_into: Option<MonitorHandle>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconciliationPlan {
    pub updates: Vec<MonitorUpdate>,
    pub additions: Vec<MonitorAddition>,
    pub removals: Vec<MonitorRemoval>,
    pub primary: Option<MonitorHandle>,
}

#[derive(Clone, Copy)]
struct Candidate {
    handle: MonitorHandle,
    output: Option<u32>,
    rectangle: Rectangle,
    wired: bool,
    active: bool,
}

#[must_use]
pub fn monitor_from_point(monitors: &[(MonitorId, Rectangle)], point: Point) -> Option<MonitorId> {
    monitors
        .iter()
        .find_map(|(id, rectangle)| is_inside(point, *rectangle).then_some(*id))
}

#[must_use]
pub fn nearest_monitor_to_point(
    monitors: &[(MonitorId, Rectangle)],
    point: Point,
) -> Option<MonitorId> {
    monitor_from_point(monitors, point).or_else(|| {
        monitors
            .iter()
            .min_by_key(|(_, rectangle)| {
                let center_x = i64::from(rectangle.x) + i64::from(rectangle.width / 2);
                let center_y = i64::from(rectangle.y) + i64::from(rectangle.height / 2);
                (center_x - i64::from(point.x)).abs() + (center_y - i64::from(point.y)).abs()
            })
            .map(|(id, _)| *id)
    })
}

#[must_use]
pub fn monitor_from_client(
    monitors: &[(MonitorId, Rectangle)],
    client_rectangle: Rectangle,
) -> Option<MonitorId> {
    let center = Point {
        x: client_rectangle
            .x
            .saturating_add(client_rectangle.width / 2),
        y: client_rectangle
            .y
            .saturating_add(client_rectangle.height / 2),
    };
    nearest_monitor_to_point(monitors, center)
}

/// Builds the monitor mutations without changing daemon state. Removal entries
/// are ordered so callers can transfer desktops before deleting each monitor.
#[must_use]
pub fn reconcile_monitors(
    existing: &[ExistingMonitor],
    input: ReconciliationInput<'_>,
    settings: ReconcileSettings,
) -> ReconciliationPlan {
    let mut plan = ReconciliationPlan::default();
    let mut candidates: Vec<_> = existing
        .iter()
        .map(|monitor| Candidate {
            handle: MonitorHandle::Existing(monitor.id),
            output: monitor.output,
            rectangle: monitor.rectangle,
            wired: false,
            active: true,
        })
        .collect();
    let mut last_wired = None;

    for info in input.monitors {
        let found = if let Some(output) = info.output {
            // The `!wired` guard also keeps a duplicate output from resolving to
            // a monitor this pass has only just added, which has no id yet.
            candidates
                .iter()
                .position(|candidate| candidate.output == Some(output) && !candidate.wired)
        } else {
            candidates.iter().position(|candidate| {
                candidate.output.is_none()
                    && !candidate.wired
                    && candidate.rectangle == info.rectangle
            })
        };
        if info.enabled {
            if let Some(index) = found {
                let candidate = &mut candidates[index];
                candidate.rectangle = info.rectangle;
                candidate.wired = true;
                last_wired = Some(candidate.handle);
                plan.updates.push(MonitorUpdate {
                    id: existing_monitor_id(candidate.handle),
                    rectangle: info.rectangle,
                    wired: true,
                });
            } else {
                let handle = MonitorHandle::Added(plan.additions.len());
                candidates.push(Candidate {
                    handle,
                    output: info.output,
                    rectangle: info.rectangle,
                    wired: true,
                    active: true,
                });
                last_wired = Some(handle);
                plan.additions.push(MonitorAddition {
                    handle,
                    info: info.clone(),
                });
            }
        } else if !settings.remove_disabled_monitors && info.connected {
            if let Some(index) = found {
                candidates[index].wired = true;
            }
        }
    }

    for candidate in &candidates {
        if let MonitorHandle::Existing(id) = candidate.handle {
            if !plan.updates.iter().any(|update| update.id == id) {
                plan.updates.push(MonitorUpdate {
                    id,
                    rectangle: candidate.rectangle,
                    wired: candidate.wired,
                });
            }
        }
    }

    if settings.merge_overlapping_monitors {
        for outer in 0..candidates.len() {
            if !candidates[outer].active || !candidates[outer].wired {
                continue;
            }
            for inner in 0..candidates.len() {
                if inner == outer || !candidates[inner].active || !candidates[inner].wired {
                    continue;
                }
                if contains(candidates[outer].rectangle, candidates[inner].rectangle) {
                    let source = candidates[inner].handle;
                    let destination = candidates[outer].handle;
                    candidates[inner].active = false;
                    if last_wired == Some(source) {
                        last_wired = Some(destination);
                    }
                    plan.removals.push(MonitorRemoval {
                        source,
                        merge_into: Some(destination),
                    });
                }
            }
        }
    }

    if settings.remove_unplugged_monitors {
        for candidate in &mut candidates {
            if candidate.active && !candidate.wired {
                candidate.active = false;
                plan.removals.push(MonitorRemoval {
                    source: candidate.handle,
                    merge_into: last_wired.filter(|target| *target != candidate.handle),
                });
            }
        }
    }

    plan.primary = input.primary_output.and_then(|output| {
        candidates
            .iter()
            .find(|candidate| {
                candidate.active && candidate.wired && candidate.output == Some(output)
            })
            .map(|candidate| candidate.handle)
    });
    plan
}

fn existing_monitor_id(handle: MonitorHandle) -> MonitorId {
    match handle {
        MonitorHandle::Existing(id) => id,
        MonitorHandle::Added(_) => unreachable!("only existing monitors are updated"),
    }
}
