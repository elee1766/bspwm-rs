//! Subscription record builders and the broadcast helper.

use super::DaemonApp;
use crate::subscribe::print_report;
use crate::tree::NodeId;
use crate::types::{Rectangle, SubscriberMask};
use crate::world::{DesktopId, MonitorId};

impl DaemonApp {
    /// The `monitor desktop` identifier prefix shared by desktop records.
    #[must_use]
    pub(super) fn desktop_ids(&self, monitor: MonitorId, desktop: DesktopId) -> String {
        format!(
            "0x{:08X} 0x{:08X}",
            self.monitor_xid(monitor),
            self.desktop_xid(desktop)
        )
    }

    /// The `monitor desktop node` identifier triple shared by node records.
    #[must_use]
    pub(super) fn node_ids(&self, monitor: MonitorId, desktop: DesktopId, node: NodeId) -> String {
        self.node_ids_raw(monitor, desktop, self.xid(node))
    }

    /// [`DaemonApp::node_ids`] for a window that may have no node, such as the
    /// absent anchor of a transfer, which upstream reports as `0x00000000`.
    #[must_use]
    pub(super) fn node_ids_raw(&self, monitor: MonitorId, desktop: DesktopId, node: u32) -> String {
        format!("{} 0x{node:08X}", self.desktop_ids(monitor, desktop))
    }

    /// Emits one subscription record.
    pub(super) fn publish(&mut self, mask: SubscriberMask, status: &str) {
        self.broadcast_status(mask, status.as_bytes());
    }

    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.entries().len()
    }

    pub fn broadcast_status(&mut self, mask: SubscriberMask, status: &[u8]) {
        if self.subscribers.entries().is_empty() {
            return;
        }
        let report = print_report(self.world(), &self.state.settings);
        self.subscribers.put_status(mask, status, &report);
    }

    pub(super) fn broadcast_report(&mut self) {
        self.broadcast_status(SubscriberMask::REPORT, &[]);
    }
}

pub(super) fn node_geometry_status(
    monitor: u32,
    desktop: u32,
    node: u32,
    rectangle: Rectangle,
) -> String {
    format!(
        "node_geometry 0x{monitor:08X} 0x{desktop:08X} 0x{node:08X} {}x{}+{}+{}\n",
        rectangle.width, rectangle.height, rectangle.x, rectangle.y,
    )
}

#[cfg(test)]
pub(super) fn node_stack_status(node: u32, relation: &str, sibling: u32) -> String {
    format!("node_stack 0x{node:08X} {relation} 0x{sibling:08X}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::test_support::{
        TestResponse, app_with_desktop, emit_pending_broadcasts, read_available, subscribe_socket,
    };
    use crate::messages::{Domain, MessageHandler};

    #[test]
    fn unix_subscription_masks_emit_exact_records_and_not_reports() {
        let (mut app, _, _) = app_with_desktop();
        let mut client =
            subscribe_socket(&mut app, b"subscribe\x00--count\x001\x00monitor_rename\x00");
        assert!(read_available(&mut client).is_empty());

        app.dispatch(
            Domain::Monitor,
            &[b"focused", b"--rename", b"new"],
            &mut TestResponse::default(),
        )
        .unwrap();
        emit_pending_broadcasts(&mut app);
        assert_eq!(
            read_available(&mut client),
            b"monitor_rename 0x00000001 monitor new\n"
        );
        assert_eq!(app.subscriber_count(), 0);
    }

    #[test]
    fn unix_subscription_gets_desktop_rename_and_layout_records() {
        let (mut app, _, _) = app_with_desktop();
        let mut client = subscribe_socket(
            &mut app,
            b"subscribe\x00-c\x002\x00desktop_rename\x00desktop_layout\x00",
        );
        app.dispatch(
            Domain::Desktop,
            &[b"focused", b"--rename", b"web"],
            &mut TestResponse::default(),
        )
        .unwrap();
        emit_pending_broadcasts(&mut app);
        assert_eq!(
            read_available(&mut client),
            b"desktop_rename 0x00000001 0x00000002 I web\n"
        );
        app.dispatch(
            Domain::Desktop,
            &[b"focused", b"--layout", b"monocle"],
            &mut TestResponse::default(),
        )
        .unwrap();
        emit_pending_broadcasts(&mut app);
        assert_eq!(
            read_available(&mut client),
            b"desktop_layout 0x00000001 0x00000002 monocle\n"
        );
        assert_eq!(app.subscriber_count(), 0);
    }

    #[test]
    fn unlimited_layout_subscriber_gets_one_record_from_queued_effect() {
        let (mut app, _, _) = app_with_desktop();
        let mut client = subscribe_socket(&mut app, b"subscribe\0desktop_layout\0");

        app.dispatch(
            Domain::Desktop,
            &[b"focused", b"--layout", b"monocle"],
            &mut TestResponse::default(),
        )
        .unwrap();
        assert!(read_available(&mut client).is_empty());
        emit_pending_broadcasts(&mut app);

        assert_eq!(
            read_available(&mut client),
            b"desktop_layout 0x00000001 0x00000002 monocle\n"
        );
        assert_eq!(app.subscriber_count(), 1);
    }

    #[test]
    fn unix_report_count_closes_after_a_config_report_change() {
        let (mut app, _, _) = app_with_desktop();
        let mut client = subscribe_socket(&mut app, b"subscribe\x00-c\x002\x00report\x00");
        assert_eq!(read_available(&mut client), b"WMmonitor:FI:LT\n");

        app.dispatch(
            Domain::Config,
            &[b"status_prefix", b"prefix:"],
            &mut TestResponse::default(),
        )
        .unwrap();
        assert_eq!(read_available(&mut client), b"prefix:Mmonitor:FI:LT\n");
        assert_eq!(app.subscriber_count(), 0);
    }

    #[test]
    fn arrangement_and_stack_subscription_records_match_upstream_bytes() {
        assert_eq!(
            node_geometry_status(1, 2, 3, Rectangle::new(-4, 5, 60, 70)),
            "node_geometry 0x00000001 0x00000002 0x00000003 60x70+-4+5\n"
        );
        assert_eq!(
            node_stack_status(0x10, "below", 0x20),
            "node_stack 0x00000010 below 0x00000020\n"
        );
    }
}
