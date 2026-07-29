#![allow(clippy::cast_sign_loss)]

use crate::query::Coordinates;
use crate::settings::Settings;
use crate::tree::NodeId;
use crate::types::{ClientState, MAXLEN, StateTransitions, WmFlags};
use crate::window::set_property;
use crate::world::{DesktopId, World};
use crate::x11::{Atoms, ScreenGeometry, X11};
pub use bspwm_core::strut::{StrutPartial, apply_strut_partial, parse_strut, parse_strut_partial};
use xcb::{Xid, XidNew, x};

/// Returns the atoms advertised by upstream bspwm, in canonical order.
#[must_use]
pub fn supported_atoms(atoms: &Atoms, allowed_actions: bool) -> Vec<x::Atom> {
    let mut result = vec![
        atoms.net_supported,
        atoms.net_supporting_wm_check,
        atoms.net_desktop_names,
        atoms.net_desktop_geometry,
        atoms.net_desktop_viewport,
        atoms.net_workarea,
        atoms.net_number_of_desktops,
        atoms.net_current_desktop,
        atoms.net_client_list,
        atoms.net_active_window,
        atoms.net_close_window,
        atoms.net_restack_window,
        atoms.net_moveresize_window,
        atoms.net_wm_moveresize,
        atoms.net_request_frame_extents,
        atoms.net_frame_extents,
        atoms.net_wm_sync_request,
        atoms.net_wm_sync_request_counter,
        atoms.net_wm_strut_partial,
        atoms.net_wm_strut,
        atoms.net_wm_desktop,
        atoms.net_wm_user_time,
        atoms.net_wm_user_time_window,
        atoms.net_wm_state,
        atoms.net_wm_state_hidden,
        atoms.net_wm_state_fullscreen,
        atoms.net_wm_state_below,
        atoms.net_wm_state_above,
        atoms.net_wm_state_sticky,
        atoms.net_wm_state_demands_attention,
        atoms.net_wm_state_focused,
        atoms.net_wm_window_type,
        atoms.net_wm_window_type_dock,
        atoms.net_wm_window_type_desktop,
        atoms.net_wm_window_type_notification,
        atoms.net_wm_window_type_dialog,
        atoms.net_wm_window_type_utility,
        atoms.net_wm_window_type_toolbar,
        atoms.net_wm_ping,
        atoms.net_desktop_layout,
    ];
    if allowed_actions {
        result.extend([
            atoms.net_wm_allowed_actions,
            atoms.net_wm_action_move,
            atoms.net_wm_action_resize,
            atoms.net_wm_action_minimize,
            atoms.net_wm_action_stick,
            atoms.net_wm_action_fullscreen,
            atoms.net_wm_action_change_desktop,
            atoms.net_wm_action_close,
            atoms.net_wm_action_above,
            atoms.net_wm_action_below,
        ]);
    }
    result
}

/// Writes `_NET_SUPPORTED` on the root window.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn set_supported(x11: &X11, allowed_actions: bool) -> xcb::ProtocolResult<()> {
    let values = supported_atoms(x11.atoms(), allowed_actions);
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_supported,
        x::ATOM_ATOM,
        &values,
    )
}

/// Publishes the supporting WM check, UTF-8 name, and process ID.
///
/// # Errors
/// Returns an X protocol error if any checked property request fails.
pub fn set_supporting(x11: &X11, window: x::Window, wm_name: &str) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_supporting_wm_check,
        x::ATOM_WINDOW,
        &[window],
    )?;
    set_property(
        x11,
        window,
        x11.atoms().net_supporting_wm_check,
        x::ATOM_WINDOW,
        &[window],
    )?;
    set_property(
        x11,
        window,
        x11.atoms().net_wm_name,
        x11.atoms().utf8_string,
        wm_name.as_bytes(),
    )?;
    set_property(
        x11,
        window,
        x11.atoms().net_wm_pid,
        x::ATOM_CARDINAL,
        &[std::process::id()],
    )
}

/// Counts desktops in upstream monitor/desktop order.
#[must_use]
pub fn number_of_desktops(world: &World) -> u32 {
    world.monitor_order().iter().fold(0_u32, |count, monitor| {
        count
            .wrapping_add(u32::try_from(world.monitor(*monitor).desktops.len()).unwrap_or(u32::MAX))
    })
}

/// Returns a desktop's global EWMH index, or zero as upstream does when absent.
#[must_use]
pub fn desktop_index(world: &World, desktop: DesktopId) -> u32 {
    let mut index = 0_u32;
    for (_, candidate) in world.desktops() {
        if candidate == desktop {
            return index;
        }
        index = index.wrapping_add(1);
    }
    0
}

/// Locates a global EWMH desktop index in monitor/desktop order.
#[must_use]
pub fn locate_desktop(world: &World, mut index: u32) -> Option<Coordinates> {
    for (monitor, desktop) in world.desktops() {
        if index == 0 {
            return Some(Coordinates::desktop(monitor, desktop));
        }
        index = index.wrapping_sub(1);
    }
    None
}

/// Writes `_NET_DESKTOP_LAYOUT` on the root window.
///
/// Uses a single horizontal row with one column per desktop and the top-left
/// starting corner, which matches a linear desktop list.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_desktop_layout(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    let count = number_of_desktops(world);
    // Orientation=Horizontal(0), columns=count, rows=1, starting_corner=TopLeft(0)
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_desktop_layout,
        x::ATOM_CARDINAL,
        &[0_u32, count, 1, 0],
    )
}

/// Writes `_NET_NUMBER_OF_DESKTOPS` on the root window.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_number_of_desktops(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_number_of_desktops,
        x::ATOM_CARDINAL,
        &[number_of_desktops(world)],
    )
}

/// Writes the focused monitor's active desktop index, if there is one.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_current_desktop(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    let Some(monitor) = world.focused_monitor else {
        return Ok(());
    };
    let Some(desktop) = world.monitor(monitor).active_desktop else {
        return Ok(());
    };
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_current_desktop,
        x::ATOM_CARDINAL,
        &[desktop_index(world, desktop)],
    )
}

/// Builds the `_NET_DESKTOP_NAMES` byte payload without a final NUL byte.
#[must_use]
pub fn desktop_names_payload(world: &World) -> Vec<u8> {
    let mut names = Vec::with_capacity(MAXLEN);
    for (_, desktop) in world.desktops() {
        let remaining = MAXLEN.saturating_sub(names.len());
        names.extend(
            world
                .desktop(desktop)
                .name
                .as_bytes()
                .iter()
                .copied()
                .take(remaining),
        );
        if names.len() < MAXLEN {
            names.push(0);
        }
    }
    names.pop();
    names
}

/// Writes `_NET_DESKTOP_NAMES` as `UTF8_STRING`.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_desktop_names(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    let names = desktop_names_payload(world);
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_desktop_names,
        x11.atoms().utf8_string,
        &names,
    )
}

/// Builds `_NET_DESKTOP_GEOMETRY` for a non-scrolling desktop model.
#[must_use]
pub const fn desktop_geometry_payload(screen: ScreenGeometry) -> [u32; 2] {
    [screen.width as u32, screen.height as u32]
}

/// Writes the common desktop geometry from the live root dimensions.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_desktop_geometry(x11: &X11) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_desktop_geometry,
        x::ATOM_CARDINAL,
        &desktop_geometry_payload(x11.geometry()),
    )
}

/// Builds flat x/y coordinate pairs for `_NET_DESKTOP_VIEWPORT`.
#[must_use]
#[allow(clippy::cast_sign_loss)]
pub fn desktop_viewports_payload(world: &World) -> Vec<u32> {
    world
        .desktops()
        .flat_map(|(monitor, _)| {
            let rectangle = world.monitor(monitor).rectangle;
            [rectangle.x as u32, rectangle.y as u32]
        })
        .collect()
}

/// Writes `_NET_DESKTOP_VIEWPORT` as x/y CARDINAL pairs.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_desktop_viewports(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    let coordinates = desktop_viewports_payload(world);
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_desktop_viewport,
        x::ATOM_CARDINAL,
        &coordinates,
    )
}

/// Builds viewport-relative usable rectangles for `_NET_WORKAREA`.
#[must_use]
pub fn workareas_payload(world: &World) -> Vec<u32> {
    world
        .desktops()
        .flat_map(|(monitor_id, desktop_id)| {
            let monitor = world.monitor(monitor_id);
            let desktop = world.desktop(desktop_id);
            let left = monitor
                .padding
                .left
                .saturating_add(desktop.padding.left)
                .max(0);
            let right = monitor
                .padding
                .right
                .saturating_add(desktop.padding.right)
                .max(0);
            let top = monitor
                .padding
                .top
                .saturating_add(desktop.padding.top)
                .max(0);
            let bottom = monitor
                .padding
                .bottom
                .saturating_add(desktop.padding.bottom)
                .max(0);
            [
                left as u32,
                top as u32,
                monitor
                    .rectangle
                    .width
                    .saturating_sub(left)
                    .saturating_sub(right)
                    .max(0) as u32,
                monitor
                    .rectangle
                    .height
                    .saturating_sub(top)
                    .saturating_sub(bottom)
                    .max(0) as u32,
            ]
        })
        .collect()
}

/// Writes one usable rectangle per desktop.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_workareas(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_workarea,
        x::ATOM_CARDINAL,
        &workareas_payload(world),
    )
}

/// Returns the active client window, or X11 `None` when focus is not a client.
#[must_use]
pub fn active_window(world: &World) -> x::Window {
    let window = world
        .focused_monitor
        .and_then(|monitor| world.monitor(monitor).active_desktop)
        .and_then(|desktop| world.desktop(desktop).tree.focus)
        .filter(|node| world.tree.node(*node).client.is_some())
        .map_or(0, |node| world.tree.node(node).external_id);
    x::Window::new(window)
}

/// Writes `_NET_ACTIVE_WINDOW` on the root window.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_active_window(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_active_window,
        x::ATOM_WINDOW,
        &[active_window(world)],
    )
}

/// Writes `_NET_WM_DESKTOP` for one client window.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn set_client_desktop(x11: &X11, window: x::Window, index: u32) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        window,
        x11.atoms().net_wm_desktop,
        x::ATOM_CARDINAL,
        &[index],
    )
}

/// Publishes the symmetric native X border as EWMH frame extents.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn set_frame_extents(
    x11: &X11,
    window: x::Window,
    border_width: u32,
) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        window,
        x11.atoms().net_frame_extents,
        x::ATOM_CARDINAL,
        &[border_width; 4],
    )
}

/// Writes one desktop index to every client leaf in a subtree.
///
/// # Errors
/// Returns the first X protocol error from a checked property request.
pub fn set_subtree_desktop(
    x11: &X11,
    world: &World,
    root: NodeId,
    desktop: DesktopId,
) -> xcb::ProtocolResult<()> {
    let index = desktop_index(world, desktop);
    for node in client_leaves(world, root) {
        set_client_desktop(
            x11,
            x::Window::new(world.tree.node(node).external_id),
            index,
        )?;
    }
    Ok(())
}

/// Rewrites `_NET_WM_DESKTOP` for all managed client windows.
///
/// # Errors
/// Returns the first X protocol error from a checked property request.
pub fn update_client_desktops(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    for (_, desktop, root) in world.roots() {
        set_subtree_desktop(x11, world, root, desktop)?;
    }
    Ok(())
}

/// Builds `_NET_CLIENT_LIST` in monitor, desktop, then leaf order.
#[must_use]
pub fn client_list_payload(world: &World) -> Vec<u32> {
    world
        .roots()
        .flat_map(|(_, _, root)| {
            client_leaves(world, root).map(|node| world.tree.node(node).external_id)
        })
        .collect()
}

/// Builds `_NET_CLIENT_LIST_STACKING` from bottom to top.
#[must_use]
pub fn client_stacking_payload(stacking: &stack_mirror::StackMirror) -> Vec<u32> {
    stacking.windows()
}

/// Writes `_NET_CLIENT_LIST` on the root window.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_client_list(x11: &X11, world: &World) -> xcb::ProtocolResult<()> {
    let windows: Vec<_> = client_list_payload(world)
        .into_iter()
        .map(x::Window::new)
        .collect();
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_client_list,
        x::ATOM_WINDOW,
        &windows,
    )
}

/// Writes `_NET_CLIENT_LIST_STACKING` on the root window.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn update_client_stacking_list(
    x11: &X11,
    stacking: &stack_mirror::StackMirror,
) -> xcb::ProtocolResult<()> {
    let windows: Vec<_> = client_stacking_payload(stacking)
        .into_iter()
        .map(x::Window::new)
        .collect();
    set_property(
        x11,
        x11.root(),
        x11.atoms().net_client_list_stacking,
        x::ATOM_WINDOW,
        &windows,
    )
}

/// Replaces a window's `_NET_WM_STATE` atom list.
///
/// # Errors
/// Returns an X protocol error if the checked property request fails.
pub fn set_wm_state(x11: &X11, window: x::Window, states: &[x::Atom]) -> xcb::ProtocolResult<()> {
    set_property(x11, window, x11.atoms().net_wm_state, x::ATOM_ATOM, states)
}

#[must_use]
pub fn allowed_action_atoms(
    world: &World,
    node: NodeId,
    settings: &Settings,
    atoms: &Atoms,
) -> Vec<x::Atom> {
    let value = world.tree.node(node);
    let Some(client) = value.client.as_ref() else {
        return Vec::new();
    };
    let mut actions = vec![
        atoms.net_wm_action_minimize,
        atoms.net_wm_action_stick,
        atoms.net_wm_action_close,
        atoms.net_wm_action_above,
        atoms.net_wm_action_below,
    ];
    if client.state == ClientState::Floating {
        actions.push(atoms.net_wm_action_move);
        actions.push(atoms.net_wm_action_resize);
    }
    let ignored = if client.state == ClientState::Fullscreen {
        StateTransitions::EXIT
    } else {
        StateTransitions::ENTER
    };
    if !settings.ignore_ewmh_fullscreen.contains(ignored) {
        actions.push(atoms.net_wm_action_fullscreen);
    }
    if world.desktops().count() > 1 {
        actions.push(atoms.net_wm_action_change_desktop);
    }
    actions
}

/// Replaces the operations advertised for one managed client.
///
/// # Errors
/// Returns an X protocol error if the property cannot be replaced.
pub fn set_allowed_actions(
    x11: &X11,
    window: x::Window,
    actions: &[x::Atom],
) -> xcb::ProtocolResult<()> {
    set_property(
        x11,
        window,
        x11.atoms().net_wm_allowed_actions,
        x::ATOM_ATOM,
        actions,
    )
}

#[must_use]
pub fn wm_flags_from_ids(states: &[u32], atoms: &Atoms) -> WmFlags {
    wm_flag_atoms(atoms)
        .iter()
        .fold(WmFlags::default(), |flags, (atom, flag)| {
            if states.contains(&atom.resource_id()) {
                flags.union(*flag)
            } else {
                flags
            }
        })
}

#[must_use]
pub fn wm_state_atoms(flags: WmFlags, atoms: &Atoms) -> Vec<x::Atom> {
    wm_flag_atoms(atoms)
        .into_iter()
        .filter_map(|(atom, flag)| flags.contains(flag).then_some(atom))
        .collect()
}

fn wm_flag_atoms(atoms: &Atoms) -> [(x::Atom, WmFlags); 13] {
    [
        (atoms.net_wm_state_modal, WmFlags::MODAL),
        (atoms.net_wm_state_sticky, WmFlags::STICKY),
        (atoms.net_wm_state_maximized_vert, WmFlags::MAXIMIZED_VERT),
        (atoms.net_wm_state_maximized_horz, WmFlags::MAXIMIZED_HORZ),
        (atoms.net_wm_state_shaded, WmFlags::SHADED),
        (atoms.net_wm_state_skip_taskbar, WmFlags::SKIP_TASKBAR),
        (atoms.net_wm_state_skip_pager, WmFlags::SKIP_PAGER),
        (atoms.net_wm_state_hidden, WmFlags::HIDDEN),
        (atoms.net_wm_state_fullscreen, WmFlags::FULLSCREEN),
        (atoms.net_wm_state_above, WmFlags::ABOVE),
        (atoms.net_wm_state_below, WmFlags::BELOW),
        (
            atoms.net_wm_state_demands_attention,
            WmFlags::DEMANDS_ATTENTION,
        ),
        (atoms.net_wm_state_focused, WmFlags::FOCUSED),
    ]
}

/// Reads and validates a 12-CARDINAL `_NET_WM_STRUT_PARTIAL` property.
///
/// # Errors
/// Returns a connection or protocol error if the property cannot be retrieved.
pub fn get_strut_partial(x11: &X11, window: x::Window) -> xcb::Result<Option<StrutPartial>> {
    use crate::window::CardinalProperty;

    match crate::window::get_cardinal_property(x11, window, x11.atoms().net_wm_strut_partial)? {
        CardinalProperty::Values(values) => Ok(parse_strut_partial(&values)),
        CardinalProperty::Invalid => Ok(None),
        CardinalProperty::Absent => {
            let geometry = x11.geometry();
            match crate::window::get_cardinal_property(x11, window, x11.atoms().net_wm_strut)? {
                CardinalProperty::Values(values) => {
                    Ok(parse_strut(&values, geometry.width, geometry.height))
                }
                CardinalProperty::Absent | CardinalProperty::Invalid => Ok(None),
            }
        }
    }
}

/// Returns `true` when the window sets `_NET_WM_OPAQUE_REGION` to an area
/// that does not cover its full geometry, indicating client-side decorations
/// with transparent margins (shadows, resize grips, rounded corners).
#[must_use]
pub fn has_csd(x11: &X11, window: x::Window) -> bool {
    let Ok(geom) = crate::window::geometry(x11, window) else {
        return false;
    };
    let values: Vec<u32> = match crate::window::get_property(
        x11,
        window,
        x11.atoms().net_wm_opaque_region,
        x::ATOM_CARDINAL,
    ) {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    // _NET_WM_OPAQUE_REGION is a list of rectangles: [x, y, width, height, ...]
    if values.len() < 4 || !values.len().is_multiple_of(4) {
        return false;
    }
    // Check if the first rectangle covers less than the full window.
    let ox = values[0];
    let oy = values[1];
    let ow = values[2];
    let oh = values[3];
    let win_w = u32::try_from(geom.rectangle.width).unwrap_or(0);
    let win_h = u32::try_from(geom.rectangle.height).unwrap_or(0);
    ox > 0 || oy > 0 || ow < win_w || oh < win_h
}

fn client_leaves(world: &World, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    world
        .tree
        .leaves(root)
        .filter(move |node| world.tree.node(*node).client.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::tree::Client;
    use crate::types::{Padding, Rectangle};
    use crate::world::MonitorId;
    use xcb::Xid;

    fn sample_world() -> (World, [MonitorId; 2], [DesktopId; 3], [NodeId; 2]) {
        let settings = Settings::default();
        let mut world = World::default();
        let right = world.create_monitor(
            1,
            Some("right"),
            Rectangle::new(100, 20, 100, 80),
            &settings,
        );
        let left = world.create_monitor(
            2,
            Some("left"),
            Rectangle::new(-100, 0, 200, 100),
            &settings,
        );
        let one = world.create_desktop(11, Some("I"), &settings);
        let two = world.create_desktop(12, Some("web"), &settings);
        let three = world.create_desktop(13, Some("III"), &settings);
        assert!(world.add_desktop(left, one));
        assert!(world.add_desktop(left, two));
        assert!(world.add_desktop(right, three));

        let first = world.tree.add_node(0x100, 0.5);
        let second = world.tree.add_node(0x200, 0.5);
        world.tree.node_mut(first).client = Some(Client::from_settings(&settings));
        world.tree.node_mut(second).client = Some(Client::from_settings(&settings));
        world.desktop_mut(one).tree.root = Some(first);
        world.desktop_mut(one).tree.focus = Some(first);
        world.desktop_mut(three).tree.root = Some(second);
        world.desktop_mut(three).tree.focus = Some(second);
        (world, [left, right], [one, two, three], [first, second])
    }

    #[test]
    fn desktop_payloads_follow_monitor_then_desktop_order() {
        let (world, [left, right], [one, two, three], _) = sample_world();
        assert_eq!(world.monitor_order(), &[left, right]);
        assert_eq!(number_of_desktops(&world), 3);
        assert_eq!(desktop_index(&world, one), 0);
        assert_eq!(desktop_index(&world, two), 1);
        assert_eq!(desktop_index(&world, three), 2);
        assert_eq!(locate_desktop(&world, 2).unwrap().desktop, Some(three));
        assert_eq!(locate_desktop(&world, 3), None);
        assert_eq!(desktop_names_payload(&world), b"I\0web\0III");
        assert_eq!(
            desktop_viewports_payload(&world),
            [(-100_i32) as u32, 0, (-100_i32) as u32, 0, 100, 20]
        );
    }

    #[test]
    fn desktop_geometry_and_workareas_use_screen_size_padding_and_desktop_order() {
        assert_eq!(
            desktop_geometry_payload(ScreenGeometry::root(5120, 1440)),
            [5120, 1440]
        );
        let (mut world, [left, _], [one, _, _], _) = sample_world();
        world.monitor_mut(left).padding = Padding {
            top: 5,
            right: 0,
            bottom: 0,
            left: 10,
        };
        world.desktop_mut(one).padding = Padding {
            top: 0,
            right: 20,
            bottom: 10,
            left: 0,
        };
        assert_eq!(
            workareas_payload(&world),
            [10, 5, 170, 85, 10, 5, 190, 95, 0, 0, 100, 80]
        );
    }

    #[test]
    fn absent_desktop_index_and_empty_names_match_upstream_fallbacks() {
        let settings = Settings::default();
        let mut world = World::default();
        let detached = world.create_desktop(1, Some("detached"), &settings);
        assert_eq!(desktop_index(&world, detached), 0);
        assert!(desktop_names_payload(&world).is_empty());
        assert!(desktop_viewports_payload(&world).is_empty());
        assert!(workareas_payload(&world).is_empty());
    }

    #[test]
    fn client_payload_uses_leaf_order_and_active_window_requires_a_client() {
        struct Noop;
        impl stack_mirror::StackBackend for Noop {
            type Error = ();
            fn stack_above(&mut self, _: u32, _: u32) -> Result<(), ()> {
                Ok(())
            }
            fn stack_below(&mut self, _: u32, _: u32) -> Result<(), ()> {
                Ok(())
            }
        }

        let (mut world, [left, _], [one, _, _], [first, second]) = sample_world();
        assert_eq!(client_list_payload(&world), [0x100, 0x200]);
        let mut stacking = stack_mirror::StackMirror::new();
        let _ = stacking.insert(&mut Noop, 0x200, 3);
        let _ = stacking.insert(&mut Noop, 0x100, 3);
        assert_eq!(client_stacking_payload(&stacking), [0x200, 0x100]);
        world.focused_monitor = Some(left);
        world.monitor_mut(left).active_desktop = Some(one);
        world.desktop_mut(one).tree.focus = Some(first);
        assert_eq!(active_window(&world).resource_id(), 0x100);
        world.desktop_mut(one).tree.focus = None;
        assert_eq!(active_window(&world).resource_id(), 0);
        assert!(world.tree.node(second).client.is_some());
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn writes_root_properties_on_live_display() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let world = World::default();
        set_supported(&x11, false).expect("set _NET_SUPPORTED");
        update_number_of_desktops(&x11, &world).expect("set desktop count");
        x11.flush().expect("flush requests");
    }
}
