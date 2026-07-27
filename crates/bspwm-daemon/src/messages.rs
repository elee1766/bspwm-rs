#![allow(
    clippy::cast_possible_wrap,
    clippy::missing_errors_doc,
    clippy::redundant_closure_for_method_calls
)]

use std::io::{self, Write};
use std::path::PathBuf;

use crate::common::FAILURE_MESSAGE;
use crate::types::SubscriberMask;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscription {
    pub mask: SubscriberMask,
    pub count: i32,
    pub fifo_path: Option<PathBuf>,
}

pub const SELECTOR_OK: i32 = 0;
pub const SELECTOR_INVALID: i32 = 1;
pub const SELECTOR_BAD_MODIFIERS: i32 = 2;
pub const SELECTOR_BAD_DESCRIPTOR: i32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Domain {
    Node,
    Desktop,
    Monitor,
    Query,
    Subscribe,
    Wm,
    Rule,
    Config,
}

/// A message whose first argument names no known domain or command.
///
/// The raw bytes are carried so the diagnostic can reproduce them verbatim,
/// including invalid UTF-8.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Unknown domain or command: '{}'", String::from_utf8_lossy(&self.0))]
pub struct UnknownDomain(pub Vec<u8>);

impl TryFrom<&[u8]> for Domain {
    type Error = UnknownDomain;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        match value {
            b"node" => Ok(Self::Node),
            b"desktop" => Ok(Self::Desktop),
            b"monitor" => Ok(Self::Monitor),
            b"query" => Ok(Self::Query),
            b"subscribe" => Ok(Self::Subscribe),
            b"wm" => Ok(Self::Wm),
            b"rule" => Ok(Self::Rule),
            b"config" => Ok(Self::Config),
            _ => Err(UnknownDomain(value.to_vec())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageControl {
    Continue,
    Quit(i32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageOutcome {
    Close(MessageControl),
    Subscribe(Subscription),
}

pub trait Response: Write {
    fn close(&mut self) -> io::Result<()>;
}

pub trait MessageHandler {
    fn dispatch(
        &mut self,
        domain: Domain,
        args: &[&[u8]],
        rsp: &mut dyn Response,
    ) -> io::Result<Option<Subscription>>;
}

#[must_use]
pub fn decode_arguments(msg: &[u8]) -> Vec<&[u8]> {
    let mut args = Vec::new();
    let mut start = 0;
    for (index, byte) in msg.iter().enumerate() {
        if *byte == 0 {
            args.push(&msg[start..index]);
            start = index + 1;
        }
    }
    args
}

#[allow(clippy::missing_errors_doc)]
pub fn handle_message(
    msg: &[u8],
    exit_status: i32,
    handler: &mut impl MessageHandler,
    rsp: &mut impl Response,
) -> io::Result<MessageOutcome> {
    let args = decode_arguments(msg);
    process_message(&args, exit_status, handler, rsp)
}

#[allow(clippy::missing_errors_doc)]
pub fn process_message(
    args: &[&[u8]],
    exit_status: i32,
    handler: &mut impl MessageHandler,
    rsp: &mut impl Response,
) -> io::Result<MessageOutcome> {
    let Some((domain, args)) = args.split_first() else {
        fail(rsp, b"No arguments given.\n")?;
        finish_response(rsp)?;
        return Ok(MessageOutcome::Close(MessageControl::Continue));
    };

    let control = if *domain == b"quit" {
        cmd_quit(args, exit_status, rsp)?
    } else {
        match Domain::try_from(*domain) {
            Ok(domain) => {
                if let Some(subscription) = handler.dispatch(domain, args, rsp)? {
                    return Ok(MessageOutcome::Subscribe(subscription));
                }
            }
            Err(UnknownDomain(name)) => {
                fail_parts(rsp, &[b"Unknown domain or command: '", &name, b"'.\n"])?;
            }
        }
        MessageControl::Continue
    };

    finish_response(rsp)?;
    Ok(MessageOutcome::Close(control))
}

#[allow(clippy::missing_errors_doc)]
pub fn cmd_quit(
    args: &[&[u8]],
    exit_status: i32,
    rsp: &mut dyn Response,
) -> io::Result<MessageControl> {
    if let Some(argument) = args.first() {
        let Some(status) = scan_integer(argument) else {
            fail_parts(rsp, &[b"quit: Invalid argument: '", argument, b"'.\n"])?;
            return Ok(MessageControl::Continue);
        };
        return Ok(MessageControl::Quit(status));
    }
    Ok(MessageControl::Quit(exit_status))
}

/// `scanf("%i")`-equivalent prefix scan: base-0 radix sniffing, wrapping on overflow.
pub(crate) fn scan_integer(input: &[u8]) -> Option<i32> {
    crate::parse::scan_wrapping_i32(input)
}

#[allow(clippy::missing_errors_doc)]
pub fn handle_failure(code: i32, src: &[u8], val: &[u8], rsp: &mut dyn Response) -> io::Result<()> {
    match code {
        SELECTOR_BAD_DESCRIPTOR => fail_parts(
            rsp,
            &[src, b": Invalid descriptor found in '", val, b"'.\n"],
        ),
        SELECTOR_BAD_MODIFIERS => {
            fail_parts(rsp, &[src, b": Invalid modifier found in '", val, b"'.\n"])
        }
        SELECTOR_INVALID => fail(rsp, b""),
        _ => Ok(()),
    }
}

#[allow(clippy::missing_errors_doc)]
pub fn fail(rsp: &mut dyn Response, message: &[u8]) -> io::Result<()> {
    rsp.write_all(&[FAILURE_MESSAGE])?;
    rsp.write_all(message)
}

pub(crate) fn fail_parts(rsp: &mut dyn Response, parts: &[&[u8]]) -> io::Result<()> {
    rsp.write_all(&[FAILURE_MESSAGE])?;
    for part in parts {
        rsp.write_all(part)?;
    }
    Ok(())
}

fn finish_response(rsp: &mut dyn Response) -> io::Result<()> {
    let flush_result = rsp.flush();
    let close_result = rsp.close();
    flush_result.and(close_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestResponse {
        bytes: Vec<u8>,
        flushes: usize,
        closes: usize,
    }

    impl Write for TestResponse {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    impl Response for TestResponse {
        fn close(&mut self) -> io::Result<()> {
            self.closes += 1;
            Ok(())
        }
    }

    struct Recorder {
        calls: Vec<(Domain, Vec<Vec<u8>>)>,
        subscription_valid: bool,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Self {
                calls: Vec::new(),
                subscription_valid: true,
            }
        }
    }

    impl Recorder {
        fn record(&mut self, domain: Domain, args: &[&[u8]]) {
            self.calls.push((
                domain,
                args.iter().map(|argument| argument.to_vec()).collect(),
            ));
        }
    }

    impl MessageHandler for Recorder {
        fn dispatch(
            &mut self,
            domain: Domain,
            args: &[&[u8]],
            _rsp: &mut dyn Response,
        ) -> io::Result<Option<Subscription>> {
            self.record(domain, args);
            Ok((domain == Domain::Subscribe)
                .then(|| {
                    self.subscription_valid.then_some(Subscription {
                        mask: SubscriberMask::REPORT,
                        count: -1,
                        fifo_path: None,
                    })
                })
                .flatten())
        }
    }

    #[test]
    fn handle_message_decodes_only_nul_terminated_arguments() {
        let mut handler = Recorder::default();
        let mut rsp = TestResponse::default();
        let result = handle_message(b"node\0\0third\0ignored", 0, &mut handler, &mut rsp).unwrap();
        assert_eq!(
            handler.calls,
            vec![(Domain::Node, vec![b"".to_vec(), b"third".to_vec()])]
        );
        assert_eq!(result, MessageOutcome::Close(MessageControl::Continue));
        assert_eq!((rsp.flushes, rsp.closes), (1, 1));
    }

    #[test]
    fn decoder_preserves_every_non_nul_byte_and_empty_fields() {
        assert_eq!(decode_arguments(b"\0"), vec![&b""[..]]);
        assert_eq!(decode_arguments(b"\0\0"), vec![&b""[..], &b""[..]]);
        assert_eq!(decode_arguments(b"a\0unterminated"), vec![&b"a"[..]]);

        for byte in 1_u8..=u8::MAX {
            let message = [byte, 0];
            assert_eq!(
                decode_arguments(&message),
                vec![&message[..1]],
                "byte {byte}"
            );
        }
    }

    #[test]
    fn no_complete_argument_has_exact_failure_and_closes_response() {
        for message in [&b""[..], &b"query"[..], &b"\xff"[..]] {
            let mut handler = Recorder::default();
            let mut rsp = TestResponse::default();
            let result = handle_message(message, 0, &mut handler, &mut rsp).unwrap();
            assert_eq!(rsp.bytes, b"\x07No arguments given.\n");
            assert_eq!((rsp.flushes, rsp.closes), (1, 1));
            assert_eq!(result, MessageOutcome::Close(MessageControl::Continue));
            assert!(handler.calls.is_empty());
        }
    }

    #[test]
    fn all_nine_domains_dispatch_exactly_and_preserve_raw_arguments() {
        let domains: &[(&[u8], Domain)] = &[
            (b"node", Domain::Node),
            (b"desktop", Domain::Desktop),
            (b"monitor", Domain::Monitor),
            (b"query", Domain::Query),
            (b"subscribe", Domain::Subscribe),
            (b"wm", Domain::Wm),
            (b"rule", Domain::Rule),
            (b"config", Domain::Config),
        ];
        for &(name, domain) in domains {
            let mut handler = Recorder::default();
            let mut rsp = TestResponse::default();
            let result =
                process_message(&[name, b"a", b"\xff"], 7, &mut handler, &mut rsp).unwrap();
            assert_eq!(
                handler.calls,
                vec![(domain, vec![b"a".to_vec(), vec![0xff]])]
            );
            let expected = if domain == Domain::Subscribe {
                MessageOutcome::Subscribe(Subscription {
                    mask: SubscriberMask::REPORT,
                    count: -1,
                    fifo_path: None,
                })
            } else {
                MessageOutcome::Close(MessageControl::Continue)
            };
            assert_eq!(result, expected);
        }

        let mut handler = Recorder::default();
        let mut rsp = TestResponse::default();
        let result = process_message(&[b"quit"], 7, &mut handler, &mut rsp).unwrap();
        assert_eq!(result, MessageOutcome::Close(MessageControl::Quit(7)));
        assert!(handler.calls.is_empty());
    }

    #[test]
    fn domain_matching_is_exact_and_unknown_diagnostic_is_byte_preserving() {
        for domain in [&b"Node"[..], &b"node\0suffix"[..], &b"\xff"[..], &b""[..]] {
            let mut handler = Recorder::default();
            let mut rsp = TestResponse::default();
            let result = process_message(&[domain], 0, &mut handler, &mut rsp).unwrap();
            let mut expected = b"\x07Unknown domain or command: '".to_vec();
            expected.extend_from_slice(domain);
            expected.extend_from_slice(b"'.\n");
            assert_eq!(rsp.bytes, expected);
            assert_eq!(result, MessageOutcome::Close(MessageControl::Continue));
            assert_eq!((rsp.flushes, rsp.closes), (1, 1));
        }
    }

    #[test]
    fn subscription_controls_its_response_lifetime() {
        let subscription = Subscription {
            mask: SubscriberMask::REPORT,
            count: -1,
            fifo_path: None,
        };
        for valid in [true, false] {
            let mut handler = Recorder {
                subscription_valid: valid,
                ..Recorder::default()
            };
            let mut rsp = TestResponse::default();
            let result = process_message(&[b"subscribe"], 0, &mut handler, &mut rsp).unwrap();
            let count = usize::from(!valid);
            let expected = valid.then_some(subscription.clone()).map_or(
                MessageOutcome::Close(MessageControl::Continue),
                MessageOutcome::Subscribe,
            );
            assert_eq!(result, expected);
            assert_eq!((rsp.flushes, rsp.closes), (count, count));
        }
    }

    struct QuitCase {
        label: &'static str,
        args: &'static [&'static [u8]],
        inherited_status: i32,
        control: MessageControl,
        response: &'static [u8],
    }

    const QUIT_CASES: &[QuitCase] = &[
        QuitCase {
            label: "zero",
            args: &[b"0", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(0),
            response: b"",
        },
        QuitCase {
            label: "decimal",
            args: &[b"42", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(42),
            response: b"",
        },
        QuitCase {
            label: "signed prefix",
            args: &[b"  -17junk", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(-17),
            response: b"",
        },
        QuitCase {
            label: "octal",
            args: &[b"+010", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(8),
            response: b"",
        },
        QuitCase {
            label: "invalid octal suffix",
            args: &[b"09", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(0),
            response: b"",
        },
        QuitCase {
            label: "hexadecimal prefix",
            args: &[b"0x10tail", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(16),
            response: b"",
        },
        QuitCase {
            label: "negative hexadecimal",
            args: &[b"-0Xf", b"ignored"],
            inherited_status: 99,
            control: MessageControl::Quit(-15),
            response: b"",
        },
        QuitCase {
            label: "no argument inherits status",
            args: &[],
            inherited_status: 99,
            control: MessageControl::Quit(99),
            response: b"",
        },
        QuitCase {
            label: "empty",
            args: &[b""],
            inherited_status: 12,
            control: MessageControl::Continue,
            response: b"\x07quit: Invalid argument: ''.\n",
        },
        QuitCase {
            label: "sign only",
            args: &[b"  +"],
            inherited_status: 12,
            control: MessageControl::Continue,
            response: b"\x07quit: Invalid argument: '  +'.\n",
        },
        QuitCase {
            label: "text",
            args: &[b"garbage"],
            inherited_status: 12,
            control: MessageControl::Continue,
            response: b"\x07quit: Invalid argument: 'garbage'.\n",
        },
        QuitCase {
            label: "non-UTF-8",
            args: &[b"\xff"],
            inherited_status: 12,
            control: MessageControl::Continue,
            response: b"\x07quit: Invalid argument: '\xff'.\n",
        },
    ];

    #[test]
    fn cmd_quit_matches_percent_i_prefix_parsing_and_rejects_invalid_arguments() {
        for case in QUIT_CASES {
            let mut rsp = TestResponse::default();
            assert_eq!(
                cmd_quit(case.args, case.inherited_status, &mut rsp).unwrap(),
                case.control,
                "{} control",
                case.label,
            );
            assert_eq!(rsp.bytes, case.response, "{} response", case.label);
        }
    }

    #[test]
    fn failure_framing_and_selector_messages_are_exact() {
        let cases: &[(i32, &[u8])] = &[
            (
                SELECTOR_BAD_DESCRIPTOR,
                b"\x07query -n: Invalid descriptor found in '\xff'.\n",
            ),
            (
                SELECTOR_BAD_MODIFIERS,
                b"\x07query -n: Invalid modifier found in '\xff'.\n",
            ),
            (SELECTOR_INVALID, b"\x07"),
            (SELECTOR_OK, b""),
            (99, b""),
        ];
        for &(code, expected) in cases {
            let mut rsp = TestResponse::default();
            handle_failure(code, b"query -n", b"\xff", &mut rsp).unwrap();
            assert_eq!(rsp.bytes, expected);
        }

        let mut rsp = TestResponse::default();
        fail(&mut rsp, b"first").unwrap();
        fail(&mut rsp, b"").unwrap();
        assert_eq!(rsp.bytes, b"\x07first\x07");
    }

    #[test]
    fn finish_attempts_close_even_when_flush_fails() {
        struct FlushFailure {
            closed: bool,
        }

        impl Write for FlushFailure {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                Ok(buffer.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("flush"))
            }
        }

        impl Response for FlushFailure {
            fn close(&mut self) -> io::Result<()> {
                self.closed = true;
                Ok(())
            }
        }

        let mut rsp = FlushFailure { closed: false };
        let mut handler = Recorder::default();
        assert!(process_message(&[b"query"], 0, &mut handler, &mut rsp).is_err());
        assert!(rsp.closed);
    }
}
