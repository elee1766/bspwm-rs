use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Action {
    #[default]
    Run,
    Help,
    Version,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Arguments {
    action: Action,
    config_path: Option<PathBuf>,
    state_path: Option<PathBuf>,
    inherited_listener: Option<RawFd>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParseError {
    MissingValue(char),
    UnknownOption(char),
    UnexpectedArgument(OsString),
    InvalidFileDescriptor(OsString),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(option) => {
                write!(formatter, "option -{option} requires an argument")
            }
            Self::UnknownOption(option) => write!(formatter, "unknown option: -{option}"),
            Self::UnexpectedArgument(value) => {
                write!(
                    formatter,
                    "unexpected argument: {}",
                    value.to_string_lossy()
                )
            }
            Self::InvalidFileDescriptor(value) => write!(
                formatter,
                "invalid inherited socket descriptor: {}",
                value.to_string_lossy()
            ),
        }
    }
}

fn parse_fd(value: &OsStr) -> Result<RawFd, ParseError> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| ParseError::InvalidFileDescriptor(value.to_os_string()))
}

fn parse_arguments(arguments: &[OsString]) -> Result<Arguments, ParseError> {
    let mut parsed = Arguments::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let bytes = argument.as_os_str().as_bytes();
        if bytes == b"--" {
            if let Some(value) = arguments.get(index + 1) {
                return Err(ParseError::UnexpectedArgument(value.clone()));
            }
            break;
        }
        if bytes == b"--help" {
            parsed.action = Action::Help;
            index += 1;
            continue;
        }
        if bytes == b"--version" {
            parsed.action = Action::Version;
            index += 1;
            continue;
        }
        if bytes.first() != Some(&b'-') || bytes.len() == 1 {
            return Err(ParseError::UnexpectedArgument(argument.clone()));
        }

        let mut option_index = 1;
        while option_index < bytes.len() {
            let option = bytes[option_index];
            match option {
                b'h' => parsed.action = Action::Help,
                b'v' => parsed.action = Action::Version,
                b'c' | b's' | b'o' => {
                    let value = if option_index + 1 < bytes.len() {
                        OsString::from(OsStr::from_bytes(&bytes[option_index + 1..]))
                    } else {
                        index += 1;
                        arguments
                            .get(index)
                            .cloned()
                            .ok_or(ParseError::MissingValue(char::from(option)))?
                    };
                    if option == b'o' {
                        parsed.inherited_listener = Some(parse_fd(&value)?);
                        break;
                    }
                    let path = PathBuf::from(value);
                    if option == b'c' {
                        parsed.config_path = Some(path);
                    } else {
                        parsed.state_path = Some(path);
                    }
                    break;
                }
                _ => return Err(ParseError::UnknownOption(char::from(option))),
            }
            option_index += 1;
        }
        index += 1;
    }
    Ok(parsed)
}

fn main() {
    const FAILURE: i32 = 1;

    let original_args: Vec<OsString> = std::env::args_os().collect();
    let arguments = match parse_arguments(&original_args[1..]) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(FAILURE);
        }
    };
    match arguments.action {
        Action::Help => {
            println!("bspwm-rs [-h|-v|-c CONFIG_PATH]");
            return;
        }
        Action::Version => {
            println!("0.9.12");
            return;
        }
        Action::Run => {}
    }

    let config_path = match bspwm::runtime::resolve_config_path(
        arguments.config_path.as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    ) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(FAILURE);
        }
    };
    let Some(socket_path) = bspwm::common::socket_path_from_env() else {
        eprintln!("Failed to determine the socket path.");
        std::process::exit(FAILURE);
    };
    let options = bspwm::runtime::RuntimeOptions {
        config_path,
        state_path: arguments.state_path,
        inherited_listener: arguments.inherited_listener,
        original_args,
    };
    let mut inherited_fds = bspwm::runtime::InheritedFds::from_env();
    match bspwm::entrypoint::run(&options, &socket_path, &mut inherited_fds) {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(FAILURE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_data_and_inherited_descriptors() {
        let input = ["-vc", "/tmp/rc", "-s/tmp/state", "-o", "12"].map(OsString::from);
        let arguments = parse_arguments(&input).unwrap();
        assert_eq!(arguments.action, Action::Version);
        assert_eq!(arguments.config_path, Some(PathBuf::from("/tmp/rc")));
        assert_eq!(arguments.state_path, Some(PathBuf::from("/tmp/state")));
        assert_eq!(arguments.inherited_listener, Some(12));
        assert_eq!(
            parse_arguments(&[OsString::from("-o"), OsString::from("bad")]),
            Err(ParseError::InvalidFileDescriptor(OsString::from("bad")))
        );
    }
}
