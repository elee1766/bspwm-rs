//! Restart/restore persistence: subscriber outputs, state files, and fd plumbing.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use xcb::{Xid, XidNew, x};

use super::DaemonApp;
use super::action::XAction;
use crate::ewmh;
use crate::messages::{Response, Subscription};
use crate::monitor;
use crate::restore;
use crate::runtime::{InheritedFds, RuntimeError, UnixResponse};
use crate::subscribe::{make_subscriber, print_report};
use crate::types::SubscriberMask;
use crate::window;
use crate::x11::X11;

#[derive(Debug)]
pub(super) enum SubscriberOutput {
    Socket(UnixResponse),
    Fifo(File),
}

impl SubscriberOutput {
    fn writer(&mut self) -> &mut dyn Write {
        match self {
            Self::Socket(stream) => stream,
            Self::Fifo(stream) => stream,
        }
    }
}

impl AsFd for SubscriberOutput {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Socket(stream) => stream.as_fd(),
            Self::Fifo(stream) => stream.as_fd(),
        }
    }
}

impl Write for SubscriberOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer().flush()
    }
}

impl DaemonApp {
    pub(super) fn load_state(
        &mut self,
        path: &Path,
        x11: &X11,
        restore_subscribers: bool,
    ) -> Result<(), RuntimeError> {
        let json = std::fs::read_to_string(path)?;
        let restored = restore::restore_state(&json, &self.state.settings).map_err(|error| {
            RuntimeError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                error.to_string(),
            ))
        })?;
        self.reconstruct_state(restored, x11, restore_subscribers)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn reconstruct_state(
        &mut self,
        mut restored: restore::RestoredState,
        x11: &X11,
        restore_subscribers: bool,
    ) -> Result<(), RuntimeError> {
        restored.regenerate_xids(|| x11.connection().generate_id::<x::Window>().resource_id());

        crate::pointer::ungrab_buttons(x11, self.all_client_windows())?;
        for feedback in self.tree().feedback_windows() {
            window::destroy(x11, x::Window::new(feedback))?;
        }
        self.mapped_feedbacks.clear();
        for monitor in self.world().monitor_order() {
            if let Some(root) = self.world().monitor(*monitor).root_id {
                monitor::destroy_monitor_root(x11, root)?;
            }
        }
        if restore_subscribers {
            self.restored_subscribers = std::mem::take(&mut restored.event_subscribers);
        }
        // The restored state replaces the world wholesale, so no cached
        // `window -> location` entry survives it.
        self.invalidate_window_index();
        self.state.apply_restored(restored);
        self.last_user_time = None;
        self.user_time_windows.clear();
        self.sync_request_clients.clear();
        self.pending_ewmh_pings.clear();
        for id in self.world().monitor_order().to_vec() {
            let value = self.world().monitor(id);
            let root = monitor::create_monitor_root(
                x11,
                &value.name,
                value.rectangle,
                self.state.settings.focus_follows_pointer,
            )?;
            self.world_mut().monitor_mut(id).root_id = Some(root);
        }
        let clients = self.all_client_nodes();
        let focused = self
            .world()
            .focused_monitor
            .and_then(|monitor| self.world().monitor(monitor).active_desktop)
            .and_then(|desktop| self.world().desktop(desktop).tree.focus);
        for node in clients.iter().copied() {
            let window_id = self.xid(node);
            let window = x::Window::new(window_id);
            let protocols: Vec<u32> =
                window::get_property(x11, window, x11.atoms().wm_protocols, x::ATOM_ATOM)
                    .unwrap_or_default();
            let states: Vec<u32> =
                window::get_property(x11, window, x11.atoms().net_wm_state, x::ATOM_ATOM)
                    .unwrap_or_default();
            let hints = window::wm_hints(x11, window).unwrap_or(window::WmHints {
                input: None,
                urgent: false,
            });
            let direct_user_time = window::get_property::<u32>(
                x11,
                window,
                x11.atoms().net_wm_user_time,
                x::ATOM_CARDINAL,
            )
            .unwrap_or_default()
            .first()
            .copied();
            let user_time_window = window::get_property::<u32>(
                x11,
                window,
                x11.atoms().net_wm_user_time_window,
                x::ATOM_WINDOW,
            )
            .unwrap_or_default()
            .first()
            .copied()
            .filter(|auxiliary| {
                *auxiliary != x::WINDOW_NONE.resource_id() && *auxiliary != window_id
            });
            let user_time = user_time_window
                .and_then(|auxiliary| {
                    window::get_property::<u32>(
                        x11,
                        x::Window::new(auxiliary),
                        x11.atoms().net_wm_user_time,
                        x::ATOM_CARDINAL,
                    )
                    .ok()
                    .and_then(|values| values.first().copied())
                })
                .or(direct_user_time);
            if let Some(auxiliary) = user_time_window
                && window::listen_for_property_changes(x11, x::Window::new(auxiliary)).is_ok()
            {
                self.user_time_windows.insert(auxiliary, window_id);
            }
            if focused == Some(node)
                && let Some(user_time) = user_time
            {
                self.note_user_time(user_time);
            }
            let client = self.client_mut(node);
            client.icccm.input_hint = hints.input.unwrap_or(true);
            client.icccm.take_focus = protocols.contains(&x11.atoms().wm_take_focus.resource_id());
            client.icccm.delete_window =
                protocols.contains(&x11.atoms().wm_delete_window.resource_id());
            client.icccm.ping = protocols.contains(&x11.atoms().net_wm_ping.resource_id());
            client.size_hints = window::normal_hints(x11, window).unwrap_or_default();
            client.wm_flags = ewmh::wm_flags_from_ids(&states, x11.atoms());
            if let Some(counter) = window::sync_request_counter(x11, window) {
                self.sync_request_clients.insert(window_id, counter);
            }
            Self::execute_action(
                x11,
                XAction::SetClientEventMask {
                    window: window_id,
                    enter_window: self.state.settings.focus_follows_pointer,
                },
            )?;
            crate::pointer::grab_client_buttons(
                x11,
                window,
                &self.state.settings,
                self.lock_masks,
            )?;
        }
        self.arrange_all(x11)?;
        for monitor in self.world().monitor_order().to_vec() {
            let active = self.world().monitor(monitor).active_desktop;
            for desktop in self.world().monitor(monitor).desktops.clone() {
                if let Some(root) = self.world().desktop(desktop).tree.root {
                    self.set_subtree_visibility(x11, desktop, root, active == Some(desktop))?;
                }
            }
        }
        for node in clients {
            self.sync_window_state(x11, node)?;
        }
        self.refresh_colors(x11)?;
        // Restack all visible desktops so the X server matches the restored
        // model stacking order. Without this, windows appear in map order
        // after restart instead of their saved stacking positions.
        for monitor in self.world().monitor_order().to_vec() {
            if let Some(desktop) = self.world().monitor(monitor).active_desktop
                && let Some(root) = self.world().desktop(desktop).tree.root
            {
                let focused = self.world().desktop(desktop).tree.focus;
                let actions = self.state.stacking_order.stack(
                    &self.state.world.tree,
                    root,
                    focused.is_some(),
                    self.state.auto_raise,
                );
                self.execute_restacks(x11, desktop, &actions)?;
            }
        }
        self.update_ewmh(x11)?;
        let focus = self
            .world()
            .focused_monitor
            .and_then(|monitor| self.world().monitor(monitor).active_desktop)
            .and_then(|desktop| self.world().desktop(desktop).tree.focus);
        self.apply_focus(x11, focus)?;
        Ok(())
    }

    pub(super) fn restore_inherited_subscribers(
        &mut self,
        fds: &mut InheritedFds,
    ) -> Result<(), RuntimeError> {
        let restored = std::mem::take(&mut self.restored_subscribers);
        for subscriber in restored {
            let field = SubscriberMask::from_bits_retain(subscriber.field);
            let output = if let Some(path) = subscriber.fifo_path.as_deref() {
                let file = OpenOptions::new()
                    .write(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open(path)?;
                // The path reopened the FIFO safely, so release the inherited
                // description. Only descriptors the validated table actually
                // handed us may be closed.
                fds.close_inherited(subscriber.file_descriptor)?;
                SubscriberOutput::Fifo(file)
            } else {
                SubscriberOutput::Socket(UnixResponse::new(
                    fds.take_unix_stream(subscriber.file_descriptor)?,
                ))
            };
            let report = print_report(self.world(), &self.state.settings);
            self.subscribers.add_subscriber(
                make_subscriber(
                    output,
                    subscriber.fifo_path.map(Into::into),
                    field,
                    subscriber.count,
                ),
                report.as_bytes(),
            )?;
        }
        Ok(())
    }

    pub(super) fn write_restart_state(&mut self, path: &Path) -> Result<Vec<RawFd>, RuntimeError> {
        let mut state: serde_json::Value =
            serde_json::from_str(&crate::query::query_state(&self.state))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut descriptors = Vec::with_capacity(self.subscribers.entries().len());
        let subscribers = self
            .subscribers
            .entries()
            .iter()
            .map(|subscriber| {
                let fd = subscriber.stream.as_fd().as_raw_fd();
                set_output_inheritable(&subscriber.stream)?;
                descriptors.push(fd);
                let mut value = serde_json::json!({
                    "fileDescriptor": fd,
                    "field": subscriber.field.bits(),
                    "count": subscriber.count,
                });
                if let Some(path) = &subscriber.fifo_path {
                    value["fifoPath"] = serde_json::Value::String(path.to_string_lossy().into());
                }
                Ok(value)
            })
            .collect::<io::Result<Vec<_>>>()?;
        state["eventSubscribers"] = serde_json::Value::Array(subscribers);
        let bytes = serde_json::to_vec(&state).map_err(io::Error::other)?;
        std::fs::write(path, bytes)?;
        Ok(descriptors)
    }

    pub(super) fn retain_response(
        &mut self,
        mut response: UnixResponse,
        subscription: Subscription,
    ) -> Result<(), RuntimeError> {
        let (output, fifo_path) = if let Some(path) = subscription.fifo_path {
            if let Err(error) = response.close() {
                let _ = std::fs::remove_file(&path);
                return Err(error.into());
            }
            let stream = match OpenOptions::new().write(true).open(&path) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = std::fs::remove_file(&path);
                    return Err(error.into());
                }
            };
            (SubscriberOutput::Fifo(stream), Some(path))
        } else {
            (SubscriberOutput::Socket(response), None)
        };
        let report = print_report(self.world(), &self.state.settings);
        self.subscribers.add_subscriber(
            make_subscriber(output, fifo_path, subscription.mask, subscription.count),
            report.as_bytes(),
        )?;
        Ok(())
    }

    pub(super) fn prune_dead_subscribers(&mut self) {
        self.subscribers.prune_dead(|output| match output {
            SubscriberOutput::Socket(response) => response.peer_disconnected(),
            SubscriberOutput::Fifo(_) => false,
        });
    }
}

fn set_output_inheritable(output: &SubscriberOutput) -> io::Result<()> {
    let flags = nix::fcntl::FdFlag::from_bits_truncate(
        nix::fcntl::fcntl(output, nix::fcntl::FcntlArg::F_GETFD).map_err(io::Error::from)?,
    );
    nix::fcntl::fcntl(
        output,
        nix::fcntl::FcntlArg::F_SETFD(flags - nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .map(|_| ())
    .map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::daemon::test_support::{
        TestResponse, app_with_desktop, emit_pending_broadcasts, read_available, subscribe_socket,
        test_path,
    };
    use crate::messages::{Domain, MessageHandler};
    use crate::state::CommandEffect;

    #[test]
    fn unix_report_subscriptions_emit_immediately_and_retain_the_expected_count() {
        let cases: &[(&str, &[u8], usize)] = &[
            ("default", b"subscribe\0", 1),
            ("count one", b"subscribe\x00-c\x001\x00", 0),
        ];
        for &(label, request, retained_subscribers) in cases {
            let (mut app, _, _) = app_with_desktop();
            let mut client = subscribe_socket(&mut app, request);
            assert_eq!(
                read_available(&mut client),
                b"WMmonitor:FI:LT\n",
                "{label} report"
            );
            assert_eq!(
                app.subscriber_count(),
                retained_subscribers,
                "{label} retained subscribers"
            );
        }
    }

    #[test]
    fn restart_state_includes_subscribers_history_and_stacking() {
        let (mut app, _, _) = app_with_desktop();
        let _client = subscribe_socket(&mut app, b"subscribe\0node\0");
        let path = test_path("restart-state");
        let descriptors = app.write_restart_state(&path).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(value["focusHistory"].is_array());
        assert!(value["stackingList"].is_array());
        assert_eq!(value["eventSubscribers"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["eventSubscribers"][0]["fileDescriptor"],
            descriptors[0]
        );
        assert_eq!(
            value["eventSubscribers"][0]["field"],
            SubscriberMask::NODE.bits()
        );
        assert!(value["eventSubscribers"][0].get("fifoPath").is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn load_state_queue_keeps_existing_command_subscribers() {
        let (mut app, _, _) = app_with_desktop();
        let _client = subscribe_socket(&mut app, b"subscribe\0node\0");
        let path = test_path("queued-load-state");
        std::fs::write(&path, crate::query::query_state(&app.state)).unwrap();
        app.dispatch(
            Domain::Wm,
            &[b"--load-state", path.as_os_str().as_encoded_bytes()],
            &mut TestResponse::default(),
        )
        .unwrap();
        assert_eq!(app.subscriber_count(), 1);
        assert!(matches!(
            app.state.pending_effects.last(),
            Some(CommandEffect::LoadState { .. })
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn disconnected_unix_subscribers_are_pruned() {
        let (mut app, _, _) = app_with_desktop();
        let client = subscribe_socket(&mut app, b"subscribe\0node\0");
        assert_eq!(app.subscriber_count(), 1);
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(1);
        while app.subscriber_count() != 0 && Instant::now() < deadline {
            app.prune_dead_subscribers();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.subscriber_count(), 0);
    }

    #[test]
    fn fifo_subscription_reports_path_streams_and_unlinks_on_count() {
        let (mut app, _, _) = app_with_desktop();
        let (server, mut control) = UnixStream::pair().unwrap();
        control
            .write_all(b"subscribe\x00--fifo\x00--count\x002\x00report\x00monitor_rename\x00")
            .unwrap();
        let server = thread::spawn(move || {
            let (outcome, response) = crate::runtime::handle_stream(
                server,
                &mut app,
                0,
                crate::bspc::BUFFER_SIZE,
                Duration::from_secs(1),
            )
            .unwrap();
            let crate::messages::MessageOutcome::Subscribe(subscription) = outcome else {
                panic!("expected subscription outcome");
            };
            app.retain_response(response, subscription).unwrap();
            app
        });

        let mut path = String::new();
        control.read_to_string(&mut path).unwrap();
        let path = std::path::PathBuf::from(path.trim_end());
        let mut fifo = OpenOptions::new().read(true).open(&path).unwrap();
        let mut app = server.join().unwrap();

        let report = b"WMmonitor:FI:LT\n";
        let mut bytes = vec![0; report.len()];
        fifo.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, report);
        app.dispatch(
            Domain::Monitor,
            &[b"focused", b"--rename", b"new"],
            &mut TestResponse::default(),
        )
        .unwrap();
        emit_pending_broadcasts(&mut app);
        let mut event = Vec::new();
        fifo.read_to_end(&mut event).unwrap();
        assert_eq!(event, b"monitor_rename 0x00000001 monitor new\n");
        assert!(!path.exists());
        assert_eq!(app.subscriber_count(), 0);
    }
}
