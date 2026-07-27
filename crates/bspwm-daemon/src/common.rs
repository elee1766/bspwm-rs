use std::env;
use std::path::PathBuf;

pub use bspwm_client::{
    Display, FAILURE_MESSAGE, SOCKET_ENV_VAR, SOCKET_PATH_TEMPLATE, parse_display,
    socket_path_from_env,
};

pub const STATE_PATH_TEMPLATE: &str = "/tmp/bspwm{host}_{display}_{screen}-state";

#[must_use]
pub fn state_path_from_env() -> Option<PathBuf> {
    let display = parse_display(&env::var("DISPLAY").ok()?)?;
    Some(PathBuf::from(
        STATE_PATH_TEMPLATE
            .replace("{host}", &display.host)
            .replace("{display}", &display.display.to_string())
            .replace("{screen}", &display.screen.to_string()),
    ))
}
