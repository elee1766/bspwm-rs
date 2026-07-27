use std::error::Error;

use bspwm::window::send_client_message;
use bspwm::x11::X11;
use xcb::{Xid, XidNew, x};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().ok_or("missing EWMH command")?;
    let x11 = X11::connect(None)?;
    if command == "query-focus" {
        if arguments.next().is_some() {
            return Err("query-focus takes no arguments".into());
        }
        let cookie = x11.connection().send_request(&x::GetInputFocus {});
        let focus = x11.connection().wait_for_reply(cookie)?.focus();
        println!("0x{:08X}", focus.resource_id());
        return Ok(());
    }
    let root = x11.root();
    let mask = x::EventMask::SUBSTRUCTURE_REDIRECT | x::EventMask::SUBSTRUCTURE_NOTIFY;

    let (window, message_type, data) = match command.as_str() {
        "desktop" => {
            let desktop = parse_u32(&required(&mut arguments, "desktop index")?)?;
            (
                root,
                x11.atoms().net_current_desktop,
                [desktop, x::CURRENT_TIME, 0, 0, 0],
            )
        }
        "active" => {
            let window = parse_window(&required(&mut arguments, "window")?)?;
            (
                window,
                x11.atoms().net_active_window,
                [1, x::CURRENT_TIME, 0, 0, 0],
            )
        }
        "move" => {
            let window = parse_window(&required(&mut arguments, "window")?)?;
            let desktop = parse_u32(&required(&mut arguments, "desktop index")?)?;
            (window, x11.atoms().net_wm_desktop, [desktop, 1, 0, 0, 0])
        }
        "fullscreen" => {
            let window = parse_window(&required(&mut arguments, "window")?)?;
            let action = match required(&mut arguments, "state action")?.as_str() {
                "remove" => 0,
                "add" => 1,
                "toggle" => 2,
                _ => return Err("state action must be remove, add, or toggle".into()),
            };
            (
                window,
                x11.atoms().net_wm_state,
                [
                    action,
                    x11.atoms().net_wm_state_fullscreen.resource_id(),
                    0,
                    1,
                    0,
                ],
            )
        }
        "close" => {
            let window = parse_window(&required(&mut arguments, "window")?)?;
            (
                window,
                x11.atoms().net_close_window,
                [x::CURRENT_TIME, 1, 0, 0, 0],
            )
        }
        _ => return Err("unknown EWMH command".into()),
    };
    if arguments.next().is_some() {
        return Err("too many arguments".into());
    }

    send_client_message(&x11, root, window, message_type, data, mask)?;
    x11.flush()?;
    Ok(())
}

fn required(arguments: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    arguments.next().ok_or_else(|| format!("missing {name}"))
}

fn parse_window(value: &str) -> Result<x::Window, Box<dyn Error>> {
    Ok(x::Window::new(parse_u32(value)?))
}

fn parse_u32(value: &str) -> Result<u32, Box<dyn Error>> {
    Ok(if let Some(hexadecimal) = value.strip_prefix("0x") {
        u32::from_str_radix(hexadecimal, 16)?
    } else {
        value.parse()?
    })
}
