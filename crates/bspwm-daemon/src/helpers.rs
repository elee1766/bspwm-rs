use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use bspwm_core::parse::is_hex_color;

#[cfg(unix)]
use nix::sys::stat::Mode;
#[cfg(unix)]
use nix::unistd::mkfifo;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Read;

pub const RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";

/// Reads a file without imposing UTF-8 on its contents.
///
/// # Errors
///
/// Returns the underlying filesystem error if the file cannot be read.
pub fn read_string(file_path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    fs::read(file_path)
}

/// Copies at most `len` bytes, the Rust equivalent of the useful portion of
/// upstream's NUL-terminated `copy_string` result.
#[must_use]
pub fn copy_string(value: impl AsRef<[u8]>, len: usize) -> Vec<u8> {
    let value = value.as_ref();
    value[..len.min(value.len())].to_vec()
}

#[cfg(unix)]
/// Creates a uniquely named FIFO by replacing the template's trailing `XXXXXX`.
///
/// # Errors
///
/// Returns an error for an invalid template or if the temporary path or FIFO
/// cannot be created.
pub fn mktempfifo(template: &str) -> io::Result<PathBuf> {
    let runtime_dir =
        std::env::var_os(RUNTIME_DIR_ENV).map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    mktempfifo_in(&runtime_dir, template)
}

#[cfg(unix)]
fn mktempfifo_in(runtime_dir: &Path, template: &str) -> io::Result<PathBuf> {
    let prefix = template.strip_suffix("XXXXXX").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "FIFO template must end in XXXXXX",
        )
    })?;

    for _ in 0..100 {
        let mut random = [0_u8; 6];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let suffix: String = random
            .into_iter()
            .map(|byte| {
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
                    [usize::from(byte) % 62] as char
            })
            .collect();
        let path = runtime_dir.join(format!("{prefix}{suffix}"));

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        if let Err(error) = fs::remove_file(&path) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }

        if let Err(error) = mkfifo(&path, Mode::from_bits_truncate(0o666)) {
            let _ = fs::remove_file(&path);
            return Err(io::Error::from(error));
        }
        return Ok(path);
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique FIFO path",
    ))
}

#[must_use]
pub const fn cleaned_mask(mask: u16, num_lock: u16, scroll_lock: u16, caps_lock: u16) -> u16 {
    mask & !(num_lock | scroll_lock | caps_lock)
}

#[must_use]
pub const fn unsigned_subtract(value: u16, amount: u16) -> u16 {
    value.saturating_sub(amount)
}

/// Converts `#RRGGBB` to the opaque pixel used by bspwm on 32-bit visuals.
#[must_use]
pub fn color_pixel(value: &str) -> u32 {
    u32::from_str_radix(value.trim_start_matches('#'), 16).unwrap_or_default() | 0xFF00_0000
}

#[derive(Clone, Debug)]
pub struct EscapedTokens<'a> {
    remaining: &'a [u8],
    separator: u8,
    finished: bool,
}

impl<'a> EscapedTokens<'a> {
    #[must_use]
    pub const fn new(input: &'a [u8], separator: u8) -> Self {
        Self {
            remaining: input,
            separator,
            finished: false,
        }
    }
}

impl Iterator for EscapedTokens<'_> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let mut token = Vec::new();
        let mut escaped = false;
        for (index, &byte) in self.remaining.iter().enumerate() {
            if byte == b'\\' && !escaped {
                escaped = true;
            } else if byte == self.separator && !escaped {
                self.remaining = &self.remaining[index + 1..];
                return Some(token);
            } else {
                token.push(byte);
                escaped = false;
            }
        }

        self.remaining = &[];
        self.finished = true;
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;

    #[test]
    fn reads_all_file_bytes_and_reports_missing_files() {
        let path = isolated_path("read-string");
        fs::write(&path, b"abc\0\xff").unwrap();
        assert_eq!(read_string(&path).unwrap(), b"abc\0\xff");
        fs::remove_file(&path).unwrap();
        assert_eq!(
            read_string(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn copies_only_available_requested_bytes() {
        assert_eq!(copy_string("abcdef", 3), b"abc");
        assert_eq!(copy_string("abc", 8), b"abc");
        assert_eq!(copy_string("abc", 0), b"");
    }

    #[test]
    fn removes_lock_modifiers_from_masks() {
        assert_eq!(
            cleaned_mask(0b1_1111, 0b0_0001, 0b0_0100, 0b1_0000),
            0b0_1010
        );
    }

    #[test]
    fn unsigned_subtraction_stops_at_zero() {
        assert_eq!(unsigned_subtract(12, 5), 7);
        assert_eq!(unsigned_subtract(5, 12), 0);
    }

    #[cfg(unix)]
    #[test]
    fn creates_a_fifo_at_an_isolated_temporary_path() {
        let directory = isolated_path("fifo-dir");
        fs::create_dir(&directory).unwrap();

        let path = mktempfifo_in(&directory, "bspwm_fifo.XXXXXX").unwrap();
        assert_eq!(path.parent(), Some(directory.as_path()));
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_templates_without_six_trailing_placeholders() {
        let error = mktempfifo_in(Path::new("/tmp"), "bspwm_fifo").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    fn isolated_path(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "bspwm-rs-{label}-{}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn validates_only_full_rgb_hex_colors() {
        assert!(is_hex_color("#aBc123"));
        assert!(!is_hex_color("#abc"));
        assert!(!is_hex_color("112233"));
        assert!(!is_hex_color("#gg0000"));
    }

    #[test]
    fn color_pixels_are_forced_opaque() {
        assert_eq!(color_pixel("#123456"), 0xFF12_3456);
    }

    #[test]
    fn tokenizes_escaped_input() {
        for (label, input, expected) in [
            (
                "escaped separators and empty fields",
                &br"a\:b::c\\d:"[..],
                vec![b"a:b".to_vec(), vec![], br"c\d".to_vec(), vec![]],
            ),
            (
                "trailing escape character",
                &b"abc\\"[..],
                vec![b"abc".to_vec()],
            ),
        ] {
            assert_eq!(
                EscapedTokens::new(input, b':').collect::<Vec<_>>(),
                expected,
                "{label}"
            );
        }
    }
}
