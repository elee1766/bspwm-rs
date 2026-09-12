use std::env;
use std::path::PathBuf;

pub use bspwm_ipc::{
    Display, FAILURE_MESSAGE, SOCKET_ENV_VAR, SOCKET_PATH_TEMPLATE, expand_path_template,
    parse_display, socket_path_from_env,
};

pub const STATE_PATH_TEMPLATE: &str = "/tmp/bspwm{host}_{display}_{screen}-state";
pub const STATE_FILE_TEMPLATE: &str = "bspwm{host}_{display}_{screen}-state";

/// Resolves the restart state path, preferring the user-private runtime
/// directory over the shared `/tmp` fallback.
///
/// `XDG_RUNTIME_DIR` is owned by and readable only to the user, so the state
/// dump is not exposed to, or pre-createable by, other local users. The `/tmp`
/// template remains as a fallback for sessions without it; writes there are
/// still symlink-safe.
#[must_use]
pub fn state_path_from_env() -> Option<PathBuf> {
    let display = parse_display(&env::var("DISPLAY").ok()?)?;
    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        let file = expand_path_template(STATE_FILE_TEMPLATE, &display);
        return Some(PathBuf::from(runtime_dir).join(file));
    }
    Some(expand_path_template(STATE_PATH_TEMPLATE, &display))
}
