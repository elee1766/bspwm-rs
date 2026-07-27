use std::path::Path;

use crate::daemon::DaemonApp;
use crate::runtime::{InheritedFds, Runtime, RuntimeError, RuntimeOptions};

/// Runs the daemon from already parsed process inputs.
///
/// # Errors
///
/// Returns an error when runtime initialization or the event loop fails.
pub fn run(
    options: &RuntimeOptions,
    socket_path: &Path,
    inherited_fds: &mut InheritedFds,
) -> Result<i32, RuntimeError> {
    Runtime::start(
        DaemonApp::default(),
        options,
        None,
        socket_path,
        inherited_fds,
    )
    .and_then(Runtime::run)
}
