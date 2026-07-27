use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub const BUFFER_SIZE: usize = 8192;
pub const FAILURE_MESSAGE: u8 = 0x07;
pub const SOCKET_ENV_VAR: &str = "BSPWM_SOCKET";
pub const SOCKET_PATH_TEMPLATE: &str = "/tmp/bspwm{host}_{display}_{screen}-socket";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Display {
    pub host: String,
    pub display: i32,
    pub screen: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Response {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub failed: bool,
}

#[must_use]
pub fn parse_display(value: &str) -> Option<Display> {
    let value = value.rsplit_once('/').map_or(value, |(_, display)| display);
    let (host, numbers) = value.rsplit_once(':')?;
    let (display, screen) = numbers
        .split_once('.')
        .map_or((numbers, "0"), |parts| parts);
    Some(Display {
        host: host.into(),
        display: display.parse().ok()?,
        screen: screen.parse().ok()?,
    })
}

#[must_use]
pub fn socket_path_from_env() -> Option<PathBuf> {
    if let Some(path) = env::var_os(SOCKET_ENV_VAR) {
        return Some(PathBuf::from(path));
    }
    let display = parse_display(&env::var("DISPLAY").ok()?)?;
    Some(PathBuf::from(
        SOCKET_PATH_TEMPLATE
            .replace("{host}", &display.host)
            .replace("{display}", &display.display.to_string())
            .replace("{screen}", &display.screen.to_string()),
    ))
}

#[must_use]
pub fn make_message<'a>(args: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut message = Vec::with_capacity(BUFFER_SIZE);
    for argument in args {
        let remaining = BUFFER_SIZE.saturating_sub(message.len());
        if remaining == 0 {
            break;
        }
        let bytes = argument.as_bytes();
        let copy_length = bytes.len().min(remaining.saturating_sub(1));
        message.extend_from_slice(&bytes[..copy_length]);
        if message.len() < BUFFER_SIZE {
            message.push(0);
        }
    }
    message
}

#[allow(clippy::missing_errors_doc)]
pub fn send_message(path: &Path, args: &[String]) -> io::Result<Response> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let failed = send_message_stream(path, args, &mut stdout, &mut stderr)?;
    Ok(Response {
        stdout,
        stderr,
        failed,
    })
}

#[allow(clippy::missing_errors_doc)]
pub fn send_message_stream(
    path: &Path,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<bool> {
    let mut stream = UnixStream::connect(path)?;
    let message = make_message(args.iter().map(String::as_str));
    stream.write_all(&message)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    stream_response(&mut stream, stdout, stderr)
}

#[allow(clippy::missing_errors_doc)]
pub fn read_response(reader: &mut impl Read) -> io::Result<Response> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let failed = stream_response(reader, &mut stdout, &mut stderr)?;
    Ok(Response {
        stdout,
        stderr,
        failed,
    })
}

#[allow(clippy::missing_errors_doc)]
pub fn stream_response(
    reader: &mut impl Read,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<bool> {
    let mut failed = false;
    let mut buffer = [0_u8; BUFFER_SIZE - 1];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(failed);
        }
        let chunk = &buffer[..count];
        if chunk[0] == FAILURE_MESSAGE {
            failed = true;
            stderr.write_all(&chunk[1..])?;
            stderr.flush()?;
        } else if let Err(error) = stdout.write_all(chunk).and_then(|()| stdout.flush()) {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return Ok(failed);
            }
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct FlushWriter {
        output: Vec<u8>,
        flushed: Rc<Cell<bool>>,
    }

    impl Write for FlushWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed.set(true);
            Ok(())
        }
    }

    struct SubscriptionReader {
        read_count: usize,
        first_chunk_flushed: Rc<Cell<bool>>,
    }

    impl Read for SubscriptionReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.read_count += 1;
            match self.read_count {
                1 => {
                    buffer[..6].copy_from_slice(b"first\n");
                    Ok(6)
                }
                2 => {
                    assert!(self.first_chunk_flushed.get());
                    buffer[..7].copy_from_slice(b"second\n");
                    Ok(7)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn parses_local_remote_and_protocol_display_names() {
        let cases = [
            (
                "local",
                ":0",
                Some(Display {
                    host: String::new(),
                    display: 0,
                    screen: 0,
                }),
            ),
            (
                "remote with screen",
                "host:2.3",
                Some(Display {
                    host: "host".into(),
                    display: 2,
                    screen: 3,
                }),
            ),
            (
                "protocol prefix",
                "tcp/host:2",
                Some(Display {
                    host: "host".into(),
                    display: 2,
                    screen: 0,
                }),
            ),
            ("invalid", "invalid", None),
        ];
        for (label, input, expected) in cases {
            assert_eq!(parse_display(input), expected, "{label}");
        }
    }

    #[test]
    fn message_is_nul_separated_and_terminated() {
        assert_eq!(make_message(["query", "-M"]), b"query\0-M\0");
    }

    #[test]
    fn response_routes_failure_chunks_to_stderr() {
        let cases = [
            (
                "success chunk",
                &b"ok\n\x07bad\n"[..],
                Response {
                    stdout: b"ok\n\x07bad\n".to_vec(),
                    stderr: Vec::new(),
                    failed: false,
                },
            ),
            (
                "failure chunk",
                &b"\x07bad\n"[..],
                Response {
                    stdout: Vec::new(),
                    stderr: b"bad\n".to_vec(),
                    failed: true,
                },
            ),
        ];
        for (label, mut input, expected) in cases {
            assert_eq!(read_response(&mut input).unwrap(), expected, "{label}");
        }
    }

    #[test]
    fn response_is_written_and_flushed_before_the_next_subscription_event() {
        let flushed = Rc::new(Cell::new(false));
        let mut input = SubscriptionReader {
            read_count: 0,
            first_chunk_flushed: Rc::clone(&flushed),
        };
        let mut stdout = FlushWriter {
            output: Vec::new(),
            flushed,
        };
        let mut stderr = Vec::new();
        assert!(!stream_response(&mut input, &mut stdout, &mut stderr).unwrap());
        assert_eq!(stdout.output, b"first\nsecond\n");
    }

    #[test]
    fn closed_stdout_stops_reading_a_persistent_response() {
        struct BrokenStdout;

        impl Write for BrokenStdout {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let flushed = Rc::new(Cell::new(false));
        let mut input = SubscriptionReader {
            read_count: 0,
            first_chunk_flushed: flushed,
        };
        let mut stderr = Vec::new();
        assert!(!stream_response(&mut input, &mut BrokenStdout, &mut stderr).unwrap());
        assert_eq!(input.read_count, 1);
    }
}
