//! Translation boundary between `xcb` events and bspwm's daemon state.
//!
//! This module deliberately contains no window-management policy. Implementors of
//! [`EventHandler`] can borrow the daemon as needed, while this boundary retains
//! protocol ordering and exposes the state needed by upstream's event filters.

use xcb::{Xid, XidNew, randr, x};

use crate::types::{Rectangle, wrapping_i16, wrapping_u16};
use crate::x11::{Atoms, X11};

/// The result of dispatching one item read from the X connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    Handled,
    IgnoredEvent,
    IgnoredBadWindow,
}

/// An error that prevented an event from being dispatched.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError<E> {
    #[error("X connection error: {0}")]
    Connection(#[source] xcb::ConnError),
    #[error("event handler failed: {0}")]
    Handler(#[source] E),
}

/// Operations corresponding to every event category consumed by upstream
/// `events.c`, plus both event categories exposed by `RandR`.
///
/// There are intentionally no default implementations: silently dropping a
/// newly unwired policy handler is more dangerous than requiring it explicitly.
#[allow(clippy::missing_errors_doc)]
pub trait EventHandler {
    type Error;

    fn map_request(&mut self, event: &x::MapRequestEvent) -> Result<(), Self::Error>;
    fn destroy_notify(&mut self, event: &x::DestroyNotifyEvent) -> Result<(), Self::Error>;
    fn unmap_notify(&mut self, event: &x::UnmapNotifyEvent) -> Result<(), Self::Error>;
    fn client_message(&mut self, event: &x::ClientMessageEvent) -> Result<(), Self::Error>;
    fn configure_request(&mut self, event: &x::ConfigureRequestEvent) -> Result<(), Self::Error>;
    fn configure_notify(&mut self, event: &x::ConfigureNotifyEvent) -> Result<(), Self::Error>;
    fn property_notify(&mut self, event: &x::PropertyNotifyEvent) -> Result<(), Self::Error>;
    fn enter_notify(&mut self, event: &x::EnterNotifyEvent) -> Result<(), Self::Error>;
    fn motion_notify(&mut self, event: &x::MotionNotifyEvent) -> Result<(), Self::Error>;
    fn button_press(&mut self, event: &x::ButtonPressEvent) -> Result<(), Self::Error>;
    fn button_release(&mut self, event: &x::ButtonReleaseEvent) -> Result<(), Self::Error>;
    fn focus_in(&mut self, event: &x::FocusInEvent) -> Result<(), Self::Error>;
    fn mapping_notify(&mut self, event: &x::MappingNotifyEvent) -> Result<(), Self::Error>;
    fn randr_screen_change_notify(
        &mut self,
        event: &randr::ScreenChangeNotifyEvent,
    ) -> Result<(), Self::Error>;
    fn randr_notify(&mut self, event: &randr::NotifyEvent) -> Result<(), Self::Error>;
    fn protocol_error(&mut self, error: &xcb::ProtocolError) -> Result<(), Self::Error>;
}

/// Dispatches an event or asynchronous error returned by `xcb`.
///
/// Core events not selected by upstream bspwm and events from unrelated enabled
/// extensions are ignored. As in upstream, `BadWindow` is ignored because races
/// with disappearing clients are unavoidable; all other protocol errors are
/// presented to the handler. Fatal connection errors are returned directly.
///
/// # Errors
/// Returns [`DispatchError::Connection`] for a fatal connection error, or
/// [`DispatchError::Handler`] when the selected handler method fails.
pub fn handle_event<H: EventHandler>(
    handler: &mut H,
    item: xcb::Result<xcb::Event>,
) -> Result<DispatchOutcome, DispatchError<H::Error>> {
    let result = match item {
        Ok(xcb::Event::X(event)) => match event {
            x::Event::MapRequest(event) => handler.map_request(&event),
            x::Event::DestroyNotify(event) => handler.destroy_notify(&event),
            x::Event::UnmapNotify(event) => handler.unmap_notify(&event),
            x::Event::ClientMessage(event) => handler.client_message(&event),
            x::Event::ConfigureRequest(event) => handler.configure_request(&event),
            x::Event::ConfigureNotify(event) => handler.configure_notify(&event),
            x::Event::PropertyNotify(event) => handler.property_notify(&event),
            x::Event::EnterNotify(event) => handler.enter_notify(&event),
            x::Event::MotionNotify(event) => handler.motion_notify(&event),
            x::Event::ButtonPress(event) => handler.button_press(&event),
            x::Event::ButtonRelease(event) => handler.button_release(&event),
            x::Event::FocusIn(event) => handler.focus_in(&event),
            x::Event::MappingNotify(event) => handler.mapping_notify(&event),
            _ => return Ok(DispatchOutcome::IgnoredEvent),
        },
        Ok(xcb::Event::RandR(event)) => match event {
            randr::Event::ScreenChangeNotify(event) => handler.randr_screen_change_notify(&event),
            randr::Event::Notify(event) => handler.randr_notify(&event),
        },
        Ok(_) => return Ok(DispatchOutcome::IgnoredEvent),
        Err(xcb::Error::Connection(error)) => return Err(DispatchError::Connection(error)),
        Err(xcb::Error::Protocol(error)) if is_bad_window(&error) => {
            return Ok(DispatchOutcome::IgnoredBadWindow);
        }
        Err(xcb::Error::Protocol(error)) => handler.protocol_error(&error),
    };

    result
        .map(|()| DispatchOutcome::Handled)
        .map_err(DispatchError::Handler)
}

fn is_bad_window(error: &xcb::ProtocolError) -> bool {
    matches!(error, xcb::ProtocolError::X(x::Error::Window(_), _))
}

/// Ordered request data for forwarding an unmanaged `ConfigureRequest`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigureRequestPlan {
    pub window: x::Window,
    pub values: Vec<x::ConfigWindow>,
}

impl ConfigureRequestPlan {
    /// Copies only fields selected by `value_mask`, in X protocol mask order.
    #[must_use]
    pub fn forward(event: &x::ConfigureRequestEvent) -> Self {
        let mask = event.value_mask();
        let mut values = Vec::with_capacity(7);
        if mask.contains(x::ConfigWindowMask::X) {
            values.push(x::ConfigWindow::X(i32::from(event.x())));
        }
        if mask.contains(x::ConfigWindowMask::Y) {
            values.push(x::ConfigWindow::Y(i32::from(event.y())));
        }
        if mask.contains(x::ConfigWindowMask::WIDTH) {
            values.push(x::ConfigWindow::Width(u32::from(event.width())));
        }
        if mask.contains(x::ConfigWindowMask::HEIGHT) {
            values.push(x::ConfigWindow::Height(u32::from(event.height())));
        }
        if mask.contains(x::ConfigWindowMask::BORDER_WIDTH) {
            values.push(x::ConfigWindow::BorderWidth(u32::from(
                event.border_width(),
            )));
        }
        if mask.contains(x::ConfigWindowMask::SIBLING) {
            values.push(x::ConfigWindow::Sibling(event.sibling()));
        }
        if mask.contains(x::ConfigWindowMask::STACK_MODE) {
            values.push(x::ConfigWindow::StackMode(event.stack_mode()));
        }
        Self {
            window: event.window(),
            values,
        }
    }

    /// Sends this forwarding plan as a checked `ConfigureWindow` request.
    ///
    /// # Errors
    /// Returns the protocol error reported for the checked request.
    pub fn execute(&self, x11: &X11) -> xcb::ProtocolResult<()> {
        x11.send_and_check_request(&x::ConfigureWindow {
            window: self.window,
            value_list: &self.values,
        })
    }
}

/// Data for the synthetic `ConfigureNotify` sent when a managed tiled client
/// cannot choose its own geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntheticConfigurePlan {
    pub window: x::Window,
    pub rectangle: Rectangle,
    pub border_width: u16,
}

impl SyntheticConfigurePlan {
    #[must_use]
    pub fn event(self) -> x::ConfigureNotifyEvent {
        x::ConfigureNotifyEvent::new(
            self.window,
            self.window,
            x::Window::none(),
            wrapping_i16(self.rectangle.x),
            wrapping_i16(self.rectangle.y),
            wrapping_u16(self.rectangle.width),
            wrapping_u16(self.rectangle.height),
            self.border_width,
            false,
        )
    }

    /// Sends the synthetic event with the ICCCM-required structure mask.
    ///
    /// # Errors
    /// Returns the protocol error reported for the checked request.
    pub fn execute(self, x11: &X11) -> xcb::ProtocolResult<()> {
        let event = self.event();
        x11.send_and_check_request(&x::SendEvent {
            propagate: false,
            destination: x::SendEventDest::Window(self.window),
            event_mask: x::EventMask::STRUCTURE_NOTIFY,
            event: &event,
        })
    }
}

/// A decoded 32-bit EWMH client message understood by upstream bspwm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EwmhClientMessage {
    CurrentDesktop {
        desktop: u32,
    },
    WmState {
        action: u32,
        /// State atoms retain wire order because upstream applies the first before
        /// the second and the two transitions can interact.
        states: [x::Atom; 2],
    },
    ActiveWindow {
        source: u32,
        timestamp: x::Timestamp,
        current_active: u32,
    },
    WmDesktop {
        desktop: u32,
    },
    CloseWindow {
        timestamp: x::Timestamp,
    },
    MoveResizeWindow {
        gravity: u8,
        flags: u8,
        source: u8,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    WmMoveResize {
        root_x: i32,
        root_y: i32,
        direction: u32,
        button: u8,
        source: u32,
    },
    RequestFrameExtents,
    RestackWindow,
}

/// Decodes the EWMH messages handled in upstream `client_message`.
///
/// Messages with a non-32-bit payload or an unrelated atom return `None`.
#[must_use]
pub fn decode_ewmh_client_message(
    event: &x::ClientMessageEvent,
    atoms: &Atoms,
) -> Option<EwmhClientMessage> {
    let x::ClientMessageData::Data32(data) = event.data() else {
        return None;
    };

    let message_type = event.r#type();
    if message_type == atoms.net_current_desktop {
        Some(EwmhClientMessage::CurrentDesktop { desktop: data[0] })
    } else if message_type == atoms.net_wm_state {
        Some(EwmhClientMessage::WmState {
            action: data[0],
            states: [x::Atom::new(data[1]), x::Atom::new(data[2])],
        })
    } else if message_type == atoms.net_active_window {
        Some(EwmhClientMessage::ActiveWindow {
            source: data[0],
            timestamp: data[1],
            current_active: data[2],
        })
    } else if message_type == atoms.net_wm_desktop {
        Some(EwmhClientMessage::WmDesktop { desktop: data[0] })
    } else if message_type == atoms.net_close_window {
        Some(EwmhClientMessage::CloseWindow { timestamp: data[0] })
    } else if message_type == atoms.net_moveresize_window {
        Some(EwmhClientMessage::MoveResizeWindow {
            gravity: (data[0] & 0xFF) as u8,
            flags: ((data[0] >> 8) & 0x0F) as u8,
            source: ((data[0] >> 12) & 0x0F) as u8,
            x: data[1].cast_signed(),
            y: data[2].cast_signed(),
            width: data[3],
            height: data[4],
        })
    } else if message_type == atoms.net_wm_moveresize {
        Some(EwmhClientMessage::WmMoveResize {
            root_x: data[0].cast_signed(),
            root_y: data[1].cast_signed(),
            direction: data[2],
            button: u8::try_from(data[3]).unwrap_or(0),
            source: data[4],
        })
    } else if message_type == atoms.net_restack_window {
        Some(EwmhClientMessage::RestackWindow)
    } else if message_type == atoms.net_request_frame_extents {
        Some(EwmhClientMessage::RequestFrameExtents)
    } else {
        None
    }
}

/// Mutable data used by the upstream enter/motion filters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PointerFilterState {
    motion_recorder_sequence: Option<u16>,
    last_motion: Option<MotionSample>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MotionSample {
    time: x::Timestamp,
    x: i16,
    y: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MotionDisposition {
    Seeded,
    BelowThreshold,
    Dispatch,
}

impl PointerFilterState {
    /// Records the sequence of an unmap known by policy to belong to the motion
    /// recorder. The caller decides which window is the recorder.
    pub const fn record_motion_recorder_unmap(&mut self, sequence: u16) {
        self.motion_recorder_sequence = Some(sequence);
    }

    /// Tests the i3-style sequence filter used by upstream for generated enters.
    #[must_use]
    pub fn is_generated_enter(&self, recorder_enabled: bool, sequence: u16) -> bool {
        recorder_enabled && self.motion_recorder_sequence == Some(sequence)
    }

    /// Applies upstream's time seed and ten-pixel Manhattan-distance filters.
    pub fn classify_motion(&mut self, event: &x::MotionNotifyEvent) -> MotionDisposition {
        let sample = MotionSample {
            time: event.time(),
            x: event.event_x(),
            y: event.event_y(),
        };
        let Some(previous) = self.last_motion else {
            self.last_motion = Some(sample);
            return MotionDisposition::Seeded;
        };

        if sample.time.wrapping_sub(previous.time) > 1_000 {
            self.last_motion = Some(sample);
            return MotionDisposition::Seeded;
        }
        let distance = (i32::from(sample.x) - i32::from(previous.x)).abs()
            + (i32::from(sample.y) - i32::from(previous.y)).abs();
        if distance < 10 {
            MotionDisposition::BelowThreshold
        } else {
            MotionDisposition::Dispatch
        }
    }
}

/// State for filtering the `MappingNotify` events generated by keyboard setup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MappingFilterState {
    pending: i8,
}

impl MappingFilterState {
    #[must_use]
    pub const fn new(pending: i8) -> Self {
        Self { pending }
    }

    #[must_use]
    pub const fn pending(self) -> i8 {
        self.pending
    }

    /// Returns whether button grabs should be rebuilt. Pointer mapping events do
    /// not consume the counter, matching upstream ordering.
    pub fn should_regrab(&mut self, request: x::Mapping) -> bool {
        if self.pending == 0 || request == x::Mapping::Pointer {
            return false;
        }
        if self.pending > 0 {
            self.pending -= 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u32) -> x::Window {
        x::Window::new(id)
    }

    #[test]
    fn configure_forwarding_preserves_mask_order() {
        let event = x::ConfigureRequestEvent::new(
            x::StackMode::Below,
            window(1),
            window(2),
            window(3),
            -10,
            20,
            800,
            600,
            4,
            x::ConfigWindowMask::X
                | x::ConfigWindowMask::WIDTH
                | x::ConfigWindowMask::BORDER_WIDTH
                | x::ConfigWindowMask::SIBLING
                | x::ConfigWindowMask::STACK_MODE,
        );

        assert_eq!(
            ConfigureRequestPlan::forward(&event),
            ConfigureRequestPlan {
                window: window(2),
                values: vec![
                    x::ConfigWindow::X(-10),
                    x::ConfigWindow::Width(800),
                    x::ConfigWindow::BorderWidth(4),
                    x::ConfigWindow::Sibling(window(3)),
                    x::ConfigWindow::StackMode(x::StackMode::Below),
                ],
            }
        );
    }

    #[test]
    fn synthetic_configure_uses_client_as_event_and_window() {
        let event = SyntheticConfigurePlan {
            window: window(7),
            rectangle: Rectangle::new(i32::from(i16::MAX) + 1, -32_769, -1, 65_537),
            border_width: 2,
        }
        .event();

        assert_eq!(event.event(), window(7));
        assert_eq!(event.window(), window(7));
        assert_eq!(event.above_sibling(), x::Window::none());
        assert_eq!((event.x(), event.y()), (i16::MIN, i16::MAX));
        assert_eq!((event.width(), event.height()), (u16::MAX, 1));
        assert_eq!(event.border_width(), 2);
        assert!(!event.override_redirect());
    }

    #[test]
    fn mapping_pointer_does_not_consume_pending_event() {
        let mut state = MappingFilterState::new(2);
        assert!(!state.should_regrab(x::Mapping::Pointer));
        assert_eq!(state.pending(), 2);
        assert!(state.should_regrab(x::Mapping::Keyboard));
        assert_eq!(state.pending(), 1);
        assert!(state.should_regrab(x::Mapping::Modifier));
        assert!(!state.should_regrab(x::Mapping::Keyboard));

        let mut unlimited = MappingFilterState::new(-1);
        assert!(unlimited.should_regrab(x::Mapping::Keyboard));
        assert!(unlimited.should_regrab(x::Mapping::Modifier));
        assert_eq!(unlimited.pending(), -1);
    }

    #[test]
    fn recorder_sequence_only_filters_while_enabled() {
        let mut state = PointerFilterState::default();
        state.record_motion_recorder_unmap(42);
        assert!(state.is_generated_enter(true, 42));
        assert!(!state.is_generated_enter(false, 42));
        assert!(!state.is_generated_enter(true, 43));
    }

    #[test]
    fn motion_filter_seeds_then_applies_manhattan_threshold() {
        fn motion(time: x::Timestamp, event_x: i16, event_y: i16) -> x::MotionNotifyEvent {
            x::MotionNotifyEvent::new(
                x::Motion::Normal,
                time,
                window(1),
                window(2),
                x::Window::none(),
                event_x,
                event_y,
                event_x,
                event_y,
                x::KeyButMask::empty(),
                true,
            )
        }

        let mut state = PointerFilterState::default();
        assert_eq!(
            state.classify_motion(&motion(2_000, 10, 10)),
            MotionDisposition::Seeded
        );
        assert_eq!(
            state.classify_motion(&motion(2_100, 14, 15)),
            MotionDisposition::BelowThreshold
        );
        assert_eq!(
            state.classify_motion(&motion(2_200, 20, 10)),
            MotionDisposition::Dispatch
        );
        assert_eq!(
            state.classify_motion(&motion(3_201, 20, 10)),
            MotionDisposition::Seeded
        );
    }
}
