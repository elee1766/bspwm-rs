//! Fixtures shared by the unit tests of the daemon submodules.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::manage::ClientInitial;
use super::{DaemonApp, WindowLocation};
use crate::messages::Response;
use crate::rule::RuleConsequence;
use crate::settings::Settings;
use crate::state::{CommandEffect, DaemonState};
use crate::tree::{NodeId, SizeHints};
use crate::types::Rectangle;
use crate::world::{DesktopId, MonitorId};

#[derive(Default)]
pub(super) struct TestResponse(pub(super) Vec<u8>);

impl Write for TestResponse {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Response for TestResponse {
    fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

pub(super) fn test_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "bspwm-rs-daemon-{name}-{}-{}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(super) fn app_with_desktop() -> (DaemonApp, MonitorId, DesktopId) {
    let settings = Settings::default();
    let mut state = DaemonState::default();
    let monitor =
        state
            .world
            .create_monitor(1, Some("monitor"), Rectangle::new(0, 0, 100, 80), &settings);
    let desktop = state.world.create_desktop(2, Some("I"), &settings);
    assert!(state.world.add_desktop(monitor, desktop));
    (DaemonApp::new(state), monitor, desktop)
}

pub(super) fn manage_window(app: &mut DaemonApp, window: u32) -> (MonitorId, DesktopId, NodeId) {
    app.manage_window_with_initial(
        window,
        &RuleConsequence::default(),
        Rectangle::new(0, 0, 1, 1),
        SizeHints::default(),
        &ClientInitial::default(),
        !window,
    )
    .unwrap()
}

pub(super) fn manage_window_with(
    app: &mut DaemonApp,
    window: u32,
    consequence: &RuleConsequence,
    initial_rectangle: Rectangle,
    size_hints: SizeHints,
    internal_xid: u32,
) -> Option<WindowLocation> {
    app.manage_window_with_initial(
        window,
        consequence,
        initial_rectangle,
        size_hints,
        &ClientInitial::default(),
        internal_xid,
    )
}

pub(super) fn subscribe_socket(app: &mut DaemonApp, request: &[u8]) -> UnixStream {
    let (server, mut client) = UnixStream::pair().unwrap();
    client.write_all(request).unwrap();
    let (outcome, response) = crate::runtime::handle_stream(
        server,
        app,
        0,
        crate::bspc::BUFFER_SIZE,
        Duration::from_secs(1),
    )
    .unwrap();
    let crate::messages::MessageOutcome::Subscribe(subscription) = outcome else {
        panic!("expected subscription outcome");
    };
    app.retain_response(response, subscription).unwrap();
    client
}

pub(super) fn read_available(stream: &mut UnixStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 256];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            // Another test spawning a child delivers SIGCHLD, which interrupts
            // this blocking read. That says nothing about the subscription.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => panic!("subscription read failed: {error}"),
        }
    }
    bytes
}

pub(super) fn emit_pending_broadcasts(app: &mut DaemonApp) {
    for effect in std::mem::take(&mut app.state.pending_effects) {
        if let CommandEffect::Broadcast {
            mask,
            status,
            report,
        } = effect
        {
            app.broadcast_status(mask, status.as_bytes());
            if report {
                app.broadcast_report();
            }
        }
    }
}
