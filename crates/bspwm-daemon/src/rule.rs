use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};

use nix::fcntl::{FcntlArg, OFlag, fcntl};

pub use bspwm_core::rule::*;

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
    #![allow(clippy::float_cmp)]

    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::types::{ClientState, Direction, HonorSizeHintsMode, Rectangle, StackLayer};

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

    /// Spawns a freshly written test script, retrying while a `fork`ed child of
    /// another test still holds an inherited write descriptor to it.
    ///
    /// `fs::write` opens with `O_CLOEXEC`, but a concurrent `Command::spawn` on
    /// another test thread can `fork` inside that window. The descriptor stays
    /// open in that child until its own `execve` completes, and `execve` of this
    /// script meanwhile fails with `ETXTBSY`.
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

    fn rule(cause: &str, effect: &str, one_shot: bool) -> Rule {
        Rule::from_cause(cause, effect, one_shot)
    }

    #[test]
    fn constructors_match_upstream_defaults() {
        assert_eq!(make_rule(), Rule::default());
        let consequence = make_rule_consequence();
        assert!(consequence.manage);
        assert!(consequence.focus);
        assert!(consequence.border);
        assert_eq!(consequence.honor_size_hints, HonorSizeHintsMode::Default);
        assert_eq!(consequence.split_ratio, 0.0);
        assert_eq!(consequence.state, None);
        assert_eq!(consequence.rect, None);
    }

    #[test]
    fn external_rule_receives_exact_arguments_and_drains_asynchronously() {
        let path = script("sleep 0.03\nprintf '%s\\n' \"$@\"");
        let mut consequence = make_rule_consequence();
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
        let mut process = spawn_script(&path, u32::MAX, &make_rule_consequence());
        await_output(&mut process);
        assert_eq!(process.output(), b"-1");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn external_rules_complete_independently_and_empty_output_is_valid() {
        let empty = script("exit 0");
        let delayed = script("sleep 0.04\nprintf 'focus=off invalid=value'");
        let consequence = make_rule_consequence();
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

    #[test]
    fn cause_construction_unescapes_colons_and_defaults_omitted_fields() {
        assert_eq!(
            Rule::from_cause(r"XTerm:term\:special", "focus=off", false),
            Rule {
                class_name: "XTerm".into(),
                instance_name: "term:special".into(),
                name: MATCH_ANY.into(),
                effect: "focus=off".into(),
                one_shot: false,
            }
        );
    }

    #[test]
    fn rules_are_ordered_and_list_format_matches_upstream() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("A:*:*", "focus=off", false));
        rules.add_rule(rule("B:b:title", "state=floating", true));
        assert_eq!(
            rules.list_rules(),
            "A:*:* => focus=off\nB:b:title -> state=floating\n"
        );
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.class_name.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
    }

    #[test]
    fn removal_by_index_reports_success_and_preserves_order() {
        let mut rules = RuleList::default();
        for class in ["A", "B", "C"] {
            let cause = format!("{class}:*:*");
            rules.add_rule(rule(&cause, "", false));
        }
        assert!(rules.remove_rule_by_index(1));
        assert!(!rules.remove_rule_by_index(9));
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.class_name.as_str())
                .collect::<Vec<_>>(),
            ["A", "C"]
        );
        assert_eq!(rules.remove_rule(0).unwrap().class_name, "A");
    }

    #[test]
    fn removal_by_cause_uses_only_exact_fields_or_a_whole_field_wildcard() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("Firefox:main:Docs", "", false));
        rules.add_rule(rule("Firefox:private:Docs", "", false));
        rules.add_rule(rule("Fire*:main:Docs", "", false));
        rules.remove_rule_by_cause("Firefox:*:Docs");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules.iter().next().unwrap().class_name, "Fire*");
    }

    #[test]
    fn parse_keys_values_has_strtok_pairing_and_ignores_a_dangling_key() {
        let mut consequence = make_rule_consequence();
        parse_keys_values(
            "monitor=one, desktop two\nstate=floating focus=off dangling",
            &mut consequence,
        );
        assert_eq!(consequence.monitor_desc, "one");
        assert_eq!(consequence.desktop_desc, "two");
        assert_eq!(consequence.state, Some(ClientState::Floating));
        assert!(!consequence.focus);
    }

    #[test]
    fn parse_key_value_covers_typed_and_boolean_consequences() {
        let mut consequence = make_rule_consequence();
        parse_keys_values(
            "split_dir=west layer=above split_ratio=0.625junk rectangle=80x60+-2+3 honor_size_hints=tiled hidden=on sticky=true private=off locked=true marked=on center=true follow=off manage=false focus=off border=false",
            &mut consequence,
        );
        assert_eq!(consequence.split_dir, Some(Direction::West));
        assert_eq!(consequence.layer, Some(StackLayer::Above));
        assert_eq!(consequence.split_ratio, 0.625);
        assert_eq!(consequence.rect, Some(Rectangle::new(-2, 3, 80, 60)));
        assert_eq!(consequence.honor_size_hints, HonorSizeHintsMode::Tiled);
        assert!(consequence.hidden && consequence.sticky && consequence.locked);
        assert!(consequence.marked && consequence.center);
        assert!(!consequence.private || consequence.follow || consequence.manage);
        assert!(!consequence.focus && !consequence.border);
    }

    #[test]
    fn invalid_values_follow_upstream_reset_and_preservation_rules() {
        let mut consequence = make_rule_consequence();
        consequence.state = Some(ClientState::Tiled);
        consequence.split_ratio = 0.4;
        consequence.rect = Some(Rectangle::default());
        consequence.honor_size_hints = HonorSizeHintsMode::Yes;
        parse_keys_values(
            "state=invalid split_ratio=1 rectangle=invalid honor_size_hints=invalid focus=invalid",
            &mut consequence,
        );
        assert_eq!(consequence.state, Some(ClientState::Tiled));
        assert_eq!(consequence.split_ratio, 0.4);
        assert_eq!(consequence.rect, None);
        assert_eq!(consequence.honor_size_hints, HonorSizeHintsMode::Default);
        assert!(consequence.focus);
    }

    #[test]
    fn applying_rules_matches_exactly_and_one_shot_removes_then_stops() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("App:*:*", "focus=off", false));
        rules.add_rule(rule("App:main:*", "state=floating", true));
        rules.add_rule(rule("App:*:*", "border=off", false));
        let mut consequence = make_rule_consequence();
        consequence.set_window_properties(&WindowProperties::new("App", "main", "title"));

        rules.apply_rules(&mut consequence);
        assert!(!consequence.focus);
        assert_eq!(consequence.state, Some(ClientState::Floating));
        assert!(consequence.border);
        assert_eq!(rules.len(), 2);

        rules.apply_rules(&mut consequence);
        assert!(!consequence.border);
    }

    #[test]
    fn later_rules_override_matching_wildcard_defaults() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("*", "state=floating focus=off", false));
        rules.add_rule(rule("App:main", "state=tiled focus=on", false));
        let mut consequence = make_rule_consequence();
        consequence.set_window_properties(&WindowProperties::new("App", "main", "title"));

        rules.apply_rules(&mut consequence);

        assert_eq!(consequence.state, Some(ClientState::Tiled));
        assert!(consequence.focus);
    }

    #[test]
    fn consequence_format_matches_external_rule_protocol() {
        let mut consequence = make_rule_consequence();
        parse_keys_values(
            "monitor=one desktop=two node=three state=pseudo_tiled layer=below honor_size_hints=on split_dir=south split_ratio=0.5 hidden=on rectangle=100x80+-1+2",
            &mut consequence,
        );
        assert_eq!(
            print_rule_consequence(&consequence),
            "monitor=one desktop=two node=three state=pseudo_tiled layer=below honor_size_hints=true split_dir=south split_ratio=0.500000 hidden=on sticky=off private=off locked=off marked=off center=off follow=off manage=on focus=on border=on rectangle=100x80+-1+2"
        );
        assert_eq!(
            consequence.to_string(),
            print_rule_consequence(&consequence)
        );
    }

    #[test]
    fn built_in_rules_follow_upstream_order() {
        let mut consequence = make_rule_consequence();
        apply_builtin_rules(
            &BuiltinRuleProperties {
                window_types: vec![BuiltinWindowType::Utility, BuiltinWindowType::Dialog],
                window_states: vec![
                    BuiltinWindowState::Fullscreen,
                    BuiltinWindowState::Below,
                    BuiltinWindowState::Sticky,
                ],
                transient: true,
                ..BuiltinRuleProperties::default()
            },
            &mut consequence,
        );
        assert!(!consequence.focus);
        assert_eq!(consequence.state, Some(ClientState::Floating));
        assert_eq!(consequence.layer, Some(StackLayer::Below));
        assert!(consequence.center && consequence.sticky);
    }
}
