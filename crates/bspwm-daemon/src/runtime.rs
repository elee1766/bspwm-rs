#![allow(clippy::cast_possible_truncation, clippy::missing_errors_doc)]

//! Daemon lifecycle, transport, and X11 ownership primitives.
//!
//! Command registration deliberately lives outside this module. A runtime only
//! needs an application that implements [`MessageHandler`] and the lifecycle
//! hooks in [`RuntimeApp`].

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::fd::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use listenfd::ListenFd;
use mio::unix::SourceFd;
use mio::{Events, Interest, Token};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use signal_hook::consts::signal::{SIGCHLD, SIGHUP, SIGINT, SIGPIPE, SIGTERM};
use signal_hook::flag;
use xcb::x;

use bspwm_ipc::{BUFFER_SIZE, FAILURE_MESSAGE};
pub use bspwm_ipc::{SocketListener, UnixResponse, receive_request};

use crate::common::state_path_from_env;
use crate::ewmh;
use crate::messages::{
    MessageControl, MessageHandler, MessageOutcome, Subscription, handle_message,
};
use crate::state::DaemonState;
use crate::x11::{ConnectError, X11};

pub const WM_NAME: &str = "bspwm";
pub const CONFIG_NAME: &str = "bspwmrc";
pub const DEFAULT_IDLE_INTERVAL: Duration = Duration::from_millis(10);
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const ROOT_EVENT_MASK: x::EventMask = x::EventMask::SUBSTRUCTURE_REDIRECT
    .union(x::EventMask::SUBSTRUCTURE_NOTIFY)
    .union(x::EventMask::STRUCTURE_NOTIFY)
    .union(x::EventMask::BUTTON_PRESS)
    .union(x::EventMask::PROPERTY_CHANGE)
    .union(x::EventMask::FOCUS_CHANGE);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeOptions {
    pub config_path: PathBuf,
    pub state_path: Option<PathBuf>,
    pub inherited_listener: Option<RawFd>,
    pub original_args: Vec<OsString>,
}

impl RuntimeOptions {
    #[must_use]
    pub const fn run_level(&self) -> u8 {
        (self.state_path.is_some() as u8) | ((self.inherited_listener.is_some() as u8) << 1)
    }
}

/// Safe ownership gateway for descriptors deliberately exported by a prior exec.
pub struct InheritedFds {
    first_fd: RawFd,
    descriptors: ListenFd,
}

impl InheritedFds {
    #[must_use]
    pub fn from_env() -> Self {
        let first_fd = env::var("LISTEN_FDS_FIRST_FD")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(3);
        Self {
            first_fd,
            descriptors: ListenFd::from_env(),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            first_fd: 3,
            descriptors: ListenFd::empty(),
        }
    }

    fn index(&self, fd: RawFd) -> io::Result<usize> {
        let offset = fd.checked_sub(self.first_fd).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor is outside the inherited table",
            )
        })?;
        usize::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid descriptor index"))
    }

    /// Closes an inherited descriptor by number.
    ///
    /// Descriptor numbers reaching this come from the restart state file, so a
    /// stale or hand-edited file could otherwise name an unrelated descriptor
    /// such as stdout. Only numbers present in the validated `LISTEN_FDS` table
    /// are accepted, and the descriptor is taken from that table so it cannot be
    /// closed twice.
    ///
    /// # Errors
    /// Returns an error when `fd` is outside the inherited table, was already
    /// taken, or cannot be closed.
    pub fn close_inherited(&mut self, fd: RawFd) -> io::Result<()> {
        let index = self.index(fd)?;
        let raw = self.descriptors.take_raw_fd(index)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "subscriber descriptor was not inherited",
            )
        })?;
        nix::unistd::close(raw).map_err(io::Error::from)
    }

    pub fn take_unix_listener(&mut self, fd: RawFd) -> io::Result<UnixListener> {
        let index = self.index(fd)?;
        self.descriptors.take_unix_listener(index)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "listener descriptor was not inherited",
            )
        })
    }

    pub fn take_unix_stream(&mut self, fd: RawFd) -> io::Result<UnixStream> {
        let index = self.index(fd)?;
        self.descriptors
            .take_custom(
                index,
                nix::libc::AF_UNIX,
                nix::libc::SOCK_STREAM,
                "unix stream",
            )?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "subscriber descriptor was not inherited",
                )
            })
    }
}

struct LifecycleSignals {
    terminate: Arc<AtomicBool>,
    child: Arc<AtomicBool>,
    _pipe: Arc<AtomicBool>,
}

impl LifecycleSignals {
    fn install() -> io::Result<Self> {
        let terminate = Arc::new(AtomicBool::new(false));
        for signal in [SIGINT, SIGHUP, SIGTERM] {
            flag::register(signal, Arc::clone(&terminate))?;
        }
        let child = Arc::new(AtomicBool::new(false));
        flag::register(SIGCHLD, Arc::clone(&child))?;
        // A handler, rather than SIG_IGN, is reset on exec and turns writes into EPIPE.
        let pipe = Arc::new(AtomicBool::new(false));
        flag::register(SIGPIPE, Arc::clone(&pipe))?;
        Ok(Self {
            terminate,
            child,
            _pipe: pipe,
        })
    }

    fn termination_requested(&self) -> bool {
        self.terminate.load(Ordering::Relaxed)
    }

    fn reap_children(&self) {
        if !self.child.swap(false, Ordering::Relaxed) {
            return;
        }
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}

/// Resolves bspwm's default configuration location without reading global state.
pub fn resolve_config_path(
    explicit: Option<&Path>,
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, RuntimeError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = xdg_config_home.filter(|path| !path.is_empty()) {
        return Ok(Path::new(path).join(WM_NAME).join(CONFIG_NAME));
    }
    let home = home.ok_or(RuntimeError::MissingHome)?;
    Ok(Path::new(home)
        .join(".config")
        .join(WM_NAME)
        .join(CONFIG_NAME))
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("HOME is not set; cannot resolve bspwmrc")]
    MissingHome,
    #[error("cannot resolve the bspwm restart state path")]
    MissingStatePath,
    #[error("another window manager is already running")]
    AnotherWindowManager,
    #[error("X11 runtime error: {0}")]
    X11(String),
    #[error("X11 runtime error: {0}")]
    Connect(#[from] ConnectError),
    #[error("X11 runtime error: {0}")]
    Xcb(#[from] xcb::Error),
    #[error("X11 runtime error: {0}")]
    Protocol(#[from] xcb::ProtocolError),
    #[error("X11 runtime error: {0}")]
    Connection(#[from] xcb::ConnError),
}

pub fn handle_stream<H: MessageHandler>(
    mut stream: UnixStream,
    handler: &mut H,
    exit_status: i32,
    request_limit: usize,
    read_timeout: Duration,
) -> io::Result<(MessageOutcome, UnixResponse)> {
    stream.set_read_timeout(Some(read_timeout))?;
    let request = match receive_request(&mut stream, request_limit) {
        Ok(request) => request,
        Err(error) => {
            write_request_error(stream, &error)?;
            return Err(error);
        }
    };
    let mut response = UnixResponse::new(stream);
    let result = handle_message(&request, exit_status, handler, &mut response)?;
    Ok((result, response))
}

/// Hooks supplied by the state/command layer to the transport runtime.
pub trait RuntimeApp: MessageHandler {
    fn state(&self) -> &DaemonState;

    fn setup(&mut self, _x11: &X11) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn restore_state(&mut self, _path: &Path, _x11: &X11) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn restore_inherited_subscribers(
        &mut self,
        _fds: &mut InheritedFds,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn run_config(&mut self, _path: &Path, _run_level: u8) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn handle_event(
        &mut self,
        event: xcb::Result<xcb::Event>,
        x11: &X11,
    ) -> Result<(), RuntimeError>;

    /// Completes X-backed work queued by the command that just ran.
    fn execute_pending_effects(&mut self, _x11: &X11) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// Polls asynchronous application work without blocking the event loop.
    fn poll(&mut self, _x11: &X11) -> Result<bool, RuntimeError> {
        Ok(false)
    }

    fn running(&self) -> bool;

    fn exit_status(&self) -> i32;

    fn set_exit_status(&mut self, _status: i32) {}

    fn request_stop(&mut self) {}

    /// Transfers a subscription response to the application.
    fn retain_response(
        &mut self,
        _response: UnixResponse,
        subscription: Subscription,
    ) -> Result<(), RuntimeError> {
        if let Some(path) = subscription.fifo_path {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    fn prune_dead_subscribers(&mut self) {}

    fn client_error(&mut self, _error: &io::Error) {}

    fn cleanup(&mut self, _x11: &X11) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn restarting(&self) -> bool {
        false
    }

    fn write_restart_state(&mut self, _path: &Path) -> Result<Vec<RawFd>, RuntimeError> {
        Ok(Vec::new())
    }

    fn restart_failed(&mut self) {}
}

const X11_SOURCE: Token = Token(0);
const LISTENER_SOURCE: Token = Token(1);

/// Which of the runtime's two input descriptors a wait reported as readable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Ready {
    x11: bool,
    listener: bool,
}

impl Ready {
    const ALL: Self = Self {
        x11: true,
        listener: true,
    };
    const NONE: Self = Self {
        x11: false,
        listener: false,
    };
}

/// Blocks the event loop on the X connection and the IPC listener.
///
/// Upstream bspwm parks in `select()` on exactly these two descriptors; this is
/// the same idea expressed with [`mio`]. [`SourceFd`] borrows a bare [`RawFd`],
/// which is what lets the runtime register descriptors it does not own without
/// any `unsafe` in this crate.
///
/// Registrations are edge triggered, so a source only reports again once
/// something new arrives on it. Every caller must therefore read each source
/// until it is empty before waiting again, which is what [`Runtime::run`]
/// documents and does.
struct InputPoller {
    poll: mio::Poll,
    events: Events,
}

impl InputPoller {
    fn new(x11_fd: RawFd, listener_fd: RawFd) -> io::Result<Self> {
        let poll = mio::Poll::new()?;
        let registry = poll.registry();
        registry.register(&mut SourceFd(&x11_fd), X11_SOURCE, Interest::READABLE)?;
        registry.register(
            &mut SourceFd(&listener_fd),
            LISTENER_SOURCE,
            Interest::READABLE,
        )?;
        Ok(Self {
            poll,
            events: Events::with_capacity(2),
        })
    }

    /// Sleeps until a descriptor is readable, `timeout` elapses, or a signal
    /// arrives.
    ///
    /// Callers must have drained everything both descriptors can offer first;
    /// see [`Runtime::run`].
    fn wait(&mut self, timeout: Duration) -> io::Result<Ready> {
        match self.poll.poll(&mut self.events, Some(timeout)) {
            Ok(()) => {}
            // `epoll_wait` is never restarted after a handler runs, so a
            // termination or `SIGCHLD` request surfaces here. Treat it as an
            // ordinary wakeup: the loop re-checks the signal flags, and a
            // conservative readiness answer keeps that cheap.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(Ready::ALL),
            Err(error) => return Err(error),
        }
        let mut ready = Ready::NONE;
        for event in &self.events {
            match event.token() {
                X11_SOURCE => ready.x11 = true,
                LISTENER_SOURCE => ready.listener = true,
                _ => {}
            }
        }
        Ok(ready)
    }
}

pub struct Runtime<A: RuntimeApp> {
    app: A,
    x11: X11,
    listener: SocketListener,
    meta_window: x::Window,
    request_limit: usize,
    read_timeout: Duration,
    idle_interval: Duration,
    cleaned_up: bool,
    signals: LifecycleSignals,
    original_args: Vec<OsString>,
}

impl<A: RuntimeApp> Runtime<A> {
    pub fn start(
        mut app: A,
        options: &RuntimeOptions,
        display_name: Option<&str>,
        socket_path: &Path,
        inherited_fds: &mut InheritedFds,
    ) -> Result<Self, RuntimeError> {
        let signals = LifecycleSignals::install()?;
        let x11 = X11::connect(display_name)?;
        claim_root(&x11)?;
        let meta_window = create_meta_window(&x11)?;
        if let Err(error) = app.setup(&x11) {
            let _ = x11.send_and_check_request(&x::DestroyWindow {
                window: meta_window,
            });
            return Err(error);
        }

        if let Some(path) = options.state_path.as_deref() {
            app.restore_state(path, &x11)?;
            app.restore_inherited_subscribers(inherited_fds)?;
        }

        if let Err(error) = setup_ewmh(&x11, meta_window, app.state()) {
            let _ = x11.send_and_check_request(&x::DestroyWindow {
                window: meta_window,
            });
            return Err(error);
        }

        let listener = if let Some(fd) = options.inherited_listener {
            SocketListener::inherited(inherited_fds.take_unix_listener(fd)?, socket_path)?
        } else {
            SocketListener::bind(socket_path)?
        };
        // Upstream starts serving even when bspwmrc is missing or cannot execute.
        let _ = app.run_config(&options.config_path, options.run_level());
        if let Some(path) = options.state_path.as_deref() {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }

        Ok(Self {
            app,
            x11,
            listener,
            meta_window,
            request_limit: BUFFER_SIZE,
            read_timeout: DEFAULT_READ_TIMEOUT,
            idle_interval: DEFAULT_IDLE_INTERVAL,
            cleaned_up: false,
            signals,
            original_args: options.original_args.clone(),
        })
    }

    #[must_use]
    pub const fn app(&self) -> &A {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut A {
        &mut self.app
    }

    #[must_use]
    pub const fn x11(&self) -> &X11 {
        &self.x11
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.listener.path()
    }

    /// Runs the event loop until the application stops or a signal arrives.
    ///
    /// Each pass ends parked in `InputPoller::wait` rather than sleeping, so
    /// a key press, a button press, a map request, or a `bspc` connection is
    /// picked up as soon as the kernel has it. The wait keeps `idle_interval`
    /// as a timeout because rule subprocesses, dead subscribers, and orphaned
    /// children are still discovered by polling.
    pub fn run(mut self) -> Result<i32, RuntimeError> {
        let mut poller = InputPoller::new(self.x11.raw_fd(), self.listener.raw_fd())?;
        // The first pass has no readiness answer to go on, so it sweeps both.
        let mut ready = Ready::ALL;
        while self.app.running() {
            if self.signals.termination_requested() {
                self.app.request_stop();
                break;
            }
            self.x11.flush()?;
            let mut did_work = self.app.poll(&self.x11)?;
            // A wait that reported nothing on the listener leaves the accept
            // queue exactly as the previous pass drained it: empty.
            if did_work || ready.listener {
                did_work |= self.accept_requests()?;
            }
            loop {
                // An asynchronous protocol error is a queue entry like any
                // other: hand it to the dispatcher, which drops the unavoidable
                // `BadWindow` races and reports the rest. Only a connection
                // error is fatal.
                let item = match self.x11.poll_for_event() {
                    Ok(None) => break,
                    Ok(Some(event)) => Ok(event),
                    Err(xcb::Error::Connection(error)) => return Err(error.into()),
                    Err(error @ xcb::Error::Protocol(_)) => Err(error),
                };
                did_work = true;
                match self.app.handle_event(item, &self.x11) {
                    Ok(()) => {}
                    // Protocol errors from event handlers are non-fatal.
                    // Upstream bspwm continues after all X errors; crashing
                    // over a rejected request tears down every client's
                    // session unnecessarily.
                    Err(
                        RuntimeError::Protocol(ref error)
                        | RuntimeError::Xcb(xcb::Error::Protocol(ref error)),
                    ) => {
                        log::warn!("protocol error during event handling: {error}");
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Err(error) = self.x11.check_connection() {
                self.app.request_stop();
                return Err(error.into());
            }
            self.app.prune_dead_subscribers();
            // The application polls its owned children first; this only collects leftovers.
            self.signals.reap_children();
            ready = if did_work {
                // Handling work can queue an event or a connection behind our
                // back, so the next pass re-examines both sources instead of
                // trusting a readiness answer taken before that work ran.
                Ready::ALL
            } else {
                // Safe to park: the drain above ran unconditionally and only
                // stopped once `poll_for_event` returned `None`, which means
                // libxcb holds no parsed event and has read the socket to
                // `EAGAIN`. Flushing first stops a request stranded in the
                // output buffer from deadlocking us against a server that is
                // waiting for it.
                self.x11.flush()?;
                poller.wait(self.idle_interval)?
            };
        }
        let status = self.app.exit_status();
        if self.app.restarting() {
            return self.restart();
        }
        self.cleanup()?;
        Ok(status)
    }

    fn restart(&mut self) -> Result<i32, RuntimeError> {
        let state_path = state_path_from_env().ok_or(RuntimeError::MissingStatePath)?;
        let mut inherited = self.app.write_restart_state(&state_path)?;
        self.listener.set_inheritable()?;
        inherited.push(self.listener.raw_fd());
        self.cleanup()?;

        // Use original argv[0] rather than /proc/self/exe: when the binary is
        // replaced on disk (e.g. `cp` before restart), /proc/self/exe points
        // to the deleted old inode, causing "No such file or directory".
        let executable = self
            .original_args
            .first()
            .cloned()
            .unwrap_or_else(|| env::current_exe().unwrap_or_default().into());
        let arguments = restart_arguments(&self.original_args, &state_path, self.listener.raw_fd());
        let first = inherited.iter().copied().min().unwrap_or(3);
        let last = inherited.iter().copied().max().unwrap_or(first);
        let error = Command::new(executable)
            .args(arguments)
            .env("LISTEN_FDS_FIRST_FD", first.to_string())
            .env("LISTEN_FDS", (last - first + 1).to_string())
            .env_remove("LISTEN_PID")
            .exec();
        self.app.restart_failed();
        Err(error.into())
    }

    fn accept_requests(&mut self) -> Result<bool, RuntimeError> {
        let mut accepted = false;
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(accepted),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            accepted = true;
            let exit_status = self.app.exit_status();
            let result = handle_stream(
                stream,
                &mut self.app,
                exit_status,
                self.request_limit,
                self.read_timeout,
            );
            match result {
                Ok((outcome, response)) => {
                    if let Err(error) = self.app.execute_pending_effects(&self.x11) {
                        if matches!(
                            error,
                            RuntimeError::Protocol(_) | RuntimeError::Xcb(xcb::Error::Protocol(_))
                        ) {
                            log::warn!("protocol error during effects: {error}");
                        } else {
                            if let MessageOutcome::Subscribe(subscription) = &outcome
                                && let Some(path) = &subscription.fifo_path
                            {
                                let _ = fs::remove_file(path);
                            }
                            return Err(error);
                        }
                    }
                    match outcome {
                        MessageOutcome::Close(control) => {
                            if let MessageControl::Quit(status) = control {
                                self.app.set_exit_status(status);
                                self.app.request_stop();
                            }
                        }
                        MessageOutcome::Subscribe(subscription) => {
                            self.app.retain_response(response, subscription)?;
                        }
                    }
                }
                Err(error) => self.app.client_error(&error),
            }
        }
    }

    fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.cleaned_up {
            return Ok(());
        }
        self.app.cleanup(&self.x11)?;
        self.x11.send_and_check_request(&x::DestroyWindow {
            window: self.meta_window,
        })?;
        self.x11.flush()?;
        self.cleaned_up = true;
        Ok(())
    }
}

#[must_use]
pub fn restart_arguments(
    original: &[OsString],
    state_path: &Path,
    listener: RawFd,
) -> Vec<OsString> {
    let end = original
        .iter()
        .position(|argument| argument == "-s")
        .unwrap_or(original.len());
    original
        .iter()
        .skip(1)
        .take(end.saturating_sub(1))
        .cloned()
        .chain([
            OsString::from("-s"),
            state_path.as_os_str().to_os_string(),
            OsString::from("-o"),
            OsString::from(listener.to_string()),
        ])
        .collect()
}

impl<A: RuntimeApp> Drop for Runtime<A> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn claim_root(x11: &X11) -> Result<(), RuntimeError> {
    // Only `BadAccess` means another window manager already holds
    // SubstructureRedirect on the root; anything else is a genuine failure and
    // must not be disguised as a conflict.
    x11.send_and_check_owned_request(&x::ChangeWindowAttributes {
        window: x11.root(),
        value_list: &[x::Cw::EventMask(ROOT_EVENT_MASK)],
    })
    .map_err(|error| match error {
        xcb::ProtocolError::X(x::Error::Access(_), _) => RuntimeError::AnotherWindowManager,
        error => error.into(),
    })
}

fn create_meta_window(x11: &X11) -> Result<x::Window, RuntimeError> {
    let window = x11.connection().generate_id();
    x11.send_and_check_owned_request(&x::CreateWindow {
        depth: x::COPY_FROM_PARENT as u8,
        wid: window,
        parent: x11.root(),
        x: -1,
        y: -1,
        width: 1,
        height: 1,
        border_width: 0,
        class: x::WindowClass::InputOnly,
        visual: x::COPY_FROM_PARENT,
        value_list: &[],
    })?;
    Ok(window)
}

fn setup_ewmh(x11: &X11, meta_window: x::Window, state: &DaemonState) -> Result<(), RuntimeError> {
    ewmh::set_supported(x11, false)?;
    ewmh::set_supporting(x11, meta_window, WM_NAME)?;
    ewmh::update_number_of_desktops(x11, &state.world)?;
    ewmh::update_desktop_names(x11, &state.world)?;
    ewmh::update_desktop_geometry(x11)?;
    ewmh::update_desktop_viewports(x11, &state.world)?;
    ewmh::update_workareas(x11, &state.world)?;
    ewmh::update_current_desktop(x11, &state.world)?;
    ewmh::update_client_desktops(x11, &state.world)?;
    ewmh::update_client_list(x11, &state.world)?;
    ewmh::update_client_stacking_list(x11, &state.stacking_order)?;
    ewmh::update_active_window(x11, &state.world)?;
    Ok(())
}

pub fn write_request_error(stream: UnixStream, error: &io::Error) -> io::Result<()> {
    let mut response = UnixResponse::new(stream);
    response.write_all(&[FAILURE_MESSAGE])?;
    writeln!(response, "{error}")?;
    response.close()
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::Shutdown;
    use std::os::fd::AsRawFd;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::Instant;

    use super::*;
    use crate::messages::Response;

    #[derive(Default)]
    struct TestHandler {
        query_arguments: Vec<Vec<u8>>,
    }

    impl MessageHandler for TestHandler {
        fn dispatch(
            &mut self,
            domain: crate::messages::Domain,
            args: &[&[u8]],
            rsp: &mut dyn Response,
        ) -> io::Result<Option<Subscription>> {
            if domain == crate::messages::Domain::Query {
                self.query_arguments = args.iter().map(|argument| argument.to_vec()).collect();
                rsp.write_all(b"handled")?;
            }
            Ok(
                (domain == crate::messages::Domain::Subscribe).then_some(Subscription {
                    mask: crate::types::SubscriberMask::REPORT,
                    count: -1,
                    fifo_path: None,
                }),
            )
        }
    }

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    fn test_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "bspwm-rs-runtime-{name}-{}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn rebuilds_upstream_restart_arguments() {
        let original = ["bspwm", "-c", "rc", "-s", "old", "-o", "7"].map(OsString::from);
        assert_eq!(
            restart_arguments(&original, Path::new("new-state"), 11),
            ["-c", "rc", "-s", "new-state", "-o", "11"].map(OsString::from)
        );
    }

    #[test]
    #[ignore = "subprocess target for lifecycle_signals_are_deferred_and_sigpipe_safe"]
    fn signal_probe() {
        if env::var_os("BSPWM_SIGNAL_PROBE").is_none() {
            return;
        }
        let signals = LifecycleSignals::install().unwrap();
        let (mut writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        assert_eq!(
            writer.write(b"pipe").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let ready = env::var_os("BSPWM_SIGNAL_READY").unwrap();
        fs::write(ready, b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !signals.termination_requested() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(signals.termination_requested());
    }

    #[test]
    fn lifecycle_signals_are_deferred_and_sigpipe_safe() {
        let ready = test_path("signal-ready");
        let mut child = Command::new(env::current_exe().unwrap())
            .args(["--ignored", "--exact", "runtime::tests::signal_probe"])
            .env("BSPWM_SIGNAL_PROBE", "1")
            .env("BSPWM_SIGNAL_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < ready_deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if !ready.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("signal probe did not become ready before its deadline");
        }
        nix::sys::signal::kill(
            Pid::from_raw(child.id().cast_signed()),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let exit_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= exit_deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signal probe did not stop before its deadline");
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = fs::remove_file(ready);
    }

    #[test]
    fn resolves_explicit_xdg_and_home_config_paths() {
        assert_eq!(
            resolve_config_path(Some(Path::new("custom")), None, None).unwrap(),
            PathBuf::from("custom")
        );
        assert_eq!(
            resolve_config_path(None, Some(OsStr::new("/xdg")), None).unwrap(),
            PathBuf::from("/xdg/bspwm/bspwmrc")
        );
        assert_eq!(
            resolve_config_path(None, None, Some(OsStr::new("/home/me"))).unwrap(),
            PathBuf::from("/home/me/.config/bspwm/bspwmrc")
        );
    }

    #[test]
    fn request_receive_is_bounded() {
        assert_eq!(receive_request(&mut &b"abc\0"[..], 4).unwrap(), b"abc\0");
        assert_eq!(
            receive_request(&mut &b"abcd"[..], 3).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn listener_refuses_live_socket_and_removes_stale_socket() {
        let path = test_path("stale");
        let first = SocketListener::bind(&path).unwrap();
        assert_eq!(
            SocketListener::bind(&path).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        drop(first);

        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        // A concurrent `Command::spawn` on another test thread can `fork`
        // between the bind above and the drop, and the child keeps the
        // listening descriptor open until its own `execve` completes -- during
        // which the stale socket still answers `connect`. Same window as
        // `rule::tests::spawn_script`.
        let deadline = Instant::now() + Duration::from_secs(2);
        let replacement = loop {
            match SocketListener::bind(&path) {
                Ok(listener) => break listener,
                Err(error)
                    if error.kind() == io::ErrorKind::AddrInUse && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("could not rebind the stale socket: {error}"),
            }
        };
        assert!(path.exists());
        drop(replacement);
        assert!(!path.exists());
    }

    #[test]
    fn input_poller_wakes_on_readiness_and_still_honours_its_timeout() {
        let (x11_source, mut x11_peer) = UnixStream::pair().unwrap();
        let (listener_source, mut listener_peer) = UnixStream::pair().unwrap();
        let mut poller =
            InputPoller::new(x11_source.as_raw_fd(), listener_source.as_raw_fd()).unwrap();

        // Rule subprocesses, dead subscribers, and orphaned children are still
        // found by polling, so an idle wait has to come back on its own.
        let started = Instant::now();
        assert_eq!(poller.wait(Duration::from_millis(60)).unwrap(), Ready::NONE);
        assert!(started.elapsed() >= Duration::from_millis(50));

        // A readable source must end the wait immediately, and be reported as
        // itself rather than as the other source.
        x11_peer.write_all(b"e").unwrap();
        let started = Instant::now();
        assert_eq!(
            poller.wait(Duration::from_secs(30)).unwrap(),
            Ready {
                x11: true,
                listener: false
            }
        );
        assert!(started.elapsed() < Duration::from_secs(1));

        listener_peer.write_all(b"c").unwrap();
        let started = Instant::now();
        assert!(poller.wait(Duration::from_secs(30)).unwrap().listener);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn peer_disconnect_check_never_blocks_on_a_silent_client() {
        let (server, client) = UnixStream::pair().unwrap();
        let mut response = UnixResponse::new(server);
        // The socket is blocking and has nothing to read; the check still has
        // to return, and has to call the subscriber alive.
        assert!(!response.peer_disconnected());
        drop(client);
        assert!(await_peer_disconnect(&mut response));
    }

    /// Waits for a closed peer to be reported as gone.
    ///
    /// `POLLHUP` needs *every* descriptor for the peer end to be closed. A
    /// `Command::spawn` on another test thread can `fork` while this socket is
    /// open, and the child holds a duplicate until its own `execve` completes,
    /// so the hangup can lag the `drop` by a moment.
    fn await_peer_disconnect(response: &mut UnixResponse) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if response.peer_disconnected() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn a_half_closed_subscriber_is_still_alive() {
        let (server, client) = UnixStream::pair().unwrap();
        let mut response = UnixResponse::new(server);
        // `bspc` shuts down its write side once the request is sent. That
        // leaves the subscriber at end-of-file for reads forever, but it can
        // still receive every record we send it. Unlike the hangup below, this
        // is unaffected by a forked child holding a duplicate descriptor.
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(!response.peer_disconnected());
        response.write_all(b"node_add\n").unwrap();
        response.flush().unwrap();
        drop(client);
        assert!(await_peer_disconnect(&mut response));
    }

    #[test]
    fn response_close_finishes_socket_output() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let mut response = UnixResponse::new(server);
        response.write_all(b"answer").unwrap();
        response.close().unwrap();
        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        assert_eq!(output, b"answer");
        assert!(response.is_closed());
    }

    #[test]
    fn socket_requests_dispatch_complete_and_fragmented_arguments_without_client_half_close() {
        let cases: &[(&str, &[&[u8]])] = &[
            ("complete", &[b"query\0-N\0"]),
            ("fragmented", &[b"que", b"ry\0-N", b"\0"]),
        ];
        for &(label, chunks) in cases {
            let (server, mut client) = UnixStream::pair().unwrap();
            let server = thread::spawn(move || {
                let mut handler = TestHandler::default();
                let (outcome, response) =
                    handle_stream(server, &mut handler, 0, BUFFER_SIZE, DEFAULT_READ_TIMEOUT)
                        .unwrap();
                (outcome, response, handler)
            });

            for (index, chunk) in chunks.iter().enumerate() {
                client.write_all(chunk).unwrap();
                if index + 1 < chunks.len() {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            let mut output = Vec::new();
            client.read_to_end(&mut output).unwrap();

            let (outcome, response, handler) = server.join().unwrap();
            assert_eq!(
                outcome,
                MessageOutcome::Close(MessageControl::Continue),
                "{label} outcome"
            );
            assert_eq!(
                handler.query_arguments,
                [b"-N".to_vec()],
                "{label} arguments"
            );
            assert!(response.is_closed(), "{label} response");
            assert_eq!(output, b"handled", "{label} output");
        }
    }

    #[test]
    fn oversized_socket_request_gets_failure_response() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(b"1234").unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        let error = handle_stream(
            server,
            &mut TestHandler::default(),
            0,
            3,
            DEFAULT_READ_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        assert_eq!(output.first(), Some(&FAILURE_MESSAGE));
        assert!(String::from_utf8_lossy(&output[1..]).contains("3-byte limit"));
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY and no running window manager"]
    fn claims_root_on_live_x_server() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        claim_root(&x11).expect("claim root event mask");
        x11.check_connection().expect("healthy X connection");
    }

    #[test]
    #[ignore = "requires a live X server selected by DISPLAY and mutates root EWMH properties"]
    fn performs_live_ewmh_setup_sequence() {
        let x11 = X11::connect(None).expect("connect to DISPLAY");
        let meta = create_meta_window(&x11).expect("create metadata window");
        setup_ewmh(&x11, meta, &DaemonState::default()).expect("set EWMH properties");
        x11.send_and_check_request(&x::DestroyWindow { window: meta })
            .expect("destroy metadata window");
    }
}
