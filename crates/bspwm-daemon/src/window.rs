#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::similar_names
)]

use xcb::{Xid, shape, x};

use crate::rule::{BuiltinRuleProperties, BuiltinWindowState, BuiltinWindowType, WindowProperties};
use crate::tree::{IcccmProps, SizeHints};
use crate::types::{Rectangle, WmFlags, wrapping_i16};
use crate::x11::X11;

pub use crate::geometry::{adapt_geometry, center, embrace};

/// Geometry returned by the X server for a window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowGeometry {
    pub rectangle: Rectangle,
    pub border_width: u16,
    pub depth: u8,
    pub root: x::Window,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleWindowProperties {
    pub override_redirect: bool,
    pub identity: WindowProperties,
    pub builtin: BuiltinRuleProperties,
    pub size_hints: SizeHints,
    pub geometry: WindowGeometry,
    pub icccm: IcccmProps,
    pub urgent: bool,
    pub wm_flags: WmFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WmHints {
    pub input: Option<bool>,
    pub urgent: bool,
}

/// The mechanism used to ask a client to close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseMethod {
    WmDeleteWindow,
    KillClient,
}

/// Returns whether `window` is still a valid X resource.
///
/// Like upstream's `window_exists`, this deliberately turns all query errors into
/// `false`; callers interested in the error should use [`geometry`].
#[must_use]
pub fn exists(x11: &X11, window: x::Window) -> bool {
    x11.request(&x::QueryTree { window }).is_ok()
}

/// Queries a window's current server-side geometry.
pub fn geometry(x11: &X11, window: x::Window) -> xcb::Result<WindowGeometry> {
    let reply = x11.request(&x::GetGeometry {
        drawable: x::Drawable::Window(window),
    })?;
    Ok(geometry_from_reply(&reply))
}

fn geometry_from_reply(reply: &x::GetGeometryReply) -> WindowGeometry {
    WindowGeometry {
        rectangle: Rectangle::from_x11(reply.x(), reply.y(), reply.width(), reply.height()),
        border_width: reply.border_width(),
        depth: reply.depth(),
        root: reply.root(),
    }
}

/// Collects the synchronous property set consumed by bspwm's built-in rules.
///
/// Every request is queued before any reply is awaited: none of them depends on
/// another's reply, so the whole set costs a single round trip instead of one
/// per property.  Replies are awaited in exactly the order the requests were
/// issued, so a client that dies mid-sequence still surfaces the same error the
/// serial version returned.
pub fn rule_properties(x11: &X11, window: x::Window) -> xcb::Result<RuleWindowProperties> {
    let atoms = x11.atoms();
    let attributes_cookie = x11.send(&x::GetWindowAttributes { window });
    let class_cookie = send_get_property(x11, window, atoms.wm_class, x::ATOM_STRING);
    let net_name_cookie = send_get_property(x11, window, atoms.net_wm_name, atoms.utf8_string);
    let wm_name_cookie = send_get_property(x11, window, atoms.wm_name, x::ATOM_ANY);
    let window_type_cookie = send_get_property(x11, window, atoms.net_wm_window_type, x::ATOM_ATOM);
    let window_state_cookie = send_get_property(x11, window, atoms.net_wm_state, x::ATOM_ATOM);
    let protocols_cookie = send_get_property(x11, window, atoms.wm_protocols, x::ATOM_ATOM);
    let wm_hints_cookie = send_get_property(x11, window, x::ATOM_WM_HINTS, x::ATOM_WM_HINTS);
    let transient_cookie = send_get_property(x11, window, atoms.wm_transient_for, x::ATOM_WINDOW);
    let normal_hints_cookie =
        send_get_property(x11, window, atoms.wm_normal_hints, atoms.wm_size_hints);
    let geometry_cookie = x11.send(&x::GetGeometry {
        drawable: x::Drawable::Window(window),
    });

    // Every reply is collected before the first failure is propagated, so no
    // cookie is abandoned and the error returned is still the earliest one.
    let connection = x11.connection();
    let attributes = connection.wait_for_reply(attributes_cookie);
    let class = wait_property::<u8>(connection, class_cookie, x::ATOM_STRING);
    let net_name = wait_property::<u8>(connection, net_name_cookie, atoms.utf8_string);
    let wm_name = wait_property::<u8>(connection, wm_name_cookie, x::ATOM_ANY);
    let window_types = wait_property::<u32>(connection, window_type_cookie, x::ATOM_ATOM);
    let window_states = wait_property::<u32>(connection, window_state_cookie, x::ATOM_ATOM);
    let protocols = wait_property::<u32>(connection, protocols_cookie, x::ATOM_ATOM);
    let wm_hint_values = wait_property::<u32>(connection, wm_hints_cookie, x::ATOM_WM_HINTS);
    let transient_values = wait_property::<u32>(connection, transient_cookie, x::ATOM_WINDOW);
    let hints = wait_property::<u32>(connection, normal_hints_cookie, atoms.wm_size_hints);
    let geometry = connection
        .wait_for_reply(geometry_cookie)
        .map(|reply| geometry_from_reply(&reply));

    let attributes = attributes?;
    let class = class?;
    let mut class_fields = class.split(|byte| *byte == 0);
    let instance_name = lossy(class_fields.next().unwrap_or_default());
    let class_name = lossy(class_fields.next().unwrap_or_default());
    let net_name = net_name?;
    let wm_name = wm_name?;
    let name = if net_name.is_empty() {
        lossy(trim_nul(&wm_name))
    } else {
        lossy(trim_nul(&net_name))
    };
    let window_types = window_types?;
    let window_states = window_states?;
    let protocols = protocols?;
    let wm_hints = parse_wm_hints(&wm_hint_values?);
    let wm_flags = crate::ewmh::wm_flags_from_ids(&window_states, atoms);
    let transient = transient_values?
        .first()
        .is_some_and(|transient| *transient != x::WINDOW_NONE.resource_id());
    let size_hints = parse_size_hints(&hints?);
    let geometry = geometry?;

    Ok(RuleWindowProperties {
        override_redirect: attributes.override_redirect(),
        identity: WindowProperties::new(class_name, instance_name, name),
        builtin: BuiltinRuleProperties {
            window_types: builtin_window_types(&window_types, atoms),
            window_states: builtin_window_states(&window_states, atoms),
            transient,
            fixed_size: size_hints.is_fixed(),
        },
        size_hints,
        geometry,
        icccm: IcccmProps {
            input_hint: wm_hints.input.unwrap_or(true),
            take_focus: protocols.contains(&atoms.wm_take_focus.resource_id()),
            delete_window: protocols.contains(&atoms.wm_delete_window.resource_id()),
        },
        urgent: wm_hints.urgent || wm_flags.contains(WmFlags::DEMANDS_ATTENTION),
        wm_flags,
    })
}

/// Maps `_NET_WM_WINDOW_TYPE` atoms onto the subset bspwm's rules understand.
fn builtin_window_types(atom_ids: &[u32], atoms: &crate::x11::Atoms) -> Vec<BuiltinWindowType> {
    atom_ids
        .iter()
        .filter_map(|atom| match *atom {
            atom if atom == atoms.net_wm_window_type_toolbar.resource_id() => {
                Some(BuiltinWindowType::Toolbar)
            }
            atom if atom == atoms.net_wm_window_type_utility.resource_id() => {
                Some(BuiltinWindowType::Utility)
            }
            atom if atom == atoms.net_wm_window_type_dialog.resource_id() => {
                Some(BuiltinWindowType::Dialog)
            }
            atom if atom == atoms.net_wm_window_type_dock.resource_id() => {
                Some(BuiltinWindowType::Dock)
            }
            atom if atom == atoms.net_wm_window_type_desktop.resource_id() => {
                Some(BuiltinWindowType::Desktop)
            }
            atom if atom == atoms.net_wm_window_type_notification.resource_id() => {
                Some(BuiltinWindowType::Notification)
            }
            _ => None,
        })
        .collect()
}

/// Maps `_NET_WM_STATE` atoms onto the subset bspwm's rules understand.
fn builtin_window_states(atom_ids: &[u32], atoms: &crate::x11::Atoms) -> Vec<BuiltinWindowState> {
    atom_ids
        .iter()
        .filter_map(|atom| match *atom {
            atom if atom == atoms.net_wm_state_fullscreen.resource_id() => {
                Some(BuiltinWindowState::Fullscreen)
            }
            atom if atom == atoms.net_wm_state_below.resource_id() => {
                Some(BuiltinWindowState::Below)
            }
            atom if atom == atoms.net_wm_state_above.resource_id() => {
                Some(BuiltinWindowState::Above)
            }
            atom if atom == atoms.net_wm_state_sticky.resource_id() => {
                Some(BuiltinWindowState::Sticky)
            }
            _ => None,
        })
        .collect()
}

/// Reads a whole property, or an empty vector when it is absent or mistyped.
///
/// A type mismatch is only rejected when the caller asked for a concrete type:
/// `x::ATOM_ANY` is `AnyPropertyType`, for which the server always reports the
/// property's actual type.
pub fn get_property<P: x::PropEl + Clone>(
    x11: &X11,
    window: x::Window,
    property: x::Atom,
    property_type: x::Atom,
) -> xcb::Result<Vec<P>> {
    let cookie = send_get_property(x11, window, property, property_type);
    wait_property(x11.connection(), cookie, property_type)
}

/// Queues a whole-property read without awaiting its reply, so callers can pipeline.
fn send_get_property(
    x11: &X11,
    window: x::Window,
    property: x::Atom,
    property_type: x::Atom,
) -> x::GetPropertyCookie {
    x11.send(&x::GetProperty {
        delete: false,
        window,
        property,
        r#type: property_type,
        long_offset: 0,
        long_length: u32::MAX,
    })
}

/// Awaits a queued property read, applying [`get_property`]'s type filtering.
fn wait_property<P: x::PropEl + Clone>(
    connection: &xcb::Connection,
    cookie: x::GetPropertyCookie,
    property_type: x::Atom,
) -> xcb::Result<Vec<P>> {
    let reply = connection.wait_for_reply(cookie)?;
    if reply.format() != P::FORMAT
        || (property_type != x::ATOM_ANY && reply.r#type() != property_type)
    {
        return Ok(Vec::new());
    }
    Ok(reply.value::<P>().to_vec())
}

#[must_use]
pub fn parse_size_hints(values: &[u32]) -> SizeHints {
    let signed = |index: usize| values.get(index).copied().unwrap_or_default().cast_signed();
    SizeHints {
        flags: values.first().copied().unwrap_or_default(),
        min_width: signed(5),
        min_height: signed(6),
        max_width: signed(7),
        max_height: signed(8),
        width_inc: signed(9),
        height_inc: signed(10),
        min_aspect_num: signed(11),
        min_aspect_den: signed(12),
        max_aspect_num: signed(13),
        max_aspect_den: signed(14),
        base_width: signed(15),
        base_height: signed(16),
    }
}

pub fn normal_hints(x11: &X11, window: x::Window) -> xcb::Result<SizeHints> {
    get_property::<u32>(
        x11,
        window,
        x11.atoms().wm_normal_hints,
        x11.atoms().wm_size_hints,
    )
    .map(|values| parse_size_hints(&values))
}

pub fn wm_hints(x11: &X11, window: x::Window) -> xcb::Result<WmHints> {
    let values = get_property::<u32>(x11, window, x::ATOM_WM_HINTS, x::ATOM_WM_HINTS)?;
    Ok(parse_wm_hints(&values))
}

#[must_use]
pub fn parse_wm_hints(values: &[u32]) -> WmHints {
    const INPUT_HINT: u32 = 1;
    const URGENCY_HINT: u32 = 1 << 8;
    let flags = values.first().copied().unwrap_or_default();
    WmHints {
        input: (flags & INPUT_HINT != 0).then(|| values.get(1).copied().unwrap_or_default() != 0),
        urgent: flags & URGENCY_HINT != 0,
    }
}

fn trim_nul(value: &[u8]) -> &[u8] {
    value.split(|byte| *byte == 0).next().unwrap_or_default()
}

fn lossy(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

/// Applies an ordered, non-duplicated X ConfigureWindow value list.
pub fn configure(
    x11: &X11,
    window: x::Window,
    value_list: &[x::ConfigWindow],
) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::ConfigureWindow { window, value_list })
}

pub fn move_window(
    x11: &X11,
    window: x::Window,
    x_position: i16,
    y_position: i16,
) -> xcb::ProtocolResult<()> {
    configure(
        x11,
        window,
        &[
            x::ConfigWindow::X(i32::from(x_position)),
            x::ConfigWindow::Y(i32::from(y_position)),
        ],
    )
}

pub fn resize(x11: &X11, window: x::Window, width: u16, height: u16) -> xcb::ProtocolResult<()> {
    configure(
        x11,
        window,
        &[
            x::ConfigWindow::Width(u32::from(width)),
            x::ConfigWindow::Height(u32::from(height)),
        ],
    )
}

pub fn move_resize(x11: &X11, window: x::Window, rectangle: Rectangle) -> xcb::ProtocolResult<()> {
    configure(
        x11,
        window,
        &[
            x::ConfigWindow::X(rectangle.x),
            x::ConfigWindow::Y(rectangle.y),
            x::ConfigWindow::Width(u32::try_from(rectangle.width).unwrap_or(0)),
            x::ConfigWindow::Height(u32::try_from(rectangle.height).unwrap_or(0)),
        ],
    )
}

pub fn map(x11: &X11, window: x::Window) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::MapWindow { window })
}

pub fn unmap(x11: &X11, window: x::Window) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::UnmapWindow { window })
}

/// Maps or unmaps without letting the WM consume its own `UnmapNotify`.
pub fn set_visibility(x11: &X11, window: x::Window, visible: bool) -> xcb::ProtocolResult<()> {
    const ROOT_MASK: x::EventMask = x::EventMask::SUBSTRUCTURE_REDIRECT
        .union(x::EventMask::SUBSTRUCTURE_NOTIFY)
        .union(x::EventMask::STRUCTURE_NOTIFY)
        .union(x::EventMask::BUTTON_PRESS)
        .union(x::EventMask::FOCUS_CHANGE);
    let quiet_mask = ROOT_MASK.difference(x::EventMask::SUBSTRUCTURE_NOTIFY);
    x11.send_and_check_request(&x::ChangeWindowAttributes {
        window: x11.root(),
        value_list: &[x::Cw::EventMask(quiet_mask)],
    })?;
    let wm_state = |value: u32| {
        set_property(
            x11,
            window,
            x11.atoms().wm_state,
            x11.atoms().wm_state,
            &[value, 0],
        )
    };
    let request = if visible {
        wm_state(1).and_then(|()| map(x11, window))
    } else {
        unmap(x11, window).and_then(|()| wm_state(3))
    };
    let restore = x11.send_and_check_request(&x::ChangeWindowAttributes {
        window: x11.root(),
        value_list: &[x::Cw::EventMask(ROOT_MASK)],
    });
    request.and(restore)
}

pub fn set_border_width(x11: &X11, window: x::Window, width: u32) -> xcb::ProtocolResult<()> {
    configure(x11, window, &[x::ConfigWindow::BorderWidth(width)])
}

pub fn set_border_color(x11: &X11, window: x::Window, pixel: u32) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::ChangeWindowAttributes {
        window,
        value_list: &[x::Cw::BorderPixel(pixel)],
    })
}

/// Creates the override-free, input-transparent window used for preselection feedback.
pub fn create_presel_feedback(x11: &X11, window: x::Window, color: u32) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::CreateWindow {
        depth: x::COPY_FROM_PARENT as u8,
        wid: window,
        parent: x11.root(),
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        border_width: 0,
        class: x::WindowClass::InputOutput,
        visual: x::COPY_FROM_PARENT,
        value_list: &[x::Cw::BackPixel(color), x::Cw::SaveUnder(true)],
    })?;
    set_property(
        x11,
        window,
        x11.atoms().wm_class,
        x::ATOM_STRING,
        b"presel_feedback\0Bspwm\0",
    )?;
    x11.send_and_check_request(&shape::Rectangles {
        operation: shape::So::Set,
        destination_kind: shape::Sk::Input,
        ordering: x::ClipOrdering::Unsorted,
        destination_window: window,
        x_offset: 0,
        y_offset: 0,
        rectangles: &[],
    })
}

pub fn destroy(x11: &X11, window: x::Window) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::DestroyWindow { window })
}

pub fn set_background_color(x11: &X11, window: x::Window, pixel: u32) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::ChangeWindowAttributes {
        window,
        value_list: &[x::Cw::BackPixel(pixel)],
    })
}

fn stack(
    x11: &X11,
    window: x::Window,
    sibling: x::Window,
    mode: x::StackMode,
) -> xcb::ProtocolResult<()> {
    if sibling.is_none() {
        return Ok(());
    }
    configure(
        x11,
        window,
        &[
            x::ConfigWindow::Sibling(sibling),
            x::ConfigWindow::StackMode(mode),
        ],
    )
}

pub fn stack_above(x11: &X11, window: x::Window, sibling: x::Window) -> xcb::ProtocolResult<()> {
    stack(x11, window, sibling, x::StackMode::Above)
}

pub fn stack_below(x11: &X11, window: x::Window, sibling: x::Window) -> xcb::ProtocolResult<()> {
    stack(x11, window, sibling, x::StackMode::Below)
}

pub fn lower(x11: &X11, window: x::Window) -> xcb::ProtocolResult<()> {
    configure(
        x11,
        window,
        &[x::ConfigWindow::StackMode(x::StackMode::Below)],
    )
}

pub fn focus(x11: &X11, window: x::Window) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::SetInputFocus {
        revert_to: x::InputFocus::Parent,
        focus: window,
        time: x::CURRENT_TIME,
    })
}

pub fn focus_client(
    x11: &X11,
    window: x::Window,
    input_hint: bool,
    take_focus: bool,
) -> xcb::ProtocolResult<()> {
    if input_hint {
        focus(x11, window)
    } else if take_focus {
        send_client_message(
            x11,
            window,
            window,
            x11.atoms().wm_protocols,
            [
                x11.atoms().wm_take_focus.resource_id(),
                x::CURRENT_TIME,
                0,
                0,
                0,
            ],
            x::EventMask::NO_EVENT,
        )
    } else {
        Ok(())
    }
}

pub fn clear_focus(x11: &X11) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::SetInputFocus {
        revert_to: x::InputFocus::PointerRoot,
        focus: x11.root(),
        time: x::CURRENT_TIME,
    })
}

pub fn warp_pointer(x11: &X11, x_position: i32, y_position: i32) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::WarpPointer {
        src_window: x::WINDOW_NONE,
        dst_window: x11.root(),
        src_x: 0,
        src_y: 0,
        src_width: 0,
        src_height: 0,
        dst_x: wrapping_i16(x_position),
        dst_y: wrapping_i16(y_position),
    })
}

pub fn warp_pointer_to_center(x11: &X11, rectangle: Rectangle) -> xcb::ProtocolResult<()> {
    let x_position = rectangle.left() + rectangle.width / 2;
    let y_position = rectangle.top() + rectangle.height / 2;
    warp_pointer(x11, x_position, y_position)
}

/// Replaces `property` on `window` with `data`, typed as `property_type`.
pub fn set_property<P: x::PropEl>(
    x11: &X11,
    window: x::Window,
    property: x::Atom,
    property_type: x::Atom,
    data: &[P],
) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property,
        r#type: property_type,
        data,
    })
}

/// Returns the EWMH desktop index when `_NET_WM_DESKTOP` is present.
pub fn wm_desktop(x11: &X11, window: x::Window) -> xcb::Result<Option<u32>> {
    Ok(
        get_property::<u32>(x11, window, x11.atoms().net_wm_desktop, x::ATOM_CARDINAL)?
            .first()
            .copied(),
    )
}

pub fn send_client_message(
    x11: &X11,
    destination: x::Window,
    window: x::Window,
    message_type: x::Atom,
    data: [u32; 5],
    event_mask: x::EventMask,
) -> xcb::ProtocolResult<()> {
    let event =
        x::ClientMessageEvent::new(window, message_type, x::ClientMessageData::Data32(data));
    x11.send_and_check_request(&x::SendEvent {
        propagate: false,
        destination: x::SendEventDest::Window(destination),
        event_mask,
        event: &event,
    })
}

pub fn close_cached(
    x11: &X11,
    window: x::Window,
    delete_window: bool,
) -> xcb::ProtocolResult<CloseMethod> {
    if delete_window {
        send_client_message(
            x11,
            window,
            window,
            x11.atoms().wm_protocols,
            [
                x11.atoms().wm_delete_window.resource_id(),
                x::CURRENT_TIME,
                0,
                0,
                0,
            ],
            x::EventMask::NO_EVENT,
        )?;
        Ok(CloseMethod::WmDeleteWindow)
    } else {
        x11.send_and_check_request(&x::KillClient {
            resource: window.resource_id(),
        })?;
        Ok(CloseMethod::KillClient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: Rectangle = Rectangle::new(0, 0, 100, 100);

    #[test]
    fn wm_hint_defaults_and_bits_are_icccm_compatible() {
        assert_eq!(
            parse_wm_hints(&[]),
            WmHints {
                input: None,
                urgent: false
            }
        );
        assert_eq!(
            parse_wm_hints(&[1, 0]),
            WmHints {
                input: Some(false),
                urgent: false
            }
        );
        assert_eq!(
            parse_wm_hints(&[1 | (1 << 8), 1]),
            WmHints {
                input: Some(true),
                urgent: true
            }
        );
    }

    #[test]
    fn parses_icccm_normal_hints_and_urgency_bits() {
        let mut values = vec![0_u32; 17];
        values[0] = SizeHints::MIN_SIZE | SizeHints::MAX_SIZE;
        values[5] = 10;
        values[6] = 20;
        values[7] = 30;
        values[8] = 40;
        let hints = parse_size_hints(&values);
        assert_eq!((hints.min_width, hints.min_height), (10, 20));
        assert_eq!((hints.max_width, hints.max_height), (30, 40));
        assert!(!hints.is_fixed());
    }

    #[test]
    fn center_handles_positioning_cases() {
        let cases = [
            (
                "smaller rectangle at midpoint",
                Rectangle::new(900, 900, 20, 40),
                Rectangle::new(10, 20, 100, 80),
                0,
                Rectangle::new(50, 40, 20, 40),
            ),
            (
                "border width after positioning",
                Rectangle::new(0, 0, 20, 20),
                SOURCE,
                3,
                Rectangle::new(37, 37, 20, 20),
            ),
            (
                "oversized dimensions independently anchored",
                Rectangle::new(0, 0, 120, 20),
                Rectangle::new(10, 20, 100, 80),
                2,
                Rectangle::new(8, 48, 120, 20),
            ),
        ];

        for (label, rectangle, area, border_width, expected) in cases {
            assert_eq!(center(rectangle, area, border_width), expected, "{label}");
        }
    }

    #[test]
    fn embrace_handles_positioning_cases() {
        let cases = [
            (
                "wholly left and above",
                Rectangle::new(-30, -40, 20, 30),
                Rectangle::new(0, 0, 20, 30),
            ),
            (
                "wholly right and below",
                Rectangle::new(100, 100, 20, 30),
                Rectangle::new(80, 70, 20, 30),
            ),
            (
                "partial overlap preserved",
                Rectangle::new(-10, 90, 20, 20),
                Rectangle::new(-10, 90, 20, 20),
            ),
            (
                "inside rectangle preserved",
                Rectangle::new(10, 20, 30, 40),
                Rectangle::new(10, 20, 30, 40),
            ),
            (
                "rectangle larger than area",
                Rectangle::new(100, 100, 150, 120),
                Rectangle::new(-50, -20, 150, 120),
            ),
        ];

        for (label, rectangle, expected) in cases {
            assert_eq!(embrace(rectangle, SOURCE), expected, "{label}");
        }
    }

    #[test]
    fn adapt_geometry_handles_monitor_mapping_cases() {
        let cases = [
            (
                "equal monitors preserve geometry",
                Rectangle::new(12, 34, 20, 30),
                SOURCE,
                Rectangle::new(12, 34, 20, 30),
            ),
            (
                "free space scales on each side",
                Rectangle::new(25, 25, 20, 20),
                Rectangle::new(100, 50, 200, 200),
                Rectangle::new(156, 106, 20, 20),
            ),
            (
                "monitor origins translate",
                Rectangle::new(10, 20, 30, 40),
                Rectangle::new(-200, 300, 100, 100),
                Rectangle::new(-190, 320, 30, 40),
            ),
            (
                "negative out-of-bounds region survives clipping",
                Rectangle::new(-10, -20, 50, 60),
                Rectangle::new(200, 200, 200, 200),
                Rectangle::new(190, 180, 50, 60),
            ),
            (
                "positive out-of-bounds region survives clipping",
                Rectangle::new(80, 80, 40, 50),
                Rectangle::new(200, 200, 200, 200),
                Rectangle::new(380, 380, 40, 50),
            ),
            (
                "division by zero when window fills source axis",
                Rectangle::new(0, 25, 100, 20),
                Rectangle::new(50, 60, 200, 200),
                Rectangle::new(50, 116, 100, 20),
            ),
        ];

        for (label, rectangle, destination, expected) in cases {
            assert_eq!(
                adapt_geometry(rectangle, SOURCE, destination),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_root_exists_and_has_geometry() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        assert!(exists(&x11, x11.root()));
        let root = geometry(&x11, x11.root()).expect("query root geometry");
        assert_eq!(root.rectangle.width, i32::from(x11.geometry().width));
        assert_eq!(root.rectangle.height, i32::from(x11.geometry().height));
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_window_requests_round_trip() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let window: x::Window = x11.connection().generate_id();
        x11.send_and_check_request(&x::CreateWindow {
            depth: x::COPY_FROM_PARENT as u8,
            wid: window,
            parent: x11.root(),
            x: 1,
            y: 2,
            width: 10,
            height: 11,
            border_width: 0,
            class: x::WindowClass::InputOutput,
            visual: x::COPY_FROM_PARENT,
            value_list: &[],
        })
        .expect("create test window");

        move_resize(&x11, window, Rectangle::new(7, 8, 31, 32)).expect("configure window");
        set_border_width(&x11, window, 2).expect("set border width");
        set_border_color(&x11, window, 0).expect("set border color");
        map(&x11, window).expect("map window");
        unmap(&x11, window).expect("unmap window");
        let actual = geometry(&x11, window).expect("query test window");
        assert_eq!(actual.rectangle, Rectangle::new(7, 8, 31, 32));
        assert_eq!(actual.border_width, 2);

        x11.send_and_check_request(&x::DestroyWindow { window })
            .expect("destroy test window");
    }

    /// The pipelined reads must still report the death of a client exactly the
    /// way the request-per-property version did: the first awaited reply is the
    /// `GetWindowAttributes` one, so its `BadWindow` is what surfaces.
    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_rule_properties_reports_bad_window_for_a_dead_client() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let window: x::Window = x11.connection().generate_id();
        x11.send_and_check_request(&x::CreateWindow {
            depth: x::COPY_FROM_PARENT as u8,
            wid: window,
            parent: x11.root(),
            x: 0,
            y: 0,
            width: 16,
            height: 16,
            border_width: 0,
            class: x::WindowClass::InputOutput,
            visual: x::COPY_FROM_PARENT,
            value_list: &[],
        })
        .expect("create test window");
        rule_properties(&x11, window).expect("read a live window");

        x11.send_and_check_owned_request(&x::DestroyWindow { window })
            .expect("destroy test window");
        let error = rule_properties(&x11, window).expect_err("dead window must fail");
        assert!(matches!(
            error,
            xcb::Error::Protocol(xcb::ProtocolError::X(x::Error::Window(_), _))
        ));
        // The connection stays usable: no reply cookie was abandoned.
        x11.check_connection().expect("healthy X connection");
        assert!(!exists(&x11, window));
    }

    #[test]
    #[ignore = "requires a live X server with the Shape extension selected by DISPLAY"]
    fn live_presel_feedback_is_input_transparent_and_mutable() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let feedback: x::Window = x11.connection().generate_id();
        create_presel_feedback(&x11, feedback, 0x11_22_33).expect("create feedback");
        move_resize(&x11, feedback, Rectangle::new(7, 9, 31, 27)).expect("configure feedback");
        map(&x11, feedback).expect("map feedback");

        let shape = x11
            .request(&shape::GetRectangles {
                window: feedback,
                source_kind: shape::Sk::Input,
            })
            .expect("query input shape");
        assert!(shape.rectangles().is_empty());
        assert_eq!(
            geometry(&x11, feedback).unwrap().rectangle,
            Rectangle::new(7, 9, 31, 27)
        );

        set_background_color(&x11, feedback, 0x44_55_66).expect("recolor feedback");
        unmap(&x11, feedback).expect("unmap feedback");
        destroy(&x11, feedback).expect("destroy feedback");
        assert!(!exists(&x11, feedback));
    }
}
