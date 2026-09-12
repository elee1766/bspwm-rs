use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

pub use bspwm_ipc::{
    BUFFER_SIZE, Display, FAILURE_MESSAGE, SOCKET_ENV_VAR, SOCKET_PATH_TEMPLATE,
    expand_path_template, parse_display, socket_path_from_env,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Response {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub failed: bool,
}

/// Encodes arguments as the NUL-separated, NUL-terminated wire message.
///
/// Returns `None` when the arguments do not fit in [`BUFFER_SIZE`]. Truncating
/// instead would silently drop trailing arguments, turning a too-long command
/// into a different, still-valid one: `monitor --rename <long> --focus` would
/// lose `--focus` and be executed without it.
#[must_use]
pub fn make_message<'a>(args: impl IntoIterator<Item = &'a str>) -> Option<Vec<u8>> {
    let mut message = Vec::with_capacity(BUFFER_SIZE);
    for argument in args {
        message.extend_from_slice(argument.as_bytes());
        message.push(0);
        if message.len() > BUFFER_SIZE {
            return None;
        }
    }
    Some(message)
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
    let message = make_message(args.iter().map(String::as_str)).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("message exceeds the {BUFFER_SIZE}-byte limit"),
        )
    })?;
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

/// Streams a daemon response, splitting it into stdout and stderr.
///
/// Failure is signalled in band by [`FAILURE_MESSAGE`], which the daemon writes
/// immediately before the diagnostic. The marker is located wherever it falls
/// rather than only at the head of a read: a successful command's output and a
/// later command's error can arrive in one chunk, and the daemon may flush the
/// marker and its message separately. Everything after the first marker belongs
/// to stderr, which matches the daemon writing it last before closing.
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
        let mut chunk = &buffer[..count];
        if !failed
            && let Some(marker) = chunk.iter().position(|byte| *byte == FAILURE_MESSAGE)
        {
            let (before, after) = chunk.split_at(marker);
            if !before.is_empty() && write_or_stop(stdout, before)? {
                return Ok(failed);
            }
            failed = true;
            chunk = &after[1..];
        }
        if chunk.is_empty() {
            continue;
        }
        if failed {
            stderr.write_all(chunk)?;
            stderr.flush()?;
        } else if write_or_stop(stdout, chunk)? {
            return Ok(failed);
        }
    }
}

/// Writes to stdout, reporting whether a closed pipe should end the stream.
fn write_or_stop(stdout: &mut impl Write, bytes: &[u8]) -> io::Result<bool> {
    match stdout.write_all(bytes).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(true),
        Err(error) => Err(error),
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
        assert_eq!(make_message(["query", "-M"]).unwrap(), b"query\0-M\0");
        // An over-long message is rejected rather than silently losing arguments.
        let long = "x".repeat(BUFFER_SIZE);
        assert_eq!(make_message(["monitor", long.as_str(), "--focus"]), None);
    }

    #[test]
    fn response_routes_failure_chunks_to_stderr() {
        let cases = [
            (
                "clean success",
                &b"ok\n"[..],
                Response {
                    stdout: b"ok\n".to_vec(),
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
            (
                // One request can carry several commands: earlier output
                // succeeds and a later command fails, sharing a read.
                "output followed by a failure",
                &b"ok\n\x07bad\n"[..],
                Response {
                    stdout: b"ok\n".to_vec(),
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
    fn failure_marker_split_across_reads_is_still_detected() {
        let mut input = Chunks::new(&[b"\x07", b"Invalid argument.\n"]);
        assert_eq!(
            read_response(&mut input).unwrap(),
            Response {
                stdout: Vec::new(),
                stderr: b"Invalid argument.\n".to_vec(),
                failed: true,
            }
        );
    }

    struct Chunks {
        chunks: std::collections::VecDeque<Vec<u8>>,
    }

    impl Chunks {
        fn new(chunks: &[&[u8]]) -> Self {
            Self {
                chunks: chunks.iter().map(|chunk| chunk.to_vec()).collect(),
            }
        }
    }

    impl Read for Chunks {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            buffer[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
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
