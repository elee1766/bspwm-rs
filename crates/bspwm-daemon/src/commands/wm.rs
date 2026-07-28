use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::{
    ArgCursor, CommandEffect, CommandHandler, CommandParseError, Response, fail, locate_monitor,
    parse_bool, parse_rectangle, text,
};
use crate::query::query_state;
use crate::restore::restore_state;
use crate::subscribe::print_report;
use crate::types::Rectangle;

fn parse_bool_argument(argument: &[u8]) -> Option<bool> {
    text(argument).and_then(parse_bool)
}

struct AddMonitorArguments<'a> {
    name: &'a str,
    rectangle: Rectangle,
}

fn parse_add_monitor_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<AddMonitorArguments<'a>, CommandParseError<'a>> {
    let name = cursor.required(command)?;
    let rectangle_argument = cursor.required(command)?;
    let (Some(name), Some(rectangle)) = (
        text(name),
        text(rectangle_argument).and_then(parse_rectangle),
    ) else {
        return Err(CommandParseError::InvalidArgument {
            command,
            value: rectangle_argument,
        });
    };
    Ok(AddMonitorArguments { name, rectangle })
}

command_set! {
    domain: b"wm";
    enum WmCommand<'a> {
        DumpState => [b"-d", b"--dump-state"],
        LoadState {
            path: &'a [u8] = raw,
        } => [b"-l", b"--load-state"],
        GetStatus => [b"-g", b"--get-status"],
        RecordHistory {
            enabled: bool = parse(parse_bool_argument),
        } => [b"-h", b"--record-history"],
        AddMonitor {
            arguments: AddMonitorArguments<'a> = custom(parse_add_monitor_arguments),
        } => [b"-a", b"--add-monitor"],
        ReorderMonitors {
            names: Vec<&'a [u8]> = rest1,
        } => [b"-O", b"--reorder-monitors"],
        AdoptOrphans => [b"-o", b"--adopt-orphans"],
        Restart => [b"-r", b"--restart"],
    }
}

impl CommandHandler<'_> {
    pub(super) fn handle_wm(&mut self, args: &[&[u8]], rsp: &mut dyn Response) -> io::Result<()> {
        if args.is_empty() {
            return CommandParseError::MissingCommands.respond(rsp, b"wm");
        }
        let mut cursor = ArgCursor::new(args);
        while let Some(command) = WmCommand::next(&mut cursor, rsp)? {
            match command {
                WmCommand::DumpState => {
                    writeln!(rsp, "{}", query_state(self.state))?;
                }
                WmCommand::LoadState { path } => {
                    let path = Path::new(std::ffi::OsStr::from_bytes(path));
                    let Ok(json) = std::fs::read_to_string(path) else {
                        return fail(rsp, b"");
                    };
                    let Ok(restored) = restore_state(&json, &self.state.settings) else {
                        return fail(rsp, b"");
                    };
                    self.state.pending_effects.push(CommandEffect::LoadState {
                        restored: Box::new(restored),
                    });
                }
                WmCommand::GetStatus => {
                    rsp.write_all(
                        print_report(&self.state.world, &self.state.settings).as_bytes(),
                    )?;
                }
                WmCommand::RecordHistory { enabled } => {
                    self.state.set_record_history(enabled);
                }
                WmCommand::AddMonitor { arguments } => {
                    let AddMonitorArguments { name, rectangle } = arguments;
                    let monitor_external_id = self.state.world.next_external_id();
                    let monitor = self.state.world.create_monitor(
                        monitor_external_id,
                        Some(name),
                        rectangle,
                        &self.state.settings,
                    );
                    let desktop_external_id = self.state.world.next_external_id();
                    let desktop = self.state.world.create_desktop(
                        desktop_external_id,
                        None,
                        &self.state.settings,
                    );
                    self.state.world.add_desktop(monitor, desktop);
                    self.state.pending_effects.extend([
                        CommandEffect::CreateMonitorRoot { monitor },
                        CommandEffect::SyncEwmh,
                    ]);
                }
                WmCommand::ReorderMonitors { names } => {
                    let requested: Vec<_> = names
                        .iter()
                        .filter_map(|name| {
                            text(name).and_then(|name| locate_monitor(&self.state.world, name))
                        })
                        .filter_map(|location| location.monitor)
                        .collect();
                    self.state.world.reorder_monitors(&requested);
                    self.state.pending_effects.push(CommandEffect::SyncEwmh);
                    break;
                }
                WmCommand::AdoptOrphans => {
                    self.state.pending_effects.push(CommandEffect::AdoptOrphans);
                }
                WmCommand::Restart => {
                    self.state.running = false;
                    self.state.restart = true;
                    break;
                }
            }
        }
        Ok(())
    }
}
