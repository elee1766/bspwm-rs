use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;

pub const BUFFER_SIZE: usize = 8192;
pub const FAILURE_MESSAGE: u8 = 0x07;
pub const SOCKET_ENV_VAR: &str = "BSPWM_SOCKET";
pub const SOCKET_PATH_TEMPLATE: &str = "/tmp/bspwm{host}_{display}_{screen}-socket";
pub const RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Display {
    pub host: String,
    pub display: i32,
    pub screen: i32,
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

/// Expands `{host}`, `{display}`, and `{screen}` placeholders in a path template.
#[must_use]
pub fn expand_path_template(template: &str, display: &Display) -> PathBuf {
    PathBuf::from(
        template
            .replace("{host}", &display.host)
            .replace("{display}", &display.display.to_string())
            .replace("{screen}", &display.screen.to_string()),
    )
}

#[must_use]
pub fn socket_path_from_env() -> Option<PathBuf> {
    if let Some(path) = env::var_os(SOCKET_ENV_VAR) {
        return Some(PathBuf::from(path));
    }
    let display = parse_display(&env::var("DISPLAY").ok()?)?;
    Some(expand_path_template(SOCKET_PATH_TEMPLATE, &display))
}

/// Creates a uniquely named FIFO by replacing the template's trailing `XXXXXX`.
///
/// # Errors
/// Returns an error for an invalid template or if randomness or FIFO creation fails.
pub fn create_fifo(template: &str) -> io::Result<PathBuf> {
    let runtime_dir =
        env::var_os(RUNTIME_DIR_ENV).map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    create_fifo_in(&runtime_dir, template)
}

fn create_fifo_in(runtime_dir: &Path, template: &str) -> io::Result<PathBuf> {
    let prefix = template.strip_suffix("XXXXXX").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "FIFO template must end in XXXXXX",
        )
    })?;

    for _ in 0..100 {
        let mut random = [0_u8; 6];
        getrandom::fill(&mut random).map_err(io::Error::from)?;
        let suffix: String = random
            .into_iter()
            .map(|byte| {
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
                    [usize::from(byte) % 62] as char
            })
            .collect();
        let path = runtime_dir.join(format!("{prefix}{suffix}"));

        match mkfifo(&path, Mode::from_bits_truncate(0o666)) {
            Ok(()) => return Ok(path),
            Err(Errno::EEXIST) => {}
            Err(error) => return Err(io::Error::from(error)),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique FIFO path",
    ))
}

/// A listener that removes only the exact filesystem socket it created.
#[derive(Debug)]
pub struct SocketListener {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketListener {
    /// # Errors
    /// Returns an error if the path cannot be safely bound as a Unix socket.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                remove_stale_socket(path)?;
                UnixListener::bind(path)?
            }
            Err(error) => return Err(error),
        };
        listener.set_nonblocking(true)?;
        Self::from_listener(listener, path)
    }

    /// # Errors
    /// Returns an error if the listener or its filesystem path cannot be inspected.
    pub fn inherited(listener: UnixListener, path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        listener.set_nonblocking(true)?;
        Self::from_listener(listener, path)
    }

    fn from_listener(listener: UnixListener, path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    /// # Errors
    /// Returns an error if the close-on-exec descriptor flag cannot be cleared.
    pub fn set_inheritable(&self) -> io::Result<()> {
        set_inheritable(&self.listener)
    }

    /// # Errors
    /// Returns an error if no pending connection can be accepted.
    pub fn accept(&self) -> io::Result<(UnixStream, SocketAddr)> {
        self.listener.accept()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        if fs::metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to replace a non-socket filesystem entry",
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "an active process owns the Unix socket",
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug)]
pub struct UnixResponse {
    stream: UnixStream,
    closed: bool,
}

impl UnixResponse {
    #[must_use]
    pub const fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            closed: false,
        }
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Reports whether the peer has torn the connection down completely.
    pub fn peer_disconnected(&mut self) -> bool {
        if self.closed {
            return true;
        }
        let mut fds = [PollFd::new(self.stream.as_fd(), PollFlags::empty())];
        match poll(&mut fds, PollTimeout::ZERO) {
            Ok(0) | Err(Errno::EINTR) => false,
            Ok(_) => fds[0].revents().is_some_and(|revents| {
                revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL)
            }),
            Err(_) => true,
        }
    }

    /// Closes the write side while retaining the socket for disconnect checks.
    ///
    /// # Errors
    /// Returns an error if the socket cannot be shut down.
    pub fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        match self.stream.shutdown(Shutdown::Write) {
            Ok(()) => {
                self.closed = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                self.closed = true;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl AsFd for UnixResponse {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl Write for UnixResponse {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "response is closed",
            ));
        }
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "response is closed",
            ));
        }
        self.stream.flush()
    }
}

/// Reads one NUL-terminated request without allowing it to exceed `limit` bytes.
///
/// # Errors
/// Returns an error if reading fails or the request exceeds `limit`.
pub fn receive_request(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut request = Vec::with_capacity(limit.min(BUFFER_SIZE));
    let mut buffer = [0_u8; BUFFER_SIZE + 1];
    loop {
        let remaining = limit.saturating_sub(request.len());
        let read_length = remaining.saturating_add(1).min(buffer.len());
        let count = match reader.read(&mut buffer[..read_length]) {
            Ok(0) => return Ok(request),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        request.extend_from_slice(&buffer[..count]);
        if request.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("request exceeds the {limit}-byte limit"),
            ));
        }
        if request.last() == Some(&0) {
            return Ok(request);
        }
    }
}

/// Clears `FD_CLOEXEC` so an owned descriptor can cross a process restart.
///
/// # Errors
/// Returns an error if descriptor flags cannot be read or changed.
pub fn set_inheritable(fd: &impl AsFd) -> io::Result<()> {
    let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).map_err(io::Error::from)?);
    fcntl(fd, FcntlArg::F_SETFD(flags - FdFlag::FD_CLOEXEC))
        .map(|_| ())
        .map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::FileTypeExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    fn test_path(label: &str) -> PathBuf {
        static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
        env::temp_dir().join(format!(
            "bspwm-ipc-{label}-{}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
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
                "remote",
                "host:2.3",
                Some(Display {
                    host: "host".into(),
                    display: 2,
                    screen: 3,
                }),
            ),
            (
                "protocol",
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
    fn creates_a_fifo_at_an_isolated_temporary_path() {
        let directory = test_path("fifo-dir");
        fs::create_dir(&directory).unwrap();
        let path = create_fifo_in(&directory, "bspwm_fifo.XXXXXX").unwrap();
        assert_eq!(path.parent(), Some(directory.as_path()));
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rejects_fifo_templates_without_six_trailing_placeholders() {
        let error = create_fifo_in(Path::new("/tmp"), "bspwm_fifo").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
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
        drop(replacement);
        assert!(!path.exists());
    }

    #[test]
    fn response_detects_disconnect_without_treating_half_close_as_a_hangup() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let mut response = UnixResponse::new(server);
        client.shutdown(Shutdown::Write).unwrap();
        assert!(!response.peer_disconnected());
        response.write_all(b"answer").unwrap();
        response.close().unwrap();
        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        assert_eq!(output, b"answer");

        drop(client);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !response.peer_disconnected() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(response.peer_disconnected());
    }
}
