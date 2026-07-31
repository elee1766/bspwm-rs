//! State-backed implementation of the bspwm command protocol.
//!
//! Commands whose correct implementation requires an X connection deliberately
//! fail instead of updating only half of the window manager's state.

/// Yields the contents of an `Option`, or leaves the enclosing handler with the
/// bare failure byte upstream emits for an unusable selection.
///
/// Defined before the submodules so that their textual scope includes it.
macro_rules! require {
    ($value:expr, $rsp:expr $(,)?) => {
        match $value {
            Some(value) => value,
            None => return crate::messages::fail($rsp, b""),
        }
    };
}

macro_rules! command_argument {
    ($cursor:ident, $command:ident, raw) => {
        $cursor.required($command)?
    };
    ($cursor:ident, $command:ident, optional) => {
        $cursor
            .peek()
            .filter(|argument| !argument.starts_with(b"-"))
            .and_then(|_| $cursor.next())
    };
    ($cursor:ident, $command:ident, parse($parser:path)) => {
        $cursor.required_parse($command, $parser)?
    };
    ($cursor:ident, $command:ident, custom($parser:path)) => {
        $parser($cursor, $command)?
    };
    ($cursor:ident, $command:ident, map($parser:path)) => {
        $parser($cursor, $command)
    };
    ($cursor:ident, $command:ident, rest1) => {{
        if $cursor.is_empty() {
            return Err($crate::commands::CommandParseError::NotEnoughArguments {
                command: $command,
            });
        }
        $cursor.take_remaining()
    }};
}

// Generates a typed command stream parser that preserves side effects from
// commands preceding a parse failure.
macro_rules! command_set {
    (
        domain: $domain:literal;
        enum $name:ident <'a> {
            $(
                $variant:ident $( {
                    $(
                        $field:ident : $field_type:ty = $argument:ident $(($parser:path))?
                    ),* $(,)?
                } )? => [$($alias:literal),+ $(,)?]
            ),* $(,)?
        }
    ) => {
        enum $name<'a> {
            $(
                $variant $( {
                    $($field: $field_type),*
                } )?
            ),*
        }

        impl<'a> $name<'a> {
            fn parse(
                cursor: &mut $crate::commands::ArgCursor<'_, 'a>,
            ) -> Result<Self, $crate::commands::CommandParseError<'a>> {
                let Some(command) = cursor.next() else {
                    return Err($crate::commands::CommandParseError::MissingCommands);
                };
                match command {
                    $(
                        $($alias)|+ => {
                            $(
                                $(
                                    let $field = command_argument!(
                                        cursor,
                                        command,
                                        $argument $(($parser))?
                                    );
                                )*
                            )?
                            Ok(Self::$variant $( { $($field),* } )?)
                        }
                    ),*
                    _ => Err($crate::commands::CommandParseError::UnknownCommand { command }),
                }
            }

            fn next(
                cursor: &mut $crate::commands::ArgCursor<'_, 'a>,
                rsp: &mut dyn $crate::messages::Response,
            ) -> std::io::Result<Option<Self>> {
                if cursor.is_empty() {
                    return Ok(None);
                }
                match Self::parse(cursor) {
                    Ok(command) => Ok(Some(command)),
                    Err(error) => {
                        error.respond(rsp, $domain)?;
                        Ok(None)
                    }
                }
            }
        }
    };
}

mod config;
mod desktop;
mod effects;
mod monitor;
mod node;
mod query;
mod rule;
mod selectors;
mod wm;

use std::io;

use crate::messages::{
    Domain, MessageHandler, Response, Subscription, fail, fail_parts, handle_failure,
};
use crate::parse::{
    BoolDeclaration, parse_bool, parse_bool_declaration, parse_circulate_direction,
    parse_client_state, parse_cycle_direction, parse_degree, parse_desktop_modifiers,
    parse_direction, parse_flip, parse_index, parse_layout, parse_monitor_modifiers,
    parse_node_modifiers, parse_rectangle, parse_resize_handle, parse_split_type,
    parse_stack_layer,
};
use crate::query::{Coordinates, locate_desktop, locate_monitor};
use crate::state::{CommandEffect, DaemonState};
use crate::tree::NodeFlag;
use crate::types::{ClientState, CycleDirection, SplitType};

/// A message handler that applies commands to the daemon's pure in-memory state.
pub struct CommandHandler<'a> {
    pub state: &'a mut DaemonState,
}

impl<'a> CommandHandler<'a> {
    #[must_use]
    pub const fn new(state: &'a mut DaemonState) -> Self {
        Self { state }
    }
}

impl MessageHandler for CommandHandler<'_> {
    fn dispatch(
        &mut self,
        domain: Domain,
        args: &[&[u8]],
        rsp: &mut dyn Response,
    ) -> io::Result<Option<Subscription>> {
        match domain {
            Domain::Node => self.handle_node(args, rsp)?,
            Domain::Desktop => self.handle_desktop(args, rsp)?,
            Domain::Monitor => self.handle_monitor(args, rsp)?,
            Domain::Query => self.handle_query(args, rsp)?,
            Domain::Subscribe => return crate::subscribe::subscribe(args, rsp),
            Domain::Wm => self.handle_wm(args, rsp)?,
            Domain::Rule => self.handle_rule(args, rsp)?,
            Domain::Config => self.handle_config(args, rsp)?,
        }
        // Structural commands free nodes their caller never named -- a
        // collapsing branch, a consumed receptacle -- and the arena reuses
        // those slots, so no store may still be holding one when the command
        // returns.
        self.state.forget_retired_nodes();
        Ok(None)
    }
}

fn text(value: &[u8]) -> Option<&str> {
    std::str::from_utf8(value).ok()
}

struct ArgCursor<'args, 'bytes> {
    args: &'args [&'bytes [u8]],
    index: usize,
}

impl<'args, 'bytes> ArgCursor<'args, 'bytes> {
    const fn new(args: &'args [&'bytes [u8]]) -> Self {
        Self { args, index: 0 }
    }

    fn is_empty(&self) -> bool {
        self.index == self.args.len()
    }

    fn next(&mut self) -> Option<&'bytes [u8]> {
        let argument = self.args.get(self.index).copied()?;
        self.index += 1;
        Some(argument)
    }

    fn peek(&self) -> Option<&'bytes [u8]> {
        self.args.get(self.index).copied()
    }

    fn required(
        &mut self,
        command: &'bytes [u8],
    ) -> Result<&'bytes [u8], CommandParseError<'bytes>> {
        self.next()
            .ok_or(CommandParseError::NotEnoughArguments { command })
    }

    fn required_parse<T>(
        &mut self,
        command: &'bytes [u8],
        parser: impl FnOnce(&'bytes [u8]) -> Option<T>,
    ) -> Result<T, CommandParseError<'bytes>> {
        let value = self.required(command)?;
        parser(value).ok_or(CommandParseError::InvalidArgument { command, value })
    }

    fn take_remaining(&mut self) -> Vec<&'bytes [u8]> {
        let remaining = self.args[self.index..].to_vec();
        self.index = self.args.len();
        remaining
    }

    fn remaining(&self) -> &'args [&'bytes [u8]] {
        &self.args[self.index..]
    }
}

struct SelectorFollowArguments<'a> {
    selector: &'a [u8],
    follow: bool,
}

struct CommandArgument<'a> {
    command: &'a [u8],
    value: &'a [u8],
}

struct CommandRestArguments<'a> {
    command: &'a [u8],
    values: Vec<&'a [u8]>,
}

fn parse_command_rest_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> CommandRestArguments<'a> {
    CommandRestArguments {
        command,
        values: cursor.take_remaining(),
    }
}

fn parse_command_argument<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<CommandArgument<'a>, CommandParseError<'a>> {
    Ok(CommandArgument {
        command,
        value: cursor.required(command)?,
    })
}

fn parse_selector_follow_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<SelectorFollowArguments<'a>, CommandParseError<'a>> {
    let selector = cursor.required(command)?;
    let follow = cursor.peek() == Some(b"--follow");
    if follow {
        cursor.next();
    }
    Ok(SelectorFollowArguments { selector, follow })
}

fn parse_optional_selector<'a>(cursor: &mut ArgCursor<'_, 'a>) -> Option<&'a [u8]> {
    cursor
        .peek()
        .filter(|argument| !argument.starts_with(b"-"))
        .and_then(|_| cursor.next())
}

fn parse_terminal<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<(), CommandParseError<'a>> {
    if cursor.is_empty() {
        Ok(())
    } else {
        Err(CommandParseError::TrailingCommands { command })
    }
}

enum CommandParseError<'a> {
    MissingCommands,
    NotEnoughArguments { command: &'a [u8] },
    InvalidArgument { command: &'a [u8], value: &'a [u8] },
    UnknownCommand { command: &'a [u8] },
    TrailingCommands { command: &'a [u8] },
}

impl CommandParseError<'_> {
    fn respond(self, rsp: &mut dyn Response, domain: &[u8]) -> io::Result<()> {
        match self {
            Self::MissingCommands => fail_parts(rsp, &[domain, b": Missing commands.\n"]),
            Self::NotEnoughArguments { command } => not_enough(rsp, domain, command),
            Self::InvalidArgument { command, value } => {
                invalid_argument(rsp, domain, command, value)
            }
            Self::UnknownCommand { command } => unknown_command(rsp, domain, command),
            Self::TrailingCommands { command } => trailing_commands(rsp, domain, command),
        }
    }
}

fn not_enough(rsp: &mut dyn Response, domain: &[u8], command: &[u8]) -> io::Result<()> {
    fail_parts(rsp, &[domain, b" ", command, b": Not enough arguments.\n"])
}

fn invalid_argument(
    rsp: &mut dyn Response,
    domain: &[u8],
    command: &[u8],
    value: &[u8],
) -> io::Result<()> {
    fail_parts(
        rsp,
        &[
            domain,
            b" ",
            command,
            b": Invalid argument: '",
            value,
            b"'.\n",
        ],
    )
}

fn unknown_command(rsp: &mut dyn Response, domain: &[u8], command: &[u8]) -> io::Result<()> {
    fail_parts(rsp, &[domain, b": Unknown command: '", command, b"'.\n"])
}

fn unknown_option(rsp: &mut dyn Response, domain: &[u8], option: &[u8]) -> io::Result<()> {
    fail_parts(rsp, &[domain, b": Unknown option: '", option, b"'.\n"])
}

fn trailing_commands(rsp: &mut dyn Response, domain: &[u8], command: &[u8]) -> io::Result<()> {
    fail_parts(rsp, &[domain, b" ", command, b": Trailing commands.\n"])
}

fn node_state_status(
    world: &crate::world::World,
    monitor: crate::world::MonitorId,
    desktop: crate::world::DesktopId,
    node: crate::tree::NodeId,
    state: ClientState,
    enabled: bool,
) -> String {
    let state = state.protocol_name();
    format!(
        "node_state 0x{:08X} 0x{:08X} 0x{:08X} {state} {}\n",
        world.monitor(monitor).external_id,
        world.desktop(desktop).external_id,
        world.tree.node(node).external_id,
        if enabled { "on" } else { "off" },
    )
}

#[cfg(test)]
mod tests;
