use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use xcb::{Xid, x};

const DEFAULT_COLOR: u32 = 0x40_5D_80;
const INPUT_HINT: u32 = 1;
const URGENCY_HINT: u32 = 1 << 8;
const P_MIN_SIZE: u32 = 1 << 4;
const P_MAX_SIZE: u32 = 1 << 5;

#[derive(Default)]
struct Options {
    control: Option<PathBuf>,
    events: Option<PathBuf>,
    delete_window: bool,
    take_focus: bool,
    input: Option<bool>,
    override_redirect: bool,
}

struct Atoms {
    wm_protocols: x::Atom,
    wm_delete_window: x::Atom,
    wm_take_focus: x::Atom,
    wm_normal_hints: x::Atom,
    wm_size_hints: x::Atom,
    net_wm_strut_partial: x::Atom,
}

enum CommandResult {
    Continue,
    Quit,
}

fn intern_atom(connection: &xcb::Connection, name: &[u8]) -> Result<x::Atom, xcb::Error> {
    let cookie = connection.send_request(&x::InternAtom {
        only_if_exists: false,
        name,
    });
    Ok(connection.wait_for_reply(cookie)?.atom())
}

fn parse_color(value: Option<String>) -> Result<u32, Box<dyn Error>> {
    let Some(value) = value else {
        return Ok(DEFAULT_COLOR);
    };
    let digits = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(&value);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("invalid color '{value}': expected #RRGGBB").into());
    }
    Ok(u32::from_str_radix(digits, 16)?)
}

fn parse_arguments() -> Result<(String, String, u32, Options), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1).peekable();
    let mut positional = Vec::new();
    while positional.len() < 3 && arguments.peek().is_some_and(|arg| !arg.starts_with("--")) {
        positional.push(arguments.next().expect("peeked argument must exist"));
    }

    let instance_name = positional.first().cloned().unwrap_or_else(|| "test".into());
    let class_name = positional.get(1).cloned().unwrap_or_else(|| "Test".into());
    let color = parse_color(positional.get(2).cloned())?;
    let mut options = Options {
        delete_window: true,
        ..Options::default()
    };
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--control" => {
                options.control = Some(arguments.next().ok_or("--control requires PATH")?.into());
            }
            "--events" => {
                options.events = Some(arguments.next().ok_or("--events requires PATH")?.into());
            }
            "--no-delete-window" => options.delete_window = false,
            "--take-focus" => options.take_focus = true,
            "--input" => {
                options.input = Some(match arguments.next().as_deref() {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err("--input requires true or false".into()),
                });
            }
            "--override-redirect" => options.override_redirect = true,
            _ => return Err(format!("unknown argument '{argument}'").into()),
        }
    }
    Ok((instance_name, class_name, color, options))
}

fn change_wm_hints(
    connection: &xcb::Connection,
    window: x::Window,
    input: Option<bool>,
    urgent: bool,
) {
    let mut hints = [0_u32; 9];
    if let Some(input) = input {
        hints[0] |= INPUT_HINT;
        hints[1] = u32::from(input);
    }
    if urgent {
        hints[0] |= URGENCY_HINT;
    }
    connection.send_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property: x::ATOM_WM_HINTS,
        r#type: x::ATOM_WM_HINTS,
        data: &hints,
    });
}

fn advertise_protocols(
    connection: &xcb::Connection,
    window: x::Window,
    atoms: &Atoms,
    delete_window: bool,
    take_focus: bool,
) {
    let mut protocols = Vec::with_capacity(2);
    if delete_window {
        protocols.push(atoms.wm_delete_window.resource_id());
    }
    if take_focus {
        protocols.push(atoms.wm_take_focus.resource_id());
    }
    connection.send_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property: atoms.wm_protocols,
        r#type: x::ATOM_ATOM,
        data: &protocols,
    });
}

fn log_line(events: &mut Option<BufWriter<File>>, line: &str) -> io::Result<()> {
    if let Some(events) = events {
        writeln!(events, "{line}")?;
        events.flush()?;
    }
    Ok(())
}

fn set_normal_hints(
    connection: &xcb::Connection,
    window: x::Window,
    atoms: &Atoms,
    parts: &[&str],
) -> Result<(), String> {
    if parts.len() != 5 {
        return Err("normal-hints requires 4 values".into());
    }
    let values: Vec<u32> = parts[1..]
        .iter()
        .map(|value| {
            value
                .parse()
                .map_err(|_| format!("invalid integer '{value}'"))
        })
        .collect::<Result<_, _>>()?;
    let mut hints = [0_u32; 18];
    hints[0] = P_MIN_SIZE | P_MAX_SIZE;
    hints[5..=8].copy_from_slice(&values);
    connection.send_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property: atoms.wm_normal_hints,
        r#type: atoms.wm_size_hints,
        data: &hints,
    });
    Ok(())
}

fn set_strut(
    connection: &xcb::Connection,
    window: x::Window,
    atoms: &Atoms,
    parts: &[&str],
) -> Result<(), String> {
    if parts.len() != 13 {
        return Err("strut requires 12 values".into());
    }
    let values: Vec<u32> = parts[1..]
        .iter()
        .map(|value| {
            value
                .parse()
                .map_err(|_| format!("invalid integer '{value}'"))
        })
        .collect::<Result<_, _>>()?;
    connection.send_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property: atoms.net_wm_strut_partial,
        r#type: x::ATOM_CARDINAL,
        data: &values,
    });
    Ok(())
}

fn process_command(
    connection: &xcb::Connection,
    window: x::Window,
    atoms: &Atoms,
    input: Option<bool>,
    line: &str,
) -> Result<CommandResult, String> {
    let parts: Vec<_> = line.split_whitespace().collect();
    let Some(command) = parts.first().copied() else {
        return Ok(CommandResult::Continue);
    };
    match command {
        "configure" => {
            if parts.len() != 5 {
                return Err("configure requires X Y WIDTH HEIGHT".into());
            }
            let x_position: i32 = parts[1]
                .parse()
                .map_err(|_| format!("invalid integer '{}'", parts[1]))?;
            let y_position: i32 = parts[2]
                .parse()
                .map_err(|_| format!("invalid integer '{}'", parts[2]))?;
            let width: u32 = parts[3]
                .parse()
                .map_err(|_| format!("invalid integer '{}'", parts[3]))?;
            let height: u32 = parts[4]
                .parse()
                .map_err(|_| format!("invalid integer '{}'", parts[4]))?;
            connection.send_request(&x::ConfigureWindow {
                window,
                value_list: &[
                    x::ConfigWindow::X(x_position),
                    x::ConfigWindow::Y(y_position),
                    x::ConfigWindow::Width(width),
                    x::ConfigWindow::Height(height),
                ],
            });
        }
        "map" => {
            connection.send_request(&x::MapWindow { window });
        }
        "unmap" => {
            connection.send_request(&x::UnmapWindow { window });
        }
        "destroy" => {
            connection.send_request(&x::DestroyWindow { window });
        }
        "focus" => {
            connection.send_request(&x::SetInputFocus {
                revert_to: x::InputFocus::Parent,
                focus: window,
                time: x::CURRENT_TIME,
            });
        }
        "barrier" => {
            if parts.len() != 1 {
                return Err("barrier takes no arguments".into());
            }
            let cookie = connection.send_request(&x::GetGeometry {
                drawable: x::Drawable::Window(window),
            });
            connection
                .wait_for_reply(cookie)
                .map_err(|error| format!("barrier failed: {error}"))?;
        }
        "urgency" => {
            if parts.len() != 2 {
                return Err("urgency requires on or off".into());
            }
            let urgent = match parts[1] {
                "on" => true,
                "off" => false,
                _ => return Err("urgency requires on or off".into()),
            };
            change_wm_hints(connection, window, input, urgent);
        }
        "normal-hints" => {
            set_normal_hints(connection, window, atoms, &parts)?;
        }
        "strut" => {
            set_strut(connection, window, atoms, &parts)?;
        }
        "delete-property" => {
            if parts.len() != 2 {
                return Err("delete-property requires ATOM_NAME".into());
            }
            let property = intern_atom(connection, parts[1].as_bytes())
                .map_err(|error| format!("could not intern atom: {error}"))?;
            connection.send_request(&x::DeleteProperty { window, property });
        }
        "quit" => return Ok(CommandResult::Quit),
        _ => return Err(format!("unknown command '{command}'")),
    }
    Ok(CommandResult::Continue)
}

fn handle_event(
    connection: &xcb::Connection,
    window: x::Window,
    atoms: &Atoms,
    event: xcb::Event,
    events: &mut Option<BufWriter<File>>,
) -> Result<bool, Box<dyn Error>> {
    let xcb::Event::X(event) = event else {
        return Ok(false);
    };
    match event {
        x::Event::ClientMessage(event)
            if event.window() == window && event.r#type() == atoms.wm_protocols =>
        {
            let x::ClientMessageData::Data32(data) = event.data() else {
                return Ok(false);
            };
            if data[0] == atoms.wm_delete_window.resource_id() {
                log_line(events, "client-message WM_DELETE_WINDOW")?;
                return Ok(true);
            }
            if data[0] == atoms.wm_take_focus.resource_id() {
                log_line(events, "client-message WM_TAKE_FOCUS")?;
                connection.send_request(&x::SetInputFocus {
                    revert_to: x::InputFocus::Parent,
                    focus: window,
                    time: data[1],
                });
                connection.flush()?;
            }
        }
        x::Event::ConfigureNotify(event) if event.window() == window => {
            log_line(
                events,
                &format!(
                    "configure-notify x={} y={} width={} height={} border={} synthetic={}",
                    event.x(),
                    event.y(),
                    event.width(),
                    event.height(),
                    event.border_width(),
                    event.response_type() & 0x80 != 0
                ),
            )?;
        }
        x::Event::FocusIn(event) if event.event() == window => {
            log_line(events, "focus-in")?;
        }
        x::Event::FocusOut(event) if event.event() == window => {
            log_line(events, "focus-out")?;
        }
        x::Event::MapNotify(event) if event.window() == window => {
            log_line(events, "map-notify")?;
        }
        x::Event::UnmapNotify(event) if event.window() == window => {
            log_line(events, "unmap-notify")?;
        }
        x::Event::DestroyNotify(event) if event.window() == window => {
            log_line(events, "destroy-notify")?;
            return Ok(true);
        }
        x::Event::Expose(event) if event.window() == window => {
            connection.send_request(&x::ClearArea {
                exposures: false,
                window,
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
            connection.flush()?;
        }
        _ => {}
    }
    Ok(false)
}

fn run_event_loop(
    connection: &xcb::Connection,
    window: x::Window,
    atoms: &Atoms,
    input: Option<bool>,
    control: &mut Option<BufReader<File>>,
    events: &mut Option<BufWriter<File>>,
) -> Result<(), Box<dyn Error>> {
    if let Some(reader) = control.as_mut() {
        let mut pending = String::new();
        let mut quit = false;
        while !quit {
            let mut did_work = false;
            let mut chunk = String::new();
            // A BufReader remains usable at EOF; retrying observes bytes appended later.
            if reader.read_line(&mut chunk)? != 0 {
                did_work = true;
                pending.push_str(&chunk);
                if pending.ends_with('\n') {
                    let command = pending.trim_end_matches(['\r', '\n']).to_owned();
                    pending.clear();
                    match process_command(connection, window, atoms, input, &command) {
                        Ok(CommandResult::Continue) => {
                            connection.flush()?;
                            while let Some(event) = connection.poll_for_event()? {
                                if handle_event(connection, window, atoms, event, events)? {
                                    quit = true;
                                    break;
                                }
                            }
                            log_line(events, &format!("command {command} ok"))?;
                        }
                        Ok(CommandResult::Quit) => {
                            log_line(events, "command quit ok")?;
                            quit = true;
                        }
                        Err(error) => {
                            log_line(events, &format!("command {command} error: {error}"))?;
                        }
                    }
                }
            }
            while let Some(event) = connection.poll_for_event()? {
                did_work = true;
                if handle_event(connection, window, atoms, event, events)? {
                    quit = true;
                    break;
                }
            }
            if !did_work && !quit {
                thread::sleep(Duration::from_millis(10));
            }
        }
    } else {
        while let Ok(event) = connection.wait_for_event() {
            if handle_event(connection, window, atoms, event, events)? {
                break;
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let (instance_name, class_name, color, options) = parse_arguments()?;
    let mut events = options
        .events
        .as_ref()
        .map(|path| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map(BufWriter::new)
        })
        .transpose()?;
    let mut control = options
        .control
        .as_ref()
        .map(File::open)
        .transpose()?
        .map(BufReader::new);

    let (connection, screen_number) = xcb::Connection::connect(None)?;
    let screen = connection
        .get_setup()
        .roots()
        .nth(usize::try_from(screen_number)?)
        .ok_or("X server has no selected screen")?;
    let window = connection.generate_id::<x::Window>();
    let atoms = Atoms {
        wm_protocols: intern_atom(&connection, b"WM_PROTOCOLS")?,
        wm_delete_window: intern_atom(&connection, b"WM_DELETE_WINDOW")?,
        wm_take_focus: intern_atom(&connection, b"WM_TAKE_FOCUS")?,
        wm_normal_hints: intern_atom(&connection, b"WM_NORMAL_HINTS")?,
        wm_size_hints: intern_atom(&connection, b"WM_SIZE_HINTS")?,
        net_wm_strut_partial: intern_atom(&connection, b"_NET_WM_STRUT_PARTIAL")?,
    };
    let wm_class_atom = intern_atom(&connection, b"WM_CLASS")?;

    connection.send_request(&x::CreateWindow {
        depth: u8::try_from(x::COPY_FROM_PARENT)?,
        wid: window,
        parent: screen.root(),
        x: 32,
        y: 32,
        width: 320,
        height: 240,
        border_width: 0,
        class: x::WindowClass::InputOutput,
        visual: x::COPY_FROM_PARENT,
        value_list: &[
            x::Cw::BackPixel(color),
            x::Cw::OverrideRedirect(options.override_redirect),
            x::Cw::EventMask(
                x::EventMask::STRUCTURE_NOTIFY
                    | x::EventMask::FOCUS_CHANGE
                    | x::EventMask::PROPERTY_CHANGE
                    | x::EventMask::EXPOSURE,
            ),
        ],
    });

    let mut wm_class = Vec::with_capacity(instance_name.len() + class_name.len() + 2);
    wm_class.extend_from_slice(instance_name.as_bytes());
    wm_class.push(0);
    wm_class.extend_from_slice(class_name.as_bytes());
    wm_class.push(0);
    connection.send_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property: wm_class_atom,
        r#type: x::ATOM_STRING,
        data: &wm_class,
    });
    advertise_protocols(
        &connection,
        window,
        &atoms,
        options.delete_window,
        options.take_focus,
    );
    if options.input.is_some() {
        change_wm_hints(&connection, window, options.input, false);
    }
    connection.send_request(&x::ChangeProperty {
        mode: x::PropMode::Replace,
        window,
        property: x::ATOM_WM_NAME,
        r#type: x::ATOM_STRING,
        data: format!("{class_name}:{instance_name}  #{color:06X}").as_bytes(),
    });
    connection.send_request(&x::MapWindow { window });
    connection.flush()?;

    println!("0x{:08X}", window.resource_id());
    io::stdout().flush()?;

    run_event_loop(
        &connection,
        window,
        &atoms,
        options.input,
        &mut control,
        &mut events,
    )?;

    connection.send_request(&x::DestroyWindow { window });
    connection.flush()?;
    Ok(())
}
