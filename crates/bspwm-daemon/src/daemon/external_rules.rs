use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};

use bspwm_core::rule::{RuleConsequence, print_rule_consequence};
use nix::fcntl::{FcntlArg, OFlag, fcntl};

/// A running external rule command and its nonblocking output pipe.
#[derive(Debug)]
pub struct ExternalRuleProcess {
    child: Child,
    stdout: ChildStdout,
    output: Vec<u8>,
    eof: bool,
    reaped: bool,
}

impl ExternalRuleProcess {
    /// Starts one external rule command with its protocol arguments.
    ///
    /// # Errors
    /// Returns an error if the command cannot be spawned or its output pipe
    /// cannot be made nonblocking.
    pub fn spawn(command: &str, window: u32, consequence: &RuleConsequence) -> io::Result<Self> {
        let mut child = Command::new(command)
            .arg(window.cast_signed().to_string())
            .arg(&consequence.class_name)
            .arg(&consequence.instance_name)
            .arg(print_rule_consequence(consequence))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::other("external rule command did not provide a stdout pipe")
        })?;
        let configure_pipe = || {
            let flags = OFlag::from_bits_truncate(
                fcntl(&stdout, FcntlArg::F_GETFL)
                    .map_err(|error| io::Error::from_raw_os_error(error as i32))?,
            );
            fcntl(&stdout, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
                .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
            Ok(())
        };
        if let Err(error) = configure_pipe() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            stdout,
            output: Vec::new(),
            eof: false,
            reaped: false,
        })
    }

    /// Drains all currently available output and reports pipe EOF.
    pub fn poll(&mut self) -> bool {
        let mut buffer = [0_u8; 4096];
        loop {
            match self.stdout.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => self.output.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.eof = true;
                    break;
                }
            }
        }
        self.reap();
        self.eof
    }

    pub fn reap(&mut self) -> bool {
        if !self.reaped {
            self.reaped = self.child.try_wait().is_ok_and(|status| status.is_some());
        }
        self.reaped
    }

    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    pub fn terminate(&mut self) {
        if !self.reap() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

impl Drop for ExternalRuleProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]

    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use bspwm_core::rule::{RuleConsequence, parse_keys_values, print_rule_consequence};

    use super::*;

    static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(0);

    fn script(body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bspwm-rs-rule-{}-{}",
            std::process::id(),
            NEXT_SCRIPT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn spawn_script(
        path: &std::path::Path,
        window: u32,
        consequence: &RuleConsequence,
    ) -> ExternalRuleProcess {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match ExternalRuleProcess::spawn(path.to_str().unwrap(), window, consequence) {
                Ok(process) => return process,
                Err(error)
                    if error.raw_os_error() == Some(nix::errno::Errno::ETXTBSY as i32)
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("could not spawn {}: {error}", path.display()),
            }
        }
    }

    fn await_output(process: &mut ExternalRuleProcess) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process.poll() {
            assert!(Instant::now() < deadline, "external rule command timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn external_rule_receives_exact_arguments_and_drains_asynchronously() {
        let path = script("sleep 0.03\nprintf '%s\\n' \"$@\"");
        let mut consequence = RuleConsequence::default();
        consequence.class_name = "Class Name".into();
        consequence.instance_name = "instance".into();
        consequence.focus = false;
        let serialized = print_rule_consequence(&consequence);
        let mut process = spawn_script(&path, 0x1234_5678, &consequence);

        assert!(!process.poll());
        await_output(&mut process);
        assert_eq!(
            String::from_utf8(process.output().to_vec()).unwrap(),
            format!("305419896\nClass Name\ninstance\n{serialized}\n")
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process.reap() {
            assert!(
                Instant::now() < deadline,
                "external rule child was not reaped"
            );
            thread::sleep(Duration::from_millis(2));
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn external_rule_formats_high_window_ids_like_upstream_percent_i() {
        let path = script("printf '%s' \"$1\"");
        let mut process = spawn_script(&path, u32::MAX, &RuleConsequence::default());
        await_output(&mut process);
        assert_eq!(process.output(), b"-1");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn external_rules_complete_independently_and_empty_output_is_valid() {
        let empty = script("exit 0");
        let delayed = script("sleep 0.04\nprintf 'focus=off invalid=value'");
        let consequence = RuleConsequence::default();
        let mut first = spawn_script(&empty, 1, &consequence);
        let mut second = spawn_script(&delayed, 2, &consequence);
        await_output(&mut first);
        assert!(first.output().is_empty());
        assert!(!second.poll());
        await_output(&mut second);
        let mut parsed = consequence;
        parse_keys_values(&String::from_utf8_lossy(second.output()), &mut parsed);
        assert!(!parsed.focus);
        assert!(parsed.manage);
        fs::remove_file(empty).unwrap();
        fs::remove_file(delayed).unwrap();
    }
}
