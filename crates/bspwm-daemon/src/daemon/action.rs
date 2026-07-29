//! The checked X requests daemon policy produces, and their executor.

use xcb::{XidNew, x};

use super::DaemonApp;
use crate::arrange::ArrangeAction;
use crate::events::SyntheticConfigurePlan;
use crate::runtime::RuntimeError;
use crate::types::Rectangle;
use crate::window;
use crate::x11::X11;

/// One checked X request produced by daemon policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XAction {
    Configure {
        window: u32,
        rectangle: Rectangle,
        border_width: u32,
    },
    SetBorderWidth {
        window: u32,
        border_width: u32,
    },
    SyntheticConfigure {
        window: u32,
        rectangle: Rectangle,
        border_width: u16,
    },
    StackAbove {
        window: u32,
        sibling: u32,
    },
    StackBelow {
        window: u32,
        sibling: u32,
    },
    Map {
        window: u32,
    },
    Lower {
        window: u32,
    },
    SetClientEventMask {
        window: u32,
        enter_window: bool,
    },
    SetWmStateNormal {
        window: u32,
    },
    Focus {
        window: u32,
    },
}

impl DaemonApp {
    #[must_use]
    pub const fn arrange_action(action: &ArrangeAction) -> XAction {
        XAction::Configure {
            window: action.window,
            rectangle: action.rectangle,
            border_width: action.border_width,
        }
    }

    pub fn execute_plan(x11: &X11, plan: &[XAction]) -> Result<(), RuntimeError> {
        for action in plan {
            Self::execute_action(x11, *action)?;
        }
        Ok(())
    }

    pub(super) fn execute_action(x11: &X11, action: XAction) -> Result<(), RuntimeError> {
        let result = match action {
            XAction::Configure {
                window: id,
                rectangle,
                border_width,
            } => window::configure(
                x11,
                x::Window::new(id),
                &[
                    x::ConfigWindow::X(rectangle.x),
                    x::ConfigWindow::Y(rectangle.y),
                    x::ConfigWindow::Width(u32::try_from(rectangle.width).unwrap_or(0)),
                    x::ConfigWindow::Height(u32::try_from(rectangle.height).unwrap_or(0)),
                    x::ConfigWindow::BorderWidth(border_width),
                ],
            ),
            XAction::SyntheticConfigure {
                window: id,
                rectangle,
                border_width,
            } => SyntheticConfigurePlan {
                window: x::Window::new(id),
                rectangle,
                border_width,
            }
            .execute(x11),
            XAction::SetBorderWidth {
                window: id,
                border_width,
            } => window::set_border_width(x11, x::Window::new(id), border_width),
            XAction::StackAbove {
                window: id,
                sibling,
            } => window::stack_above(x11, x::Window::new(id), x::Window::new(sibling)),
            XAction::StackBelow {
                window: id,
                sibling,
            } => window::stack_below(x11, x::Window::new(id), x::Window::new(sibling)),
            XAction::Map { window: id } => window::map(x11, x::Window::new(id)),
            XAction::Lower { window: id } => window::lower(x11, x::Window::new(id)),
            XAction::SetClientEventMask {
                window: id,
                enter_window,
            } => {
                let mut mask = x::EventMask::PROPERTY_CHANGE | x::EventMask::FOCUS_CHANGE;
                if enter_window {
                    mask |= x::EventMask::ENTER_WINDOW;
                }
                x11.send_and_check_request(&x::ChangeWindowAttributes {
                    window: x::Window::new(id),
                    value_list: &[x::Cw::EventMask(mask)],
                })
            }
            XAction::SetWmStateNormal { window: id } => {
                set_wm_state_property(x11, x::Window::new(id), 1)
            }
            XAction::Focus { window: id } => window::focus(x11, x::Window::new(id)),
        };
        Ok(result?)
    }
}

/// Writes the ICCCM `WM_STATE` property, whose payload is a state plus an icon window.
pub(super) fn set_wm_state_property(
    x11: &X11,
    window: x::Window,
    state: u32,
) -> xcb::ProtocolResult<()> {
    window::set_property(
        x11,
        window,
        x11.atoms().wm_state,
        x11.atoms().wm_state,
        &[state, 0],
    )
}

#[cfg(test)]
mod tests {
    use crate::daemon::test_support::{app_with_desktop, manage_window};

    #[test]
    fn arrange_actions_translate_to_window_ids() {
        let (mut app, _, desktop) = app_with_desktop();
        let (_, _, _first) = manage_window(&mut app, 10);
        let (_, _, second) = manage_window(&mut app, 20);
        assert_eq!(app.state.world.desktop(desktop).tree.focus, Some(second));
    }
}
