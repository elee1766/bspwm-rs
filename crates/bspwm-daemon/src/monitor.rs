#![allow(
    clippy::cast_possible_truncation,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use crate::geometry::{boundary_distance, on_dir_side};
use crate::types::{Direction, Rectangle, Tightness, wrapping_i16, wrapping_u16};
use crate::world::MonitorId;
use crate::x11::X11;
use xcb::{Xid, XidNew, randr, x, xinerama};

pub use bspwm_model::monitor::{
    ExistingMonitor, MonitorAddition, MonitorHandle, MonitorInfo, MonitorRemoval, MonitorUpdate,
    ReconcileSettings, ReconciliationInput, ReconciliationPlan, monitor_from_client,
    monitor_from_point, nearest_monitor_to_point,
};

const FALLBACK_MONITOR_NAME: &str = "MONITOR";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorInfoSource {
    Randr,
    Xinerama,
    Root,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorQuery {
    pub monitors: Vec<MonitorInfo>,
    pub primary_output: Option<u32>,
    pub source: MonitorInfoSource,
}

/// Queries RandR first, then Xinerama, and always has the root screen as a
/// final fallback. Individual broken RandR outputs are ignored.
#[must_use]
pub fn query_monitor_info(x11: &X11) -> MonitorQuery {
    if x11.extensions().randr.is_some() {
        if let Ok(query) = query_randr_monitor_info(x11) {
            if query.monitors.iter().any(|monitor| monitor.enabled) {
                return query;
            }
        }
    }

    if x11.extensions().xinerama.is_some() && xinerama_is_active(x11) {
        if let Ok(monitors) = query_xinerama(x11) {
            if !monitors.is_empty() {
                return MonitorQuery {
                    monitors,
                    primary_output: None,
                    source: MonitorInfoSource::Xinerama,
                };
            }
        }
    }

    let geometry = x11.geometry();
    MonitorQuery {
        monitors: vec![MonitorInfo {
            output: None,
            name: FALLBACK_MONITOR_NAME.into(),
            rectangle: Rectangle::from_x11(geometry.x, geometry.y, geometry.width, geometry.height),
            connected: true,
            enabled: true,
        }],
        primary_output: None,
        source: MonitorInfoSource::Root,
    }
}

fn xinerama_is_active(x11: &X11) -> bool {
    x11.request(&xinerama::IsActive {})
        .is_ok_and(|reply| reply.state() != 0)
}

/// Queries the current RandR outputs without applying fallback discovery.
///
/// # Errors
/// Returns an X error when screen resources cannot be retrieved.
pub fn query_randr_monitor_info(x11: &X11) -> xcb::Result<MonitorQuery> {
    // Both root queries are issued before either reply is awaited.
    let resources_cookie = x11.send(&randr::GetScreenResources { window: x11.root() });
    let primary_cookie = x11.send(&randr::GetOutputPrimary { window: x11.root() });
    let connection = x11.connection();
    let resources = connection.wait_for_reply(resources_cookie)?;
    let primary_output = connection
        .wait_for_reply(primary_cookie)
        .ok()
        .map(|reply| reply.output().resource_id())
        .filter(|output| *output != 0);
    let mut monitors = Vec::new();

    for output in resources.outputs() {
        let Ok(info) = x11.request(&randr::GetOutputInfo {
            output: *output,
            config_timestamp: x::CURRENT_TIME,
        }) else {
            continue;
        };
        let connected = info.connection() != randr::Connection::Disconnected;
        let crtc = info.crtc();
        let enabled = !crtc.is_none();
        let rectangle = if enabled {
            let Ok(crtc_info) = x11.request(&randr::GetCrtcInfo {
                crtc,
                config_timestamp: x::CURRENT_TIME,
            }) else {
                continue;
            };
            Rectangle::from_x11(
                crtc_info.x(),
                crtc_info.y(),
                crtc_info.width(),
                crtc_info.height(),
            )
        } else {
            Rectangle::default()
        };
        monitors.push(MonitorInfo {
            output: Some(output.resource_id()),
            name: String::from_utf8_lossy(info.name()).into_owned(),
            rectangle,
            connected,
            enabled,
        });
    }

    Ok(MonitorQuery {
        monitors,
        primary_output,
        source: MonitorInfoSource::Randr,
    })
}

fn query_xinerama(x11: &X11) -> xcb::Result<Vec<MonitorInfo>> {
    let reply = x11.request(&xinerama::QueryScreens {})?;
    Ok(reply
        .screen_info()
        .iter()
        .map(|screen| MonitorInfo {
            output: None,
            name: FALLBACK_MONITOR_NAME.into(),
            rectangle: Rectangle::from_x11(screen.x_org, screen.y_org, screen.width, screen.height),
            connected: true,
            enabled: true,
        })
        .collect())
}

/// Creates the input-only desktop window used to detect pointer entry for a monitor.
///
/// # Errors
/// Returns the first X protocol error encountered while creating or initializing the window.
pub fn create_monitor_root(
    x11: &X11,
    name: &str,
    rectangle: Rectangle,
    map: bool,
) -> xcb::ProtocolResult<u32> {
    let root = x11.connection().generate_id::<x::Window>();
    x11.send_and_check_request(&x::CreateWindow {
        depth: x::COPY_FROM_PARENT as u8,
        wid: root,
        parent: x11.root(),
        x: wrapping_i16(rectangle.x),
        y: wrapping_i16(rectangle.y),
        width: wrapping_u16(rectangle.width),
        height: wrapping_u16(rectangle.height),
        border_width: 0,
        class: x::WindowClass::InputOnly,
        visual: x::COPY_FROM_PARENT,
        value_list: &[x::Cw::EventMask(x::EventMask::ENTER_WINDOW)],
    })?;
    for request in [
        x11.send_and_check_request(&x::ChangeProperty {
            mode: x::PropMode::Replace,
            window: root,
            property: x11.atoms().wm_class,
            r#type: x::ATOM_STRING,
            data: b"root\0Bspwm\0",
        }),
        x11.send_and_check_request(&x::ChangeProperty {
            mode: x::PropMode::Replace,
            window: root,
            property: x11.atoms().wm_name,
            r#type: x::ATOM_STRING,
            data: name.as_bytes(),
        }),
        x11.send_and_check_request(&x::ChangeProperty {
            mode: x::PropMode::Replace,
            window: root,
            property: x11.atoms().net_wm_window_type,
            r#type: x::ATOM_ATOM,
            data: &[x11.atoms().net_wm_window_type_desktop.resource_id()],
        }),
    ] {
        request?;
    }
    x11.send_and_check_request(&x::ConfigureWindow {
        window: root,
        value_list: &[x::ConfigWindow::StackMode(x::StackMode::Below)],
    })?;
    if map {
        x11.send_and_check_request(&x::MapWindow { window: root })?;
    }
    Ok(root.resource_id())
}

/// Updates the name, position, and size of an existing monitor root.
///
/// # Errors
/// Returns an X protocol error if the window cannot be configured.
pub fn update_monitor_root(
    x11: &X11,
    root: u32,
    name: &str,
    rectangle: Rectangle,
) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window: x::Window::new(root),
        property: x11.atoms().wm_name,
        r#type: x::ATOM_STRING,
        data: name.as_bytes(),
    })?;
    x11.send_and_check_request(&x::ConfigureWindow {
        window: x::Window::new(root),
        value_list: &[
            x::ConfigWindow::X(rectangle.x),
            x::ConfigWindow::Y(rectangle.y),
            x::ConfigWindow::Width(u32::try_from(rectangle.width).unwrap_or(0)),
            x::ConfigWindow::Height(u32::try_from(rectangle.height).unwrap_or(0)),
        ],
    })
}

/// # Errors
/// Returns an X protocol error if the monitor root cannot be destroyed.
pub fn destroy_monitor_root(x11: &X11, root: u32) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::DestroyWindow {
        window: x::Window::new(root),
    })
}

/// # Errors
/// Returns an X protocol error if RandR event selection fails.
pub fn select_randr_screen_change(x11: &X11) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&randr::SelectInput {
        window: x11.root(),
        enable: randr::NotifyMask::SCREEN_CHANGE,
    })
}

#[must_use]
pub fn nearest_monitor_in_direction<F>(
    monitors: &[(MonitorId, Rectangle)],
    source: MonitorId,
    direction: Direction,
    tightness: Tightness,
    mut matches: F,
) -> Option<MonitorId>
where
    F: FnMut(MonitorId) -> bool,
{
    let source_rectangle = monitors
        .iter()
        .find_map(|(id, rectangle)| (*id == source).then_some(*rectangle))?;
    monitors
        .iter()
        .filter(|(id, rectangle)| {
            *id != source
                && matches(*id)
                && on_dir_side(source_rectangle, *rectangle, direction, tightness)
        })
        .min_by_key(|(_, rectangle)| boundary_distance(source_rectangle, *rectangle, direction))
        .map(|(id, _)| *id)
}

/// Builds the monitor mutations without changing daemon state. Removal entries
/// are ordered so callers can transfer desktops before deleting each monitor.
#[must_use]
pub fn reconcile_monitors(
    existing: &[ExistingMonitor],
    query: &MonitorQuery,
    settings: ReconcileSettings,
) -> ReconciliationPlan {
    bspwm_model::monitor::reconcile_monitors(
        existing,
        ReconciliationInput {
            monitors: &query.monitors,
            primary_output: query.primary_output,
        },
        settings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::types::Point;
    use crate::world::World;

    fn monitor_ids(rectangles: &[Rectangle]) -> (World, Vec<MonitorId>) {
        let mut world = World::default();
        let settings = Settings::default();
        let ids = rectangles
            .iter()
            .enumerate()
            .map(|(index, rectangle)| {
                world.create_monitor(index as u32, None, *rectangle, &settings)
            })
            .collect();
        (world, ids)
    }

    fn randr_query(monitors: Vec<MonitorInfo>, primary_output: Option<u32>) -> MonitorQuery {
        MonitorQuery {
            monitors,
            primary_output,
            source: MonitorInfoSource::Randr,
        }
    }

    fn output(output: u32, rectangle: Rectangle) -> MonitorInfo {
        MonitorInfo {
            output: Some(output),
            name: format!("output-{output}"),
            rectangle,
            connected: true,
            enabled: true,
        }
    }

    #[test]
    fn point_and_client_selection_use_containment_then_nearest_center() {
        let rectangles = [
            Rectangle::new(0, 0, 100, 100),
            Rectangle::new(200, 0, 100, 100),
        ];
        let (_world, ids) = monitor_ids(&rectangles);
        let monitors = [(ids[0], rectangles[0]), (ids[1], rectangles[1])];
        assert_eq!(
            monitor_from_point(&monitors, Point { x: 99, y: 50 }),
            Some(ids[0])
        );
        assert_eq!(monitor_from_point(&monitors, Point { x: 100, y: 50 }), None);
        assert_eq!(
            monitor_from_client(&monitors, Rectangle::new(150, 20, 20, 20)),
            Some(ids[1])
        );
    }

    #[test]
    fn directional_selection_uses_boundary_distance_and_filter() {
        let rectangles = [
            Rectangle::new(100, 100, 100, 100),
            Rectangle::new(0, 100, 50, 100),
            Rectangle::new(50, 100, 40, 100),
        ];
        let (_world, ids) = monitor_ids(&rectangles);
        let monitors: Vec<_> = ids.iter().copied().zip(rectangles).collect();
        assert_eq!(
            nearest_monitor_in_direction(
                &monitors,
                ids[0],
                Direction::West,
                Tightness::High,
                |_| true,
            ),
            Some(ids[2])
        );
        assert_eq!(
            nearest_monitor_in_direction(
                &monitors,
                ids[0],
                Direction::West,
                Tightness::High,
                |id| id != ids[2],
            ),
            Some(ids[1])
        );
    }

    #[test]
    fn reconciliation_updates_adds_and_tracks_primary() {
        let rectangles = [Rectangle::new(0, 0, 100, 100)];
        let (_world, ids) = monitor_ids(&rectangles);
        let existing = [ExistingMonitor {
            id: ids[0],
            output: Some(10),
            rectangle: rectangles[0],
        }];
        let query = randr_query(
            vec![
                output(10, Rectangle::new(0, 0, 120, 100)),
                output(11, Rectangle::new(120, 0, 100, 100)),
            ],
            Some(11),
        );
        let plan = reconcile_monitors(&existing, &query, ReconcileSettings::default());
        assert_eq!(plan.updates[0].rectangle.width, 120);
        assert_eq!(plan.additions.len(), 1);
        assert_eq!(plan.primary, Some(MonitorHandle::Added(0)));
        assert!(plan.removals.is_empty());
    }

    #[test]
    fn duplicate_outputs_add_a_monitor_instead_of_panicking() {
        let rectangles = [Rectangle::new(0, 0, 100, 100)];
        let (_world, ids) = monitor_ids(&rectangles);
        let existing = [ExistingMonitor {
            id: ids[0],
            output: Some(10),
            rectangle: rectangles[0],
        }];
        // A server that reports the same output twice must not resolve the
        // second report onto the monitor this pass just added, which has no id.
        let query = randr_query(
            vec![
                output(10, Rectangle::new(0, 0, 100, 100)),
                output(10, Rectangle::new(100, 0, 100, 100)),
            ],
            None,
        );
        let plan = reconcile_monitors(&existing, &query, ReconcileSettings::default());
        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].id, ids[0]);
        assert_eq!(plan.additions.len(), 1);
    }

    #[test]
    fn disabled_and_unplugged_settings_control_removal() {
        let rectangle = Rectangle::new(0, 0, 100, 100);
        let (_world, ids) = monitor_ids(&[rectangle, Rectangle::new(100, 0, 100, 100)]);
        let existing = [
            ExistingMonitor {
                id: ids[0],
                output: Some(10),
                rectangle,
            },
            ExistingMonitor {
                id: ids[1],
                output: Some(11),
                rectangle: Rectangle::new(100, 0, 100, 100),
            },
        ];
        let mut disabled = output(10, Rectangle::default());
        disabled.enabled = false;
        let query = randr_query(vec![disabled, output(12, rectangle)], None);
        let plan = reconcile_monitors(
            &existing,
            &query,
            ReconcileSettings {
                remove_disabled_monitors: false,
                remove_unplugged_monitors: true,
                merge_overlapping_monitors: false,
            },
        );
        assert!(
            !plan
                .removals
                .iter()
                .any(|entry| entry.source == MonitorHandle::Existing(ids[0]))
        );
        assert!(plan.removals.iter().any(|entry| {
            entry.source == MonitorHandle::Existing(ids[1])
                && entry.merge_into == Some(MonitorHandle::Added(0))
        }));
    }

    #[test]
    fn overlap_plan_merges_contained_monitor() {
        let outer = Rectangle::new(0, 0, 200, 200);
        let inner = Rectangle::new(50, 50, 100, 100);
        let (_world, ids) = monitor_ids(&[outer, inner]);
        let existing = [
            ExistingMonitor {
                id: ids[0],
                output: Some(10),
                rectangle: outer,
            },
            ExistingMonitor {
                id: ids[1],
                output: Some(11),
                rectangle: inner,
            },
        ];
        let query = randr_query(vec![output(10, outer), output(11, inner)], Some(11));
        let plan = reconcile_monitors(
            &existing,
            &query,
            ReconcileSettings {
                merge_overlapping_monitors: true,
                ..ReconcileSettings::default()
            },
        );
        assert_eq!(
            plan.removals,
            [MonitorRemoval {
                source: MonitorHandle::Existing(ids[1]),
                merge_into: Some(MonitorHandle::Existing(ids[0])),
            }]
        );
        assert_eq!(plan.primary, None);
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_monitor_query_reports_enabled_monitors_and_valid_primary() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let query = query_monitor_info(&x11);
        assert!(!query.monitors.is_empty());
        assert!(query.monitors.iter().any(|monitor| monitor.enabled));
        if let Some(primary) = query.primary_output {
            assert!(
                query
                    .monitors
                    .iter()
                    .any(|monitor| monitor.output == Some(primary))
            );
        }
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_monitor_root_has_upstream_geometry_class_name_and_type() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let rectangle = Rectangle::new(7, 9, 101, 103);
        let id = create_monitor_root(&x11, "test-monitor", rectangle, true)
            .expect("create monitor root");
        let root = x::Window::new(id);

        let geometry_cookie = x11.send(&x::GetGeometry {
            drawable: x::Drawable::Window(root),
        });
        let attributes_cookie = x11.send(&x::GetWindowAttributes { window: root });
        let class_cookie = x11.send(&x::GetProperty {
            delete: false,
            window: root,
            property: x11.atoms().wm_class,
            r#type: x::ATOM_STRING,
            long_offset: 0,
            long_length: u32::MAX,
        });
        let name_cookie = x11.send(&x::GetProperty {
            delete: false,
            window: root,
            property: x11.atoms().wm_name,
            r#type: x::ATOM_STRING,
            long_offset: 0,
            long_length: u32::MAX,
        });
        let type_cookie = x11.send(&x::GetProperty {
            delete: false,
            window: root,
            property: x11.atoms().net_wm_window_type,
            r#type: x::ATOM_ATOM,
            long_offset: 0,
            long_length: 1,
        });

        let geometry = x11.connection().wait_for_reply(geometry_cookie).unwrap();
        assert_eq!(
            (
                geometry.x(),
                geometry.y(),
                geometry.width(),
                geometry.height()
            ),
            (7, 9, 101, 103)
        );
        let attributes = x11.connection().wait_for_reply(attributes_cookie).unwrap();
        assert_eq!(attributes.class(), x::WindowClass::InputOnly);
        assert_eq!(attributes.map_state(), x::MapState::Viewable);
        assert_eq!(
            x11.connection()
                .wait_for_reply(class_cookie)
                .unwrap()
                .value::<u8>(),
            b"root\0Bspwm\0"
        );
        assert_eq!(
            x11.connection()
                .wait_for_reply(name_cookie)
                .unwrap()
                .value::<u8>(),
            b"test-monitor"
        );
        assert_eq!(
            x11.connection()
                .wait_for_reply(type_cookie)
                .unwrap()
                .value::<u32>(),
            &[x11.atoms().net_wm_window_type_desktop.resource_id()]
        );

        destroy_monitor_root(&x11, id).expect("destroy monitor root");
    }
}
