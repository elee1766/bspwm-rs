use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::helpers::mktempfifo;
use crate::messages::{Response, Subscription, fail_parts, scan_integer};
use crate::parse::parse_subscriber_mask;
use crate::settings::Settings;
use crate::types::{ClientState, Layout, SubscriberMask};
use crate::world::World;

pub const FIFO_TEMPLATE: &str = "bspwm_fifo.XXXXXX";

#[derive(Debug)]
pub struct Subscriber<W> {
    pub stream: W,
    pub fifo_path: Option<PathBuf>,
    pub field: SubscriberMask,
    pub count: i32,
}

#[must_use]
pub const fn make_subscriber<W>(
    stream: W,
    fifo_path: Option<PathBuf>,
    field: SubscriberMask,
    count: i32,
) -> Subscriber<W> {
    Subscriber {
        stream,
        fifo_path,
        field,
        count,
    }
}

#[derive(Debug)]
pub struct Subscribers<W> {
    entries: Vec<Subscriber<W>>,
}

impl<W> Default for Subscribers<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<W: Write> Subscribers<W> {
    #[must_use]
    pub fn entries(&self) -> &[Subscriber<W>] {
        &self.entries
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn add_subscriber(
        &mut self,
        mut subscriber: Subscriber<W>,
        report: &[u8],
    ) -> io::Result<()> {
        if subscriber.field.contains(SubscriberMask::REPORT) {
            if let Err(error) = subscriber
                .stream
                .write_all(report)
                .and_then(|()| subscriber.stream.flush())
            {
                remove_fifo(&subscriber);
                return Err(error);
            }
            if subscriber.count == 1 {
                remove_fifo(&subscriber);
                return Ok(());
            }
            subscriber.count -= 1;
        }
        self.entries.push(subscriber);
        Ok(())
    }

    pub fn remove_subscriber(&mut self, index: usize, restart: bool) -> Option<Subscriber<W>> {
        if index >= self.entries.len() {
            return None;
        }
        let subscriber = self.entries.remove(index);
        if !restart {
            remove_fifo(&subscriber);
        }
        Some(subscriber)
    }

    pub fn put_status(&mut self, mask: SubscriberMask, status: &[u8], report: &[u8]) {
        let mut index = 0;
        while index < self.entries.len() {
            if !self.entries[index].field.contains(mask) {
                index += 1;
                continue;
            }
            let subscriber = &mut self.entries[index];
            if subscriber.count > 0 {
                subscriber.count -= 1;
            }
            let bytes = if mask == SubscriberMask::REPORT {
                report
            } else {
                status
            };
            let failed = subscriber
                .stream
                .write_all(bytes)
                .and_then(|()| subscriber.stream.flush())
                .is_err();
            if failed || subscriber.count == 0 {
                let subscriber = self.entries.remove(index);
                remove_fifo(&subscriber);
            } else {
                index += 1;
            }
        }
    }

    pub fn prune_dead(&mut self, mut is_dead: impl FnMut(&mut W) -> bool) {
        let mut index = 0;
        while index < self.entries.len() {
            if is_dead(&mut self.entries[index].stream) {
                let subscriber = self.entries.remove(index);
                remove_fifo(&subscriber);
            } else {
                index += 1;
            }
        }
    }

    pub fn clear(&mut self) {
        for subscriber in self.entries.drain(..) {
            remove_fifo(&subscriber);
        }
    }
}

#[allow(clippy::missing_errors_doc)]
pub fn subscribe(args: &[&[u8]], rsp: &mut dyn Response) -> io::Result<Option<Subscription>> {
    let mut mask = SubscriberMask::default();
    let mut count = -1;
    let mut fifo_path = None;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            b"-c" | b"--count" => {
                let option = args[index];
                let Some(value) = args.get(index + 1) else {
                    let result =
                        fail_parts(rsp, &[b"subscribe ", option, b": Not enough arguments.\n"]);
                    remove_path(fifo_path.as_ref());
                    return result.map(|()| None);
                };
                let Some(value_count) = scan_integer(value).filter(|count| *count >= 1) else {
                    let result = fail_parts(
                        rsp,
                        &[
                            b"subscribe ",
                            option,
                            b": Invalid argument: '",
                            value,
                            b"'.\n",
                        ],
                    );
                    remove_path(fifo_path.as_ref());
                    return result.map(|()| None);
                };
                count = value_count;
                index += 2;
            }
            b"-f" | b"--fifo" => {
                if let Ok(path) = mktempfifo(FIFO_TEMPLATE) {
                    remove_path(fifo_path.as_ref());
                    fifo_path = Some(path);
                } else {
                    let result = fail_parts(
                        rsp,
                        &[b"subscribe ", args[index], b": Can't create FIFO.\n"],
                    );
                    remove_path(fifo_path.as_ref());
                    return result.map(|()| None);
                }
                index += 1;
            }
            argument => {
                let Some(field) = std::str::from_utf8(argument)
                    .ok()
                    .and_then(parse_subscriber_mask)
                else {
                    let result =
                        fail_parts(rsp, &[b"subscribe: Invalid argument: '", argument, b"'.\n"]);
                    remove_path(fifo_path.as_ref());
                    return result.map(|()| None);
                };
                mask = mask.union(field);
                index += 1;
            }
        }
    }
    if mask.bits() == 0 {
        mask = SubscriberMask::REPORT;
    }
    let subscription = Subscription {
        mask,
        count,
        fifo_path: fifo_path.clone(),
    };
    if let Some(path) = fifo_path.as_ref()
        && let Err(error) = writeln!(rsp, "{}", path.display()).and_then(|()| rsp.flush())
    {
        remove_path(Some(path));
        return Err(error);
    }
    Ok(Some(subscription))
}

fn remove_path(path: Option<&PathBuf>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

fn remove_fifo<W>(subscriber: &Subscriber<W>) {
    if let Some(path) = &subscriber.fifo_path {
        let _ = fs::remove_file(path);
    }
}

#[must_use]
pub fn print_report(world: &World, settings: &Settings) -> Vec<u8> {
    let mut report = Vec::from(settings.status_prefix.as_bytes());
    for (monitor_index, monitor_id) in world.monitor_order().iter().copied().enumerate() {
        let monitor = world.monitor(monitor_id);
        let monitor_char = if world.focused_monitor == Some(monitor_id) {
            b'M'
        } else {
            b'm'
        };
        report.push(monitor_char);
        report.extend_from_slice(monitor.name.as_bytes());
        for desktop_id in &monitor.desktops {
            let desktop = world.desktop(*desktop_id);
            let urgent = world.desktop_is_urgent(*desktop_id);
            let mut state = if urgent {
                b'u'
            } else if desktop.tree.root.is_none() {
                b'f'
            } else {
                b'o'
            };
            if monitor.active_desktop == Some(*desktop_id) {
                state = state.to_ascii_uppercase();
            }
            report.push(b':');
            report.push(state);
            report.extend_from_slice(desktop.name.as_bytes());
        }
        if let Some(desktop_id) = monitor.active_desktop {
            let desktop = world.desktop(desktop_id);
            let layout_char = match desktop.layout {
                Layout::Tiled => b'T',
                Layout::Monocle => b'M',
            };
            report.extend_from_slice(b":L");
            report.push(layout_char);
            if let Some(node_id) = desktop.tree.focus {
                let node = world.tree.node(node_id);
                let state_char = node
                    .client
                    .as_ref()
                    .map_or(b'@', |client| match client.state {
                        ClientState::Tiled => b'T',
                        ClientState::PseudoTiled => b'P',
                        ClientState::Floating => b'F',
                        ClientState::Fullscreen => b'=',
                    });
                report.extend_from_slice(b":T");
                report.push(state_char);
                report.extend_from_slice(b":G");
                if node.sticky {
                    report.push(b'S');
                }
                if node.private {
                    report.push(b'P');
                }
                if node.locked {
                    report.push(b'L');
                }
                if node.marked {
                    report.push(b'M');
                }
            }
        }
        if monitor_index + 1 < world.monitor_order().len() {
            report.push(b':');
        }
    }
    report.push(b'\n');
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Response;
    use crate::tree::Client;
    use crate::types::Rectangle;

    #[derive(Default)]
    struct TestResponse {
        bytes: Vec<u8>,
        fail_flush: bool,
    }

    impl Write for TestResponse {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::other("flush failed"))
            } else {
                Ok(())
            }
        }
    }

    impl Response for TestResponse {
        fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn report_follows_upstream_monitor_desktop_layout_and_focus_format() {
        let settings = Settings::default();
        let mut world = World::default();
        let monitor =
            world.create_monitor(1, Some("one"), Rectangle::new(0, 0, 100, 100), &settings);
        let desktop = world.create_desktop(2, Some("I"), &settings);
        world.add_desktop(monitor, desktop);
        let node = world.tree.add_node(3, 0.5);
        world.tree.node_mut(node).client = Some(Client::from_settings(&settings));
        world.desktop_mut(desktop).tree.root = Some(node);
        world.desktop_mut(desktop).tree.focus = Some(node);
        assert_eq!(print_report(&world, &settings), b"WMone:OI:LT:TT:G\n");
    }

    #[test]
    fn report_subscription_is_written_immediately_and_consumes_count() {
        let subscriber = make_subscriber(Vec::new(), None, SubscriberMask::REPORT, 2);
        let mut subscribers = Subscribers::default();
        subscribers.add_subscriber(subscriber, b"report\n").unwrap();
        assert_eq!(subscribers.entries()[0].count, 1);
        assert_eq!(subscribers.entries()[0].stream, b"report\n");
        subscribers.put_status(SubscriberMask::REPORT, b"ignored", b"next\n");
        assert!(subscribers.entries().is_empty());
    }

    #[test]
    fn subscribe_returns_explicit_metadata() {
        let mut response = TestResponse::default();
        let subscription = subscribe(&[b"node_add", b"--count", b"2"], &mut response)
            .unwrap()
            .unwrap();
        assert_eq!(subscription.mask, SubscriberMask::NODE_ADD);
        assert_eq!(subscription.count, 2);
        assert_eq!(subscription.fifo_path, None);
        assert!(response.bytes.is_empty());
    }

    #[test]
    fn fifo_path_is_removed_when_writing_it_fails() {
        let mut response = TestResponse {
            fail_flush: true,
            ..TestResponse::default()
        };
        assert!(subscribe(&[b"--fifo"], &mut response).is_err());
        let path = PathBuf::from(String::from_utf8(response.bytes).unwrap().trim_end());
        assert!(!path.exists());
    }
}
