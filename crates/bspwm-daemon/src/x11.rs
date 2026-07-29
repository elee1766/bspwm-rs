use std::cell::Cell;
use std::os::fd::{AsRawFd, RawFd};

use xcb::{
    CookieWithReplyChecked, Extension, ExtensionData, Request, RequestWithoutReply, sync, x,
};

xcb::atoms_struct! {
    #[derive(Copy, Clone, Debug)]
    pub struct Atoms {
        pub wm_state => b"WM_STATE" only_if_exists = false,
        pub wm_protocols => b"WM_PROTOCOLS" only_if_exists = false,
        pub wm_take_focus => b"WM_TAKE_FOCUS" only_if_exists = false,
        pub wm_delete_window => b"WM_DELETE_WINDOW" only_if_exists = false,
        pub net_wm_ping => b"_NET_WM_PING" only_if_exists = false,
        pub net_wm_sync_request => b"_NET_WM_SYNC_REQUEST" only_if_exists = false,
        pub net_wm_sync_request_counter => b"_NET_WM_SYNC_REQUEST_COUNTER" only_if_exists = false,
        pub wm_class => b"WM_CLASS" only_if_exists = false,
        pub wm_name => b"WM_NAME" only_if_exists = false,
        pub wm_transient_for => b"WM_TRANSIENT_FOR" only_if_exists = false,
        pub wm_normal_hints => b"WM_NORMAL_HINTS" only_if_exists = false,
        pub wm_size_hints => b"WM_SIZE_HINTS" only_if_exists = false,
        pub utf8_string => b"UTF8_STRING" only_if_exists = false,
        pub net_supported => b"_NET_SUPPORTED" only_if_exists = false,
        pub net_supporting_wm_check => b"_NET_SUPPORTING_WM_CHECK" only_if_exists = false,
        pub net_desktop_names => b"_NET_DESKTOP_NAMES" only_if_exists = false,
        pub net_desktop_geometry => b"_NET_DESKTOP_GEOMETRY" only_if_exists = false,
        pub net_desktop_viewport => b"_NET_DESKTOP_VIEWPORT" only_if_exists = false,
        pub net_workarea => b"_NET_WORKAREA" only_if_exists = false,
        pub net_number_of_desktops => b"_NET_NUMBER_OF_DESKTOPS" only_if_exists = false,
        pub net_current_desktop => b"_NET_CURRENT_DESKTOP" only_if_exists = false,
        pub net_client_list => b"_NET_CLIENT_LIST" only_if_exists = false,
        pub net_client_list_stacking => b"_NET_CLIENT_LIST_STACKING" only_if_exists = false,
        pub net_active_window => b"_NET_ACTIVE_WINDOW" only_if_exists = false,
        pub net_close_window => b"_NET_CLOSE_WINDOW" only_if_exists = false,
        pub net_restack_window => b"_NET_RESTACK_WINDOW" only_if_exists = false,
        pub net_moveresize_window => b"_NET_MOVERESIZE_WINDOW" only_if_exists = false,
        pub net_wm_moveresize => b"_NET_WM_MOVERESIZE" only_if_exists = false,
        pub net_request_frame_extents => b"_NET_REQUEST_FRAME_EXTENTS" only_if_exists = false,
        pub net_frame_extents => b"_NET_FRAME_EXTENTS" only_if_exists = false,
        pub net_wm_strut_partial => b"_NET_WM_STRUT_PARTIAL" only_if_exists = false,
        pub net_wm_strut => b"_NET_WM_STRUT" only_if_exists = false,
        pub net_wm_desktop => b"_NET_WM_DESKTOP" only_if_exists = false,
        pub net_wm_user_time => b"_NET_WM_USER_TIME" only_if_exists = false,
        pub net_wm_user_time_window => b"_NET_WM_USER_TIME_WINDOW" only_if_exists = false,
        pub net_startup_id => b"_NET_STARTUP_ID" only_if_exists = false,
        pub net_startup_info_begin => b"_NET_STARTUP_INFO_BEGIN" only_if_exists = false,
        pub net_startup_info => b"_NET_STARTUP_INFO" only_if_exists = false,
        pub net_wm_state => b"_NET_WM_STATE" only_if_exists = false,
        pub net_wm_state_hidden => b"_NET_WM_STATE_HIDDEN" only_if_exists = false,
        pub net_wm_state_fullscreen => b"_NET_WM_STATE_FULLSCREEN" only_if_exists = false,
        pub net_wm_state_below => b"_NET_WM_STATE_BELOW" only_if_exists = false,
        pub net_wm_state_above => b"_NET_WM_STATE_ABOVE" only_if_exists = false,
        pub net_wm_state_sticky => b"_NET_WM_STATE_STICKY" only_if_exists = false,
        pub net_wm_state_demands_attention => b"_NET_WM_STATE_DEMANDS_ATTENTION" only_if_exists = false,
        pub net_wm_state_focused => b"_NET_WM_STATE_FOCUSED" only_if_exists = false,
        pub net_wm_state_modal => b"_NET_WM_STATE_MODAL" only_if_exists = false,
        pub net_wm_state_maximized_vert => b"_NET_WM_STATE_MAXIMIZED_VERT" only_if_exists = false,
        pub net_wm_state_maximized_horz => b"_NET_WM_STATE_MAXIMIZED_HORZ" only_if_exists = false,
        pub net_wm_state_shaded => b"_NET_WM_STATE_SHADED" only_if_exists = false,
        pub net_wm_allowed_actions => b"_NET_WM_ALLOWED_ACTIONS" only_if_exists = false,
        pub net_wm_action_move => b"_NET_WM_ACTION_MOVE" only_if_exists = false,
        pub net_wm_action_resize => b"_NET_WM_ACTION_RESIZE" only_if_exists = false,
        pub net_wm_action_minimize => b"_NET_WM_ACTION_MINIMIZE" only_if_exists = false,
        pub net_wm_action_stick => b"_NET_WM_ACTION_STICK" only_if_exists = false,
        pub net_wm_action_fullscreen => b"_NET_WM_ACTION_FULLSCREEN" only_if_exists = false,
        pub net_wm_action_change_desktop => b"_NET_WM_ACTION_CHANGE_DESKTOP" only_if_exists = false,
        pub net_wm_action_close => b"_NET_WM_ACTION_CLOSE" only_if_exists = false,
        pub net_wm_action_above => b"_NET_WM_ACTION_ABOVE" only_if_exists = false,
        pub net_wm_action_below => b"_NET_WM_ACTION_BELOW" only_if_exists = false,
        pub net_wm_state_skip_taskbar => b"_NET_WM_STATE_SKIP_TASKBAR" only_if_exists = false,
        pub net_wm_state_skip_pager => b"_NET_WM_STATE_SKIP_PAGER" only_if_exists = false,
        pub net_wm_name => b"_NET_WM_NAME" only_if_exists = false,
        pub net_wm_pid => b"_NET_WM_PID" only_if_exists = false,
        pub net_wm_window_type => b"_NET_WM_WINDOW_TYPE" only_if_exists = false,
        pub net_wm_window_type_dock => b"_NET_WM_WINDOW_TYPE_DOCK" only_if_exists = false,
        pub net_wm_window_type_desktop => b"_NET_WM_WINDOW_TYPE_DESKTOP" only_if_exists = false,
        pub net_wm_window_type_notification => b"_NET_WM_WINDOW_TYPE_NOTIFICATION" only_if_exists = false,
        pub net_wm_window_type_dialog => b"_NET_WM_WINDOW_TYPE_DIALOG" only_if_exists = false,
        pub net_wm_window_type_utility => b"_NET_WM_WINDOW_TYPE_UTILITY" only_if_exists = false,
        pub net_wm_window_type_toolbar => b"_NET_WM_WINDOW_TYPE_TOOLBAR" only_if_exists = false,
        pub net_desktop_layout => b"_NET_DESKTOP_LAYOUT" only_if_exists = false,
        pub net_wm_opaque_region => b"_NET_WM_OPAQUE_REGION" only_if_exists = false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenGeometry {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

impl ScreenGeometry {
    #[must_use]
    pub const fn root(width: u16, height: u16) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionInfo {
    pub major_opcode: u8,
    pub first_event: u8,
    pub first_error: u8,
}

impl From<ExtensionData> for ExtensionInfo {
    fn from(data: ExtensionData) -> Self {
        Self {
            major_opcode: data.major_opcode,
            first_event: data.first_event,
            first_error: data.first_error,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Extensions {
    pub randr: Option<ExtensionInfo>,
    pub xinerama: Option<ExtensionInfo>,
    pub shape: Option<ExtensionInfo>,
    pub sync: Option<ExtensionInfo>,
}

impl Extensions {
    fn query(connection: &xcb::Connection) -> Self {
        Self {
            randr: xcb::randr::get_extension_data(connection).map(Into::into),
            xinerama: xcb::xinerama::get_extension_data(connection).map(Into::into),
            sync: sync::get_extension_data(connection).map(Into::into),
            shape: xcb::shape::get_extension_data(connection).map(Into::into),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("failed to connect to X server: {0}")]
    Connection(#[from] xcb::ConnError),
    #[error("X11 setup failed: {0}")]
    Xcb(#[from] xcb::Error),
    #[error("X server has no default screen {0}")]
    MissingDefaultScreen(i32),
}

pub struct X11 {
    connection: xcb::Connection,
    default_screen: i32,
    root: x::Window,
    geometry: Cell<ScreenGeometry>,
    atoms: Atoms,
    extensions: Extensions,
}

impl X11 {
    /// Opens an X connection and acquires the selected screen's setup data.
    ///
    /// # Errors
    /// Returns an error when the display cannot be opened, its selected screen is
    /// unavailable, atom interning fails, or the connection enters an error state.
    pub fn connect(display_name: Option<&str>) -> Result<Self, ConnectError> {
        let optional = [
            Extension::RandR,
            Extension::Xinerama,
            Extension::Shape,
            Extension::Sync,
        ];
        let (connection, default_screen) =
            xcb::Connection::connect_with_extensions(display_name, &[], &optional)?;
        connection.has_error()?;

        let screen_index = usize::try_from(default_screen)
            .map_err(|_| ConnectError::MissingDefaultScreen(default_screen))?;
        let (root, geometry) = {
            let setup = connection.get_setup();
            let screen = setup
                .roots()
                .nth(screen_index)
                .ok_or(ConnectError::MissingDefaultScreen(default_screen))?;
            (
                screen.root(),
                ScreenGeometry::root(screen.width_in_pixels(), screen.height_in_pixels()),
            )
        };
        let atoms = Atoms::intern_all(&connection)?;
        let extensions = Extensions::query(&connection);

        Ok(Self {
            connection,
            default_screen,
            root,
            geometry: Cell::new(geometry),
            atoms,
            extensions,
        })
    }

    #[must_use]
    pub const fn default_screen(&self) -> i32 {
        self.default_screen
    }

    #[must_use]
    pub const fn root(&self) -> x::Window {
        self.root
    }

    #[must_use]
    pub fn geometry(&self) -> ScreenGeometry {
        self.geometry.get()
    }

    pub fn update_screen_dimensions(&self, width: u16, height: u16) {
        self.geometry.set(ScreenGeometry::root(width, height));
    }

    #[must_use]
    pub const fn atoms(&self) -> &Atoms {
        &self.atoms
    }

    #[must_use]
    pub const fn extensions(&self) -> Extensions {
        self.extensions
    }

    #[must_use]
    pub const fn connection(&self) -> &xcb::Connection {
        &self.connection
    }

    /// Returns the connection's socket descriptor, for readiness polling.
    ///
    /// Readability of this descriptor only reports bytes waiting in the kernel.
    /// It says nothing about events libxcb has already parsed into its own
    /// queue, so callers must drain [`X11::poll_for_event`] to `None` before
    /// they block on it.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.connection.as_raw_fd()
    }

    /// Checks whether the connection has shut down because of a fatal error.
    ///
    /// # Errors
    /// Returns the fatal connection error, if one has occurred.
    pub fn check_connection(&self) -> xcb::ConnResult<()> {
        self.connection.has_error()
    }

    /// Sends a request expecting a reply and blocks until the reply arrives.
    ///
    /// # Errors
    /// Returns a connection or protocol error if the reply cannot be retrieved.
    pub fn request<R>(
        &self,
        request: &R,
    ) -> xcb::Result<<R::Cookie as CookieWithReplyChecked>::Reply>
    where
        R: Request,
        R::Cookie: CookieWithReplyChecked,
    {
        let cookie = self.send(request);
        self.connection.wait_for_reply(cookie)
    }

    /// Queues a request and returns its cookie, so callers can pipeline.
    pub fn send<R: Request>(&self, request: &R) -> R::Cookie {
        self.connection.send_request(request)
    }

    /// Interns `name` and returns its strongly typed X atom identifier.
    ///
    /// # Errors
    /// Returns a connection or protocol error if the reply cannot be retrieved.
    pub fn intern_atom(&self, name: &[u8], only_if_exists: bool) -> xcb::Result<x::Atom> {
        Ok(self
            .request(&x::InternAtom {
                only_if_exists,
                name,
            })?
            .atom())
    }

    /// Sends a void request against a *client* window and checks it.
    ///
    /// `BadWindow` is reported as success because a client can be destroyed
    /// between the event that scheduled the request and the request itself, and
    /// that race is unavoidable. Requests against resources this window manager
    /// owns must use [`X11::send_and_check_owned_request`] instead: for those, a
    /// `BadWindow` means the window manager's own bookkeeping is wrong and
    /// silently continuing would hide the fault.
    ///
    /// # Errors
    /// Returns any protocol error reported for the request other than `BadWindow`.
    pub fn send_and_check_request<R>(&self, request: &R) -> xcb::ProtocolResult<()>
    where
        R: RequestWithoutReply,
    {
        match self.connection.send_and_check_request(request) {
            Ok(()) | Err(xcb::ProtocolError::X(x::Error::Window(_), _)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Sends a void request against a resource this window manager owns, such as
    /// the root window, the meta window, or a preselection feedback window.
    ///
    /// # Errors
    /// Returns any protocol error reported for the request, including `BadWindow`.
    pub fn send_and_check_owned_request<R>(&self, request: &R) -> xcb::ProtocolResult<()>
    where
        R: RequestWithoutReply,
    {
        self.connection.send_and_check_request(request)
    }

    /// Returns the next queued event without blocking.
    ///
    /// # Errors
    /// Returns a connection or protocol error encountered while polling.
    pub fn poll_for_event(&self) -> xcb::Result<Option<xcb::Event>> {
        self.connection.poll_for_event()
    }

    /// Writes all buffered requests to the X server.
    ///
    /// # Errors
    /// Returns an error if the connection fails while flushing.
    pub fn flush(&self) -> xcb::ConnResult<()> {
        self.connection.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_geometry_starts_at_origin() {
        assert_eq!(
            ScreenGeometry::root(1920, 1080),
            ScreenGeometry {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }
        );
    }

    #[test]
    fn extension_data_keeps_protocol_bases() {
        let data = ExtensionData {
            ext: Extension::RandR,
            major_opcode: 140,
            first_event: 89,
            first_error: 147,
        };

        assert_eq!(
            ExtensionInfo::from(data),
            ExtensionInfo {
                major_opcode: 140,
                first_event: 89,
                first_error: 147,
            }
        );
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY"]
    fn connects_to_live_display() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        assert_ne!(x11.geometry().width, 0);
        assert_ne!(x11.geometry().height, 0);
        x11.check_connection().expect("healthy X connection");
    }
}
