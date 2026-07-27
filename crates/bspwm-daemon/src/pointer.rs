#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::module_name_repetitions
)]

//! Pointer policy, geometry planning, and X11 grabs.
//!
//! This module deliberately does not consume events.  The runtime can resolve a
//! button press with [`resolve_button_action`] and acquire an active grab without
//! entering a blocking loop here.

use xcb::x;

use crate::settings::Settings;
use crate::types::{ButtonIndex, Point, PointerAction};
use crate::x11::X11;

pub use bspwm_core::pointer::{
    ResizeInput, plan_floating_move, plan_floating_resize, resize_handle,
};
pub use bspwm_model::resize::{
    RatioUpdate, TiledResizePlan, apply_tiled_resize_plan, plan_tiled_resize,
};

const BUTTONS: [u8; 3] = [1, 2, 3];
const XK_NUM_LOCK: x::Keysym = 0xff7f;
const XK_CAPS_LOCK: x::Keysym = 0xffe5;
const XK_SCROLL_LOCK: x::Keysym = 0xff14;

/// Modifier bits occupied by the three lock keys ignored by pointer bindings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LockMasks {
    pub num_lock: u16,
    pub caps_lock: u16,
    pub scroll_lock: u16,
}

impl LockMasks {
    /// Reads the core keyboard and modifier maps, matching upstream's keysym lookup.
    pub fn query(x11: &X11) -> xcb::Result<Self> {
        let setup = x11.connection().get_setup();
        let first = setup.min_keycode();
        let count = setup.max_keycode().wrapping_sub(first).wrapping_add(1);
        let keyboard_cookie = x11.send(&x::GetKeyboardMapping {
            first_keycode: first,
            count,
        });
        let modifier_cookie = x11.send(&x::GetModifierMapping {});
        let keyboard = x11.connection().wait_for_reply(keyboard_cookie)?;
        let modifiers = x11.connection().wait_for_reply(modifier_cookie)?;

        let mask_for = |keysym| {
            modifier_mask_for_keysym(
                first,
                keyboard.keysyms_per_keycode(),
                keyboard.keysyms(),
                modifiers.keycodes_per_modifier(),
                modifiers.keycodes(),
                keysym,
            )
        };
        let caps_lock = mask_for(XK_CAPS_LOCK);
        Ok(Self {
            num_lock: mask_for(XK_NUM_LOCK),
            caps_lock: if caps_lock == 0 {
                x::ModMask::LOCK.bits() as u16
            } else {
                caps_lock
            },
            scroll_lock: mask_for(XK_SCROLL_LOCK),
        })
    }

    /// Removes lock state from an event mask before matching a binding.
    #[must_use]
    pub const fn clean(self, modifiers: u16) -> u16 {
        modifiers & !(self.num_lock | self.caps_lock | self.scroll_lock)
    }

    /// Returns upstream's base, all-locks, pairs, and individual-lock masks.
    /// Missing locks are omitted; duplicate masks are retained like the C requests.
    #[must_use]
    pub fn combinations(self, modifier: u16) -> Vec<u16> {
        let mut masks = vec![modifier];
        let (num, caps, scroll) = (self.num_lock, self.caps_lock, self.scroll_lock);
        if num != 0 && caps != 0 && scroll != 0 {
            masks.push(modifier | num | caps | scroll);
        }
        if num != 0 && caps != 0 {
            masks.push(modifier | num | caps);
        }
        if caps != 0 && scroll != 0 {
            masks.push(modifier | caps | scroll);
        }
        if num != 0 && scroll != 0 {
            masks.push(modifier | num | scroll);
        }
        if num != 0 {
            masks.push(modifier | num);
        }
        if caps != 0 {
            masks.push(modifier | caps);
        }
        if scroll != 0 {
            masks.push(modifier | scroll);
        }
        masks
    }
}

/// The policy selected for a core button press.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonAction {
    /// The unmodified click-to-focus binding.  The runtime decides whether to
    /// replay the click after attempting focus, using `swallow_first_click`.
    Focus,
    /// One of the configured modifier pointer actions.
    Pointer(PointerAction),
}

/// Resolves buttons 1-3 exactly as upstream's button-press handler does.
#[must_use]
pub fn resolve_button_action(
    button: u8,
    modifiers: u16,
    click_to_focus: ButtonIndex,
    pointer_actions: [PointerAction; 3],
    locks: LockMasks,
) -> Option<ButtonAction> {
    let index = BUTTONS.iter().position(|candidate| *candidate == button)?;
    if button_matches(click_to_focus, button) && locks.clean(modifiers) == 0 {
        Some(ButtonAction::Focus)
    } else {
        let action = pointer_actions[index];
        (action != PointerAction::None).then_some(ButtonAction::Pointer(action))
    }
}

/// Whether the original client click should be replayed after focus handling.
#[must_use]
pub const fn replay_focus_click(focus_changed: bool, swallow_first_click: bool) -> bool {
    !focus_changed || !swallow_first_click
}

/// Queues one passive grab for every lock-mask permutation without awaiting the server.
///
/// The requests are still *checked*; their cookies are drained later by
/// [`check_grabs`], so a whole batch costs one round trip instead of one per
/// permutation.
fn queue_grab_button(
    x11: &X11,
    window: x::Window,
    button: u8,
    modifier: u16,
    locks: LockMasks,
    cookies: &mut Vec<xcb::VoidCookieChecked>,
) {
    let button = x_button(button);
    let connection = x11.connection();
    for modifiers in locks.combinations(modifier) {
        cookies.push(connection.send_request_checked(&x::GrabButton {
            owner_events: false,
            grab_window: window,
            event_mask: x::EventMask::BUTTON_PRESS,
            pointer_mode: x::GrabMode::Sync,
            keyboard_mode: x::GrabMode::Async,
            confine_to: x::WINDOW_NONE,
            cursor: x::CURSOR_NONE,
            button,
            modifiers: x::ModMask::from_bits_truncate(u32::from(modifiers)),
        }));
    }
}

/// Queues every grab one client needs, in upstream's button and modifier order.
fn queue_client_buttons(
    x11: &X11,
    window: x::Window,
    settings: &Settings,
    locks: LockMasks,
    cookies: &mut Vec<xcb::VoidCookieChecked>,
) {
    for (index, button) in BUTTONS.into_iter().enumerate() {
        if button_matches(settings.click_to_focus, button) {
            queue_grab_button(x11, window, button, 0, locks, cookies);
        }
        if settings.pointer_actions[index] != PointerAction::None {
            queue_grab_button(
                x11,
                window,
                button,
                settings.pointer_modifier.mask(),
                locks,
                cookies,
            );
        }
    }
}

/// Drains a queued batch of grab cookies, returning the first non-`BadWindow` error.
///
/// Every cookie is checked even after a failure so that none is abandoned, and
/// `BadWindow` is tolerated exactly as [`X11::send_and_check_request`] does: a
/// client can disappear between the event that scheduled the grab and the grab
/// itself.
fn check_grabs(x11: &X11, cookies: Vec<xcb::VoidCookieChecked>) -> xcb::ProtocolResult<()> {
    let connection = x11.connection();
    let mut result = Ok(());
    for cookie in cookies {
        match connection.check_request(cookie) {
            Ok(()) | Err(xcb::ProtocolError::X(x::Error::Window(_), _)) => {}
            Err(error) => {
                if result.is_ok() {
                    result = Err(error);
                }
            }
        }
    }
    result
}

/// Issues one passive grab for every lock-mask permutation.
pub fn grab_button(
    x11: &X11,
    window: x::Window,
    button: u8,
    modifier: u16,
    locks: LockMasks,
) -> xcb::ProtocolResult<()> {
    let mut cookies = Vec::new();
    queue_grab_button(x11, window, button, modifier, locks, &mut cookies);
    check_grabs(x11, cookies)
}

/// Installs click-to-focus and configured pointer-action grabs on one client.
pub fn grab_client_buttons(
    x11: &X11,
    window: x::Window,
    settings: &Settings,
    locks: LockMasks,
) -> xcb::ProtocolResult<()> {
    let mut cookies = Vec::new();
    queue_client_buttons(x11, window, settings, locks, &mut cookies);
    check_grabs(x11, cookies)
}

/// Removes every passive button grab from one client window.
pub fn ungrab_client_buttons(x11: &X11, window: x::Window) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::UngrabButton {
        button: x::ButtonIndex::Any,
        grab_window: window,
        modifiers: x::ModMask::ANY,
    })
}

/// Installs grabs on a runtime-provided set of client windows.
///
/// The grabs for every client are queued before any reply is awaited, so this
/// costs one round trip in total rather than one per grab.
pub fn grab_buttons(
    x11: &X11,
    windows: impl IntoIterator<Item = x::Window>,
    settings: &Settings,
    locks: LockMasks,
) -> xcb::ProtocolResult<()> {
    let mut cookies = Vec::new();
    for window in windows {
        queue_client_buttons(x11, window, settings, locks, &mut cookies);
    }
    check_grabs(x11, cookies)
}

/// Removes grabs from a runtime-provided set of client windows.
pub fn ungrab_buttons(
    x11: &X11,
    windows: impl IntoIterator<Item = x::Window>,
) -> xcb::ProtocolResult<()> {
    let connection = x11.connection();
    let cookies: Vec<xcb::VoidCookieChecked> = windows
        .into_iter()
        .map(|window| {
            connection.send_request_checked(&x::UngrabButton {
                button: x::ButtonIndex::Any,
                grab_window: window,
                modifiers: x::ModMask::ANY,
            })
        })
        .collect();
    check_grabs(x11, cookies)
}

/// Acquires the non-confined asynchronous active grab used during move/resize.
pub fn grab_pointer(x11: &X11) -> xcb::Result<x::GrabStatus> {
    Ok(x11
        .request(&x::GrabPointer {
            owner_events: false,
            grab_window: x11.root(),
            event_mask: x::EventMask::BUTTON_RELEASE | x::EventMask::BUTTON_MOTION,
            pointer_mode: x::GrabMode::Async,
            keyboard_mode: x::GrabMode::Async,
            confine_to: x::WINDOW_NONE,
            cursor: x::CURSOR_NONE,
            time: x::CURRENT_TIME,
        })?
        .status())
}

/// Releases this client's active pointer grab.
pub fn ungrab_pointer(x11: &X11) -> xcb::ProtocolResult<()> {
    x11.send_and_check_request(&x::UngrabPointer {
        time: x::CURRENT_TIME,
    })
}

/// Queries the child window and root coordinates currently under the pointer.
pub fn query_pointer(x11: &X11) -> xcb::Result<(x::Window, Point)> {
    let reply = x11.request(&x::QueryPointer { window: x11.root() })?;
    Ok((
        reply.child(),
        Point::from_x11(reply.root_x(), reply.root_y()),
    ))
}

fn modifier_mask_for_keysym(
    first_keycode: x::Keycode,
    keysyms_per_keycode: u8,
    keysyms: &[x::Keysym],
    keycodes_per_modifier: u8,
    modifier_keycodes: &[x::Keycode],
    keysym: x::Keysym,
) -> u16 {
    if keysyms_per_keycode == 0 || keycodes_per_modifier == 0 {
        return 0;
    }
    let width = usize::from(keysyms_per_keycode);
    let mut result = 0;
    for (modifier, row) in modifier_keycodes
        .chunks_exact(usize::from(keycodes_per_modifier))
        .enumerate()
    {
        let found = row
            .iter()
            .copied()
            .filter(|keycode| *keycode != 0)
            .any(|keycode| {
                let index = usize::from(keycode.wrapping_sub(first_keycode)) * width;
                keysyms
                    .get(index..index.saturating_add(width))
                    .is_some_and(|symbols| symbols.contains(&keysym))
            });
        if found {
            result |= 1_u16 << modifier;
        }
    }
    result
}

const fn button_matches(setting: ButtonIndex, button: u8) -> bool {
    match setting {
        ButtonIndex::Any => true,
        ButtonIndex::Button1 => button == 1,
        ButtonIndex::Button2 => button == 2,
        ButtonIndex::Button3 => button == 3,
        ButtonIndex::None => false,
    }
}

const fn x_button(button: u8) -> x::ButtonIndex {
    match button {
        1 => x::ButtonIndex::N1,
        2 => x::ButtonIndex::N2,
        3 => x::ButtonIndex::N3,
        4 => x::ButtonIndex::N4,
        5 => x::ButtonIndex::N5,
        _ => x::ButtonIndex::Any,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::settings::Settings;
    use crate::tree::{Client, NodeId, Tree};
    use crate::types::{Rectangle, ResizeHandle, SplitType};

    #[test]
    fn lock_combinations_follow_upstream_order_and_omit_missing_locks() {
        let all = LockMasks {
            num_lock: 0x10,
            caps_lock: 0x02,
            scroll_lock: 0x20,
        };
        assert_eq!(
            all.combinations(0x40),
            [0x40, 0x72, 0x52, 0x62, 0x70, 0x50, 0x42, 0x60]
        );
        assert_eq!(
            LockMasks {
                num_lock: 0x10,
                caps_lock: 0,
                scroll_lock: 0,
            }
            .combinations(0x40),
            [0x40, 0x50]
        );
    }

    #[test]
    fn lock_keysyms_are_resolved_through_modifier_rows() {
        let keysyms = [0, XK_NUM_LOCK, XK_CAPS_LOCK, 0, XK_SCROLL_LOCK, 0];
        let modifier_keycodes = [0, 0, 9, 0, 10, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(
            modifier_mask_for_keysym(8, 2, &keysyms, 2, &modifier_keycodes, XK_NUM_LOCK),
            1 << 5
        );
        assert_eq!(
            modifier_mask_for_keysym(8, 2, &keysyms, 2, &modifier_keycodes, XK_CAPS_LOCK),
            1 << 1
        );
    }

    #[test]
    fn button_resolution_prioritizes_unmodified_focus() {
        let locks = LockMasks {
            num_lock: 0x10,
            caps_lock: 0x02,
            scroll_lock: 0,
        };
        let actions = [
            PointerAction::Move,
            PointerAction::ResizeSide,
            PointerAction::None,
        ];
        assert_eq!(
            resolve_button_action(1, 0x12, ButtonIndex::Button1, actions, locks),
            Some(ButtonAction::Focus)
        );
        assert_eq!(
            resolve_button_action(1, 0x52, ButtonIndex::Button1, actions, locks),
            Some(ButtonAction::Pointer(PointerAction::Move))
        );
        assert_eq!(
            resolve_button_action(3, 0x40, ButtonIndex::Button1, actions, locks),
            None
        );
        assert!(replay_focus_click(false, true));
        assert!(!replay_focus_click(true, true));
    }

    #[test]
    fn resize_handles_classify_sides_and_corners() {
        let rectangle = Rectangle::new(10, 20, 100, 50);
        let cases = [
            (
                "left side",
                Point { x: 11, y: 45 },
                PointerAction::ResizeSide,
                ResizeHandle::LEFT,
            ),
            (
                "top side",
                Point { x: 60, y: 21 },
                PointerAction::ResizeSide,
                ResizeHandle::TOP,
            ),
            (
                "right side",
                Point { x: 109, y: 45 },
                PointerAction::ResizeSide,
                ResizeHandle::RIGHT,
            ),
            (
                "bottom side",
                Point { x: 60, y: 69 },
                PointerAction::ResizeSide,
                ResizeHandle::BOTTOM,
            ),
            (
                "corner midlines belong to top left",
                Point { x: 60, y: 45 },
                PointerAction::ResizeCorner,
                ResizeHandle::TOP_LEFT,
            ),
            (
                "point below and right of corner midlines",
                Point { x: 61, y: 46 },
                PointerAction::ResizeCorner,
                ResizeHandle::BOTTOM_RIGHT,
            ),
        ];

        for (label, point, action, expected) in cases {
            assert_eq!(resize_handle(rectangle, point, action), expected, "{label}");
        }
    }

    #[test]
    fn floating_move_and_resize_preserve_anchor_geometry() {
        let rectangle = Rectangle::new(100, 200, 80, 60);
        assert_eq!(
            plan_floating_move(rectangle, -10, 15),
            Rectangle::new(90, 215, 80, 60)
        );
        assert_eq!(
            plan_floating_resize(
                rectangle,
                ResizeHandle::TOP_LEFT,
                ResizeInput::Relative { dx: 10, dy: 20 },
            ),
            Rectangle::new(110, 220, 70, 40)
        );
        assert_eq!(
            plan_floating_resize(
                rectangle,
                ResizeHandle::BOTTOM_RIGHT,
                ResizeInput::Absolute(Point { x: 210, y: 290 }),
            ),
            Rectangle::new(100, 200, 110, 90)
        );
    }

    fn tiled_tree() -> (Tree, NodeId, NodeId, NodeId) {
        let mut tree = Tree::default();
        let root = tree.add_node(100, 0.5);
        let left = tree.add_node(1, 0.5);
        let right = tree.add_node(2, 0.5);
        tree.set_children(root, left, right);
        tree.node_mut(root).rectangle = Rectangle::new(10, 20, 200, 100);
        tree.node_mut(root).split_type = SplitType::Vertical;
        let settings = Settings::default();
        tree.node_mut(left).client = Some(Client::from_settings(&settings));
        tree.node_mut(right).client = Some(Client::from_settings(&settings));
        tree.apply_layout(root, Rectangle::new(10, 20, 200, 100), false);
        (tree, root, left, right)
    }

    #[test]
    fn tiled_resize_returns_clamped_fence_updates() {
        let (mut tree, root, left, _) = tiled_tree();
        let plan = plan_tiled_resize(
            &tree,
            left,
            ResizeHandle::RIGHT,
            ResizeInput::Relative { dx: 25, dy: 0 },
        );
        assert_eq!(
            plan.vertical,
            Some(RatioUpdate {
                node: root,
                ratio: 0.625
            })
        );
        apply_tiled_resize_plan(&mut tree, plan);
        assert_eq!(tree.node(root).split_ratio, 0.625);
        let clamped = plan_tiled_resize(
            &tree,
            left,
            ResizeHandle::RIGHT,
            ResizeInput::Absolute(Point { x: -300, y: 0 }),
        );
        assert_eq!(clamped.vertical.unwrap().ratio, 0.0);
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_pointer_query_and_active_grab_round_trip() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let (_, position) = query_pointer(&x11).expect("query pointer");
        let geometry = x11.geometry();
        assert!(position.x < i32::from(geometry.width));
        assert!(position.y < i32::from(geometry.height));
        let status = grab_pointer(&x11).expect("grab pointer");
        assert_eq!(status, x::GrabStatus::Success);
        ungrab_pointer(&x11).expect("ungrab pointer");
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn live_client_passive_grabs_round_trip() {
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
        .expect("create window");
        let locks = LockMasks::query(&x11).expect("query lock masks");
        let settings = Settings::default();
        grab_client_buttons(&x11, window, &settings, locks).expect("grab client buttons");
        grab_buttons(&x11, [window], &settings, locks).expect("grab buttons in batch");
        ungrab_buttons(&x11, [window]).expect("ungrab buttons in batch");
        ungrab_client_buttons(&x11, window).expect("ungrab client buttons");
        x11.send_and_check_request(&x::DestroyWindow { window })
            .expect("destroy window");

        // A client that dies before its grabs are installed is tolerated, just
        // as it was when each permutation was checked on its own.
        grab_client_buttons(&x11, window, &settings, locks).expect("dead window is tolerated");
        ungrab_buttons(&x11, [window]).expect("dead window is tolerated");
        x11.check_connection().expect("healthy X connection");
    }
}
