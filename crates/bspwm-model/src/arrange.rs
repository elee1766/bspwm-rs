#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::field_reassign_with_default,
    clippy::missing_panics_doc
)]

use crate::settings::Settings;
use crate::tree::{Client, NodeId, Presel, SizeHints};
use crate::types::{ClientState, Layout, Padding, Rectangle};
use crate::world::{DesktopId, MonitorId, World};

/// A side-effect-free description of the X geometry work for one client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArrangeAction {
    pub node: NodeId,
    pub window: u32,
    pub rectangle: Rectangle,
    pub border_width: u32,
    pub geometry_changed: bool,
}

/// Geometry work for an existing or not-yet-created preselection feedback window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreselFeedbackPlan {
    pub node: NodeId,
    pub feedback: Option<u32>,
    pub rectangle: Rectangle,
}

#[must_use]
pub fn plan_presel_feedbacks(
    world: &World,
    desktop: DesktopId,
    settings: &Settings,
) -> Vec<PreselFeedbackPlan> {
    if !settings.presel_feedback || world.desktop(desktop).user_layout == Layout::Monocle {
        return Vec::new();
    }
    let Some(root) = world.desktop(desktop).tree.root else {
        return Vec::new();
    };
    let desktop = world.desktop(desktop);
    let gap = if settings.gapless_monocle && desktop.layout == Layout::Monocle {
        0
    } else {
        desktop.window_gap
    };
    world
        .tree
        .preorder(root)
        .filter_map(|node| {
            let value = world.tree.node(node);
            value.presel.map(|presel| PreselFeedbackPlan {
                node,
                feedback: presel.feedback,
                rectangle: presel_feedback_rectangle(value.rectangle, presel, gap),
            })
        })
        .collect()
}

#[must_use]
pub fn presel_feedback_rectangle(rectangle: Rectangle, presel: Presel, gap: i32) -> Rectangle {
    let width = rectangle.width.saturating_sub(gap).max(0);
    let height = rectangle.height.saturating_sub(gap).max(0);
    let mut result = Rectangle::new(rectangle.x, rectangle.y, width, height);
    match presel.split_dir {
        crate::types::Direction::North => {
            result.height = (presel.split_ratio * f64::from(height)) as i32;
        }
        crate::types::Direction::East => {
            result.width = ((1.0 - presel.split_ratio) * f64::from(width)) as i32;
            result.x = rectangle.x.saturating_add(width - result.width);
        }
        crate::types::Direction::South => {
            result.height = ((1.0 - presel.split_ratio) * f64::from(height)) as i32;
            result.y = rectangle.y.saturating_add(height - result.height);
        }
        crate::types::Direction::West => {
            result.width = (presel.split_ratio * f64::from(width)) as i32;
        }
    }
    result
}

/// Plans the arrangement using the size information represented by [`Client`].
///
/// `Client` currently has no ICCCM size-hint data, so its represented size-hint
/// operation is the identity. `arrange_with_size_hints` is the explicit hook to
/// replace once those fields are added to the model.
#[must_use]
pub fn arrange(
    world: &mut World,
    monitor: MonitorId,
    desktop: DesktopId,
    settings: &Settings,
) -> Vec<ArrangeAction> {
    arrange_with_size_hints(world, monitor, desktop, settings, apply_size_hints)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
pub fn apply_size_hints(client: &Client, width: i32, height: i32) -> (i32, i32) {
    if !client.honor_size_hints.should_honor(client.state) {
        return (width, height);
    }
    let hints = client.size_hints;
    let (base, min) = (SizeHints::BASE_SIZE, SizeHints::MIN_SIZE);
    let real_base_width = preferred(hints, base, hints.base_width, NO_FLAG, 0);
    let real_base_height = preferred(hints, base, hints.base_height, NO_FLAG, 0);
    let base_width = preferred(hints, base, hints.base_width, min, hints.min_width);
    let base_height = preferred(hints, base, hints.base_height, min, hints.min_height);
    let min_width = preferred(hints, min, hints.min_width, base, hints.base_width);
    let min_height = preferred(hints, min, hints.min_height, base, hints.base_height);
    let (mut width, mut height) = (width, height);

    if flag(hints, SizeHints::ASPECT)
        && hints.min_aspect_den > 0
        && hints.max_aspect_den > 0
        && height > real_base_height
        && width > real_base_width
    {
        let mut dx = f64::from(width - real_base_width);
        let mut dy = f64::from(height - real_base_height);
        let ratio = dx / dy;
        let min = f64::from(hints.min_aspect_num) / f64::from(hints.min_aspect_den);
        let max = f64::from(hints.max_aspect_num) / f64::from(hints.max_aspect_den);
        if max > 0.0 && min > 0.0 && ratio > 0.0 {
            if ratio < min {
                dy = dx / min + 0.5;
                height = dy as i32 + real_base_height;
            } else if ratio > max {
                dx = dy * max + 0.5;
                width = dx as i32 + real_base_width;
            }
        }
    }
    width = width.max(min_width);
    height = height.max(min_height);
    if flag(hints, SizeHints::MAX_SIZE) {
        if hints.max_width > 0 {
            width = width.min(hints.max_width);
        }
        if hints.max_height > 0 {
            height = height.min(hints.max_height);
        }
    }
    if hints.flags & (SizeHints::RESIZE_INC | SizeHints::BASE_SIZE) != 0
        && hints.width_inc > 0
        && hints.height_inc > 0
    {
        width -= (width - base_width).max(0) % hints.width_inc;
        height -= (height - base_height).max(0) % hints.height_inc;
    }
    (width.max(1), height.max(1))
}

const fn flag(hints: SizeHints, flag: u32) -> bool {
    hints.flags & flag != 0
}

/// A flag mask that never matches, for the two-armed ladders below.
const NO_FLAG: u32 = 0;

/// Upstream's repeated `if (flags & a) x else if (flags & b) y else 0` ladder.
const fn preferred(
    hints: SizeHints,
    primary: u32,
    primary_value: i32,
    fallback: u32,
    fallback_value: i32,
) -> i32 {
    if flag(hints, primary) {
        primary_value
    } else if flag(hints, fallback) {
        fallback_value
    } else {
        0
    }
}

/// Plans an arrangement, applying `size_hints` to each desired client size.
///
/// The callback corresponds to upstream's `apply_size_hints`: it receives the
/// client and the already selected width and height, and must return the hinted
/// dimensions. Coordinates are deliberately not exposed to the hook.
#[must_use]
pub fn arrange_with_size_hints<F>(
    world: &mut World,
    monitor: MonitorId,
    desktop: DesktopId,
    settings: &Settings,
    mut size_hints: F,
) -> Vec<ArrangeAction>
where
    F: FnMut(&Client, i32, i32) -> (i32, i32),
{
    let Some(root) = world.desktop(desktop).tree.root else {
        return Vec::new();
    };

    let monitor_rectangle = world.monitor(monitor).rectangle;
    let layout = world.desktop(desktop).layout;
    let window_gap = world.desktop(desktop).window_gap;
    let root_rectangle = root_rectangle(
        monitor_rectangle,
        world.monitor(monitor).padding,
        world.desktop(desktop).padding,
        layout,
        window_gap,
        settings,
    );

    // This is the structural half of upstream apply_layout. It records every
    // node rectangle and adjusts constrained split ratios, but performs no I/O.
    world
        .tree
        .apply_layout(root, root_rectangle, layout == Layout::Monocle);

    let only_window = world.monitor_order().len() == 1 && world.tree.node(root).client.is_some();
    let mut actions = Vec::new();
    // Collected up front because the loop writes back each client's tiled
    // rectangle; the walk itself only ever sees the layout `apply_layout` fixed.
    for node_id in world.tree.leaves(root).collect::<Vec<_>>() {
        let node = world.tree.node(node_id);
        if let Some(client) = node.client.clone() {
            let partition = node.rectangle;
            let window = node.external_id;
            let previous = state_rectangle(&client);
            let border_width = effective_border_width(
                &client,
                layout,
                only_window,
                settings.borderless_monocle,
                settings.borderless_singleton,
            );
            let (mut desired, tiled_rectangle) = desired_rectangle(
                &client,
                partition,
                monitor_rectangle,
                layout,
                window_gap,
                border_width,
                settings,
            );
            let (width, height) = size_hints(&client, desired.width, desired.height);
            desired.width = width;
            desired.height = height;

            // `tiled_rectangle` is the *un-hinted* rectangle, recorded before
            // `size_hints` narrowed `desired`, and `previous` was read from the
            // same field. A client with size hints therefore reports a geometry
            // change on every arrange. Upstream `tree.c` behaves identically --
            // do not "fix" this.
            if let Some(rectangle) = tiled_rectangle {
                world
                    .tree
                    .node_mut(node_id)
                    .client
                    .as_mut()
                    .expect("client was present")
                    .tiled_rectangle = rectangle;
            }
            actions.push(ArrangeAction {
                node: node_id,
                window,
                rectangle: desired,
                border_width,
                geometry_changed: desired != previous,
            });
        }
    }
    actions
}

fn root_rectangle(
    mut rectangle: Rectangle,
    monitor_padding: Padding,
    desktop_padding: Padding,
    layout: Layout,
    window_gap: i32,
    settings: &Settings,
) -> Rectangle {
    // Upstream folds both paddings into one expression per field; narrowing
    // once per padding is the same modulo 2^16 arithmetic, so this is exact.
    apply_padding(&mut rectangle, monitor_padding);
    apply_padding(&mut rectangle, desktop_padding);
    if layout == Layout::Monocle {
        apply_padding(&mut rectangle, settings.monocle_padding);
    }
    if !settings.gapless_monocle || layout != Layout::Monocle {
        // The gap shrinks the top-left corner only; the rest is bled off each
        // client rectangle instead.
        apply_padding(
            &mut rectangle,
            Padding {
                top: window_gap,
                right: 0,
                bottom: 0,
                left: window_gap,
            },
        );
    }
    rectangle
}

fn apply_padding(rectangle: &mut Rectangle, padding: Padding) {
    rectangle.x = rectangle.x.saturating_add(padding.left);
    rectangle.y = rectangle.y.saturating_add(padding.top);
    rectangle.width = rectangle
        .width
        .saturating_sub(padding.left.saturating_add(padding.right))
        .max(0);
    rectangle.height = rectangle
        .height
        .saturating_sub(padding.top.saturating_add(padding.bottom))
        .max(0);
}

const fn state_rectangle(client: &Client) -> Rectangle {
    match client.state {
        ClientState::Tiled | ClientState::PseudoTiled | ClientState::Fullscreen => {
            client.tiled_rectangle
        }
        ClientState::Floating => client.floating_rectangle,
    }
}

const fn effective_border_width(
    client: &Client,
    layout: Layout,
    only_window: bool,
    borderless_monocle: bool,
    borderless_singleton: bool,
) -> u32 {
    if borderless_monocle && matches!(layout, Layout::Monocle) && client.state.is_tiled()
        || borderless_singleton && only_window
        || matches!(client.state, ClientState::Fullscreen)
    {
        0
    } else {
        client.border_width
    }
}

fn desired_rectangle(
    client: &Client,
    partition: Rectangle,
    monitor_rectangle: Rectangle,
    layout: Layout,
    window_gap: i32,
    border_width: u32,
    settings: &Settings,
) -> (Rectangle, Option<Rectangle>) {
    match client.state {
        ClientState::Tiled | ClientState::PseudoTiled => {
            let gap = if settings.gapless_monocle && layout == Layout::Monocle {
                0
            } else {
                window_gap
            };
            let bleed = gap.wrapping_add(border_width.wrapping_mul(2) as i32);
            let mut desired = partition;
            desired.width = bleed_dimension(partition.width, bleed);
            desired.height = bleed_dimension(partition.height, bleed);

            if client.state == ClientState::PseudoTiled {
                desired.width = desired.width.min(client.floating_rectangle.width);
                desired.height = desired.height.min(client.floating_rectangle.height);
                if settings.center_pseudo_tiled {
                    let dx = partition.width - gap - desired.width;
                    let dy = partition.height - gap - desired.height;
                    let border_width = i32::try_from(border_width).unwrap_or(i32::MAX);
                    desired.x = partition.x.saturating_sub(border_width) + dx / 2;
                    desired.y = partition.y.saturating_sub(border_width) + dy / 2;
                }
            }
            (desired, Some(desired))
        }
        ClientState::Floating => (client.floating_rectangle, None),
        ClientState::Fullscreen => (monitor_rectangle, Some(monitor_rectangle)),
    }
}

const fn bleed_dimension(dimension: i32, bleed: i32) -> i32 {
    if bleed < dimension {
        dimension.saturating_sub(bleed)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Client;
    use crate::types::SplitType;

    #[test]
    fn preselection_feedback_geometry_matches_each_upstream_direction() {
        let rectangle = Rectangle::new(10, 20, 100, 80);
        let make = |split_dir| Presel {
            split_dir,
            split_ratio: 0.25,
            feedback: Some(7),
        };
        assert_eq!(
            presel_feedback_rectangle(rectangle, make(crate::types::Direction::North), 4),
            Rectangle::new(10, 20, 96, 19)
        );
        assert_eq!(
            presel_feedback_rectangle(rectangle, make(crate::types::Direction::East), 4),
            Rectangle::new(34, 20, 72, 76)
        );
        assert_eq!(
            presel_feedback_rectangle(rectangle, make(crate::types::Direction::South), 4),
            Rectangle::new(10, 39, 96, 57)
        );
        assert_eq!(
            presel_feedback_rectangle(rectangle, make(crate::types::Direction::West), 4),
            Rectangle::new(10, 20, 24, 76)
        );
    }

    #[test]
    fn feedback_plan_keeps_optional_xid_and_obeys_visibility_settings() {
        let settings = Settings::default();
        let (mut world, _, desktop) = world_with_desktop(&settings, Rectangle::new(0, 0, 100, 100));
        let node = world.tree.add_node(1, 0.5);
        world.tree.node_mut(node).rectangle = Rectangle::new(1, 2, 80, 60);
        world.tree.node_mut(node).presel = Some(Presel {
            split_dir: crate::types::Direction::East,
            split_ratio: 0.5,
            feedback: Some(77),
        });
        world.desktop_mut(desktop).tree.root = Some(node);

        let plans = plan_presel_feedbacks(&world, desktop, &settings);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].feedback, Some(77));
        let mut hidden = settings.clone();
        hidden.presel_feedback = false;
        assert!(plan_presel_feedbacks(&world, desktop, &hidden).is_empty());
        world.desktop_mut(desktop).user_layout = Layout::Monocle;
        assert!(plan_presel_feedbacks(&world, desktop, &settings).is_empty());
    }

    fn world_with_desktop(
        settings: &Settings,
        rectangle: Rectangle,
    ) -> (World, MonitorId, DesktopId) {
        let mut world = World::default();
        let monitor = world.create_monitor(0x10, Some("m"), rectangle, settings);
        let desktop = world.create_desktop(0x20, Some("d"), settings);
        assert!(world.add_desktop(monitor, desktop));
        (world, monitor, desktop)
    }

    fn add_client(
        world: &mut World,
        desktop: DesktopId,
        settings: &Settings,
        window: u32,
    ) -> NodeId {
        let node = world.tree.add_node(window, settings.split_ratio);
        world.tree.node_mut(node).client = Some(Client::from_settings(settings));
        world.desktop_mut(desktop).tree.root = Some(node);
        node
    }

    #[test]
    fn empty_desktop_has_no_plan() {
        let settings = Settings::default();
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(0, 0, 100, 100));
        assert!(arrange(&mut world, monitor, desktop, &settings).is_empty());
    }

    #[test]
    fn tiled_root_combines_padding_gap_and_border_bleed() {
        let mut settings = Settings::default();
        settings.padding = Padding {
            top: 2,
            right: 3,
            bottom: 4,
            left: 5,
        };
        settings.window_gap = 6;
        settings.border_width = 2;
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(10, 20, 100, 80));
        let node = add_client(&mut world, desktop, &settings, 0xabc);

        let actions = arrange(&mut world, monitor, desktop, &settings);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].node, node);
        assert_eq!(actions[0].window, 0xabc);
        assert_eq!(actions[0].rectangle, Rectangle::new(26, 30, 68, 52));
        assert_eq!(actions[0].border_width, 2);
        assert!(actions[0].geometry_changed);
        assert_eq!(
            world.tree.node(node).rectangle,
            Rectangle::new(26, 30, 78, 62)
        );
        assert_eq!(
            world
                .tree
                .node(node)
                .client
                .as_ref()
                .unwrap()
                .tiled_rectangle,
            actions[0].rectangle
        );
    }

    #[test]
    fn structural_partitioning_orders_leaves_and_honors_constraints() {
        let mut settings = Settings::default();
        settings.padding = Padding::default();
        settings.window_gap = 0;
        settings.border_width = 0;
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(0, 0, 100, 50));
        let root = world.tree.add_node(0, 0.1);
        let first = world.tree.add_node(1, 0.5);
        let second = world.tree.add_node(2, 0.5);
        world.tree.node_mut(first).client = Some(Client::from_settings(&settings));
        world.tree.node_mut(second).client = Some(Client::from_settings(&settings));
        world.tree.set_children(root, first, second);
        world.tree.node_mut(root).split_type = SplitType::Vertical;
        world.desktop_mut(desktop).tree.root = Some(root);

        let actions = arrange(&mut world, monitor, desktop, &settings);
        assert_eq!(
            actions.iter().map(|action| action.node).collect::<Vec<_>>(),
            [first, second]
        );
        assert_eq!(actions[0].rectangle, Rectangle::new(0, 0, 32, 50));
        assert_eq!(actions[1].rectangle, Rectangle::new(32, 0, 68, 50));
        assert!((world.tree.node(root).split_ratio - 0.32).abs() < f64::EPSILON);
    }

    #[test]
    fn vacant_branch_receives_the_same_partition() {
        let mut settings = Settings::default();
        settings.window_gap = 0;
        settings.border_width = 0;
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(0, 0, 90, 60));
        let root = world.tree.add_node(0, 0.25);
        let first = world.tree.add_node(1, 0.5);
        let second = world.tree.add_node(2, 0.5);
        world.tree.node_mut(first).client = Some(Client::from_settings(&settings));
        world.tree.node_mut(second).client = Some(Client::from_settings(&settings));
        world.tree.node_mut(second).vacant = true;
        world.tree.set_children(root, first, second);
        world.desktop_mut(desktop).tree.root = Some(root);

        let actions = arrange(&mut world, monitor, desktop, &settings);
        assert_eq!(actions[0].rectangle, Rectangle::new(0, 0, 90, 60));
        assert_eq!(actions[1].rectangle, Rectangle::new(0, 0, 90, 60));
    }

    #[test]
    fn monocle_modes_match_upstream_geometry() {
        {
            let mut settings = Settings::default();
            settings.padding = Padding::default();
            settings.monocle_padding = Padding {
                top: 2,
                right: 4,
                bottom: 6,
                left: 8,
            };
            settings.window_gap = 5;
            settings.border_width = 3;
            settings.borderless_monocle = true;
            let (mut world, monitor, desktop) =
                world_with_desktop(&settings, Rectangle::new(10, 20, 100, 80));
            world.desktop_mut(desktop).layout = Layout::Monocle;
            add_client(&mut world, desktop, &settings, 1);

            let action = arrange(&mut world, monitor, desktop, &settings)[0];
            assert_eq!(action.rectangle, Rectangle::new(23, 27, 78, 62));
            assert_eq!(action.border_width, 0);
        }

        {
            let mut settings = Settings::default();
            settings.padding = Padding::default();
            settings.window_gap = 9;
            settings.border_width = 2;
            settings.gapless_monocle = true;
            let (mut world, monitor, desktop) =
                world_with_desktop(&settings, Rectangle::new(0, 0, 100, 80));
            world.desktop_mut(desktop).layout = Layout::Monocle;
            add_client(&mut world, desktop, &settings, 1);

            assert_eq!(
                arrange(&mut world, monitor, desktop, &settings)[0].rectangle,
                Rectangle::new(0, 0, 96, 76)
            );
        }
    }

    #[test]
    fn pseudo_tiled_centering_modes_match_upstream_geometry() {
        {
            let mut settings = Settings::default();
            settings.padding = Padding::default();
            settings.window_gap = 6;
            settings.border_width = 2;
            let (mut world, monitor, desktop) =
                world_with_desktop(&settings, Rectangle::new(0, 0, 100, 80));
            let node = add_client(&mut world, desktop, &settings, 1);
            let client = world.tree.node_mut(node).client.as_mut().unwrap();
            client.state = ClientState::PseudoTiled;
            client.floating_rectangle = Rectangle::new(50, 50, 40, 20);

            let action = arrange(&mut world, monitor, desktop, &settings)[0];
            assert_eq!(action.rectangle, Rectangle::new(28, 28, 40, 20));
        }

        {
            let mut settings = Settings::default();
            settings.padding = Padding::default();
            settings.window_gap = 4;
            settings.border_width = 1;
            settings.center_pseudo_tiled = false;
            let (mut world, monitor, desktop) =
                world_with_desktop(&settings, Rectangle::new(10, 20, 80, 60));
            let node = add_client(&mut world, desktop, &settings, 1);
            let client = world.tree.node_mut(node).client.as_mut().unwrap();
            client.state = ClientState::PseudoTiled;
            client.floating_rectangle = Rectangle::new(0, 0, 20, 10);

            assert_eq!(
                arrange(&mut world, monitor, desktop, &settings)[0].rectangle,
                Rectangle::new(14, 24, 20, 10)
            );
        }
    }

    #[test]
    fn floating_and_fullscreen_choose_their_upstream_rectangles() {
        let mut settings = Settings::default();
        settings.padding = Padding::default();
        let monitor_rectangle = Rectangle::new(-20, 30, 200, 100);
        let (mut world, monitor, desktop) = world_with_desktop(&settings, monitor_rectangle);
        let root = world.tree.add_node(0, 0.5);
        let floating = world.tree.add_node(1, 0.5);
        let fullscreen = world.tree.add_node(2, 0.5);
        let mut floating_client = Client::from_settings(&settings);
        floating_client.state = ClientState::Floating;
        floating_client.floating_rectangle = Rectangle::new(7, 8, 30, 40);
        let mut fullscreen_client = Client::from_settings(&settings);
        fullscreen_client.state = ClientState::Fullscreen;
        world.tree.node_mut(floating).client = Some(floating_client);
        world.tree.node_mut(fullscreen).client = Some(fullscreen_client);
        world.tree.set_children(root, floating, fullscreen);
        world.desktop_mut(desktop).tree.root = Some(root);

        let actions = arrange(&mut world, monitor, desktop, &settings);
        assert_eq!(actions[0].rectangle, Rectangle::new(7, 8, 30, 40));
        assert!(!actions[0].geometry_changed);
        assert_eq!(actions[1].rectangle, monitor_rectangle);
        assert_eq!(actions[1].border_width, 0);
        assert_eq!(
            world
                .tree
                .node(fullscreen)
                .client
                .as_ref()
                .unwrap()
                .tiled_rectangle,
            monitor_rectangle
        );
    }

    #[test]
    fn borderless_singleton_requires_one_monitor_and_leaf_root() {
        let mut settings = Settings::default();
        settings.padding = Padding::default();
        settings.borderless_singleton = true;
        settings.border_width = 4;
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(0, 0, 100, 100));
        add_client(&mut world, desktop, &settings, 1);
        assert_eq!(
            arrange(&mut world, monitor, desktop, &settings)[0].border_width,
            0
        );

        let _other = world.create_monitor(
            2,
            Some("other"),
            Rectangle::new(100, 0, 100, 100),
            &settings,
        );
        assert_eq!(
            arrange(&mut world, monitor, desktop, &settings)[0].border_width,
            4
        );
    }

    #[test]
    fn repeated_identity_plan_reports_unchanged_geometry() {
        let mut settings = Settings::default();
        settings.padding = Padding::default();
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(0, 0, 100, 100));
        add_client(&mut world, desktop, &settings, 1);
        assert!(arrange(&mut world, monitor, desktop, &settings)[0].geometry_changed);
        assert!(!arrange(&mut world, monitor, desktop, &settings)[0].geometry_changed);
    }

    #[test]
    fn size_hint_hook_changes_action_but_not_unhinted_tiled_state() {
        let mut settings = Settings::default();
        settings.padding = Padding::default();
        settings.window_gap = 0;
        settings.border_width = 0;
        let (mut world, monitor, desktop) =
            world_with_desktop(&settings, Rectangle::new(0, 0, 101, 99));
        let node = add_client(&mut world, desktop, &settings, 1);

        let action = arrange_with_size_hints(
            &mut world,
            monitor,
            desktop,
            &settings,
            |client, width, height| {
                assert_eq!(client.state, ClientState::Tiled);
                (width - width % 10, height - height % 10)
            },
        )[0];
        assert_eq!(action.rectangle, Rectangle::new(0, 0, 100, 90));
        assert_eq!(
            world
                .tree
                .node(node)
                .client
                .as_ref()
                .unwrap()
                .tiled_rectangle,
            Rectangle::new(0, 0, 101, 99)
        );
    }

    #[test]
    fn represented_size_hints_are_honored_by_the_default_arranger() {
        let settings = Settings::default();
        let mut client = Client::from_settings(&settings);
        client.state = ClientState::Floating;
        client.honor_size_hints = crate::types::HonorSizeHintsMode::Yes;
        client.size_hints = SizeHints {
            flags: SizeHints::MIN_SIZE | SizeHints::MAX_SIZE,
            min_width: 80,
            min_height: 60,
            max_width: 100,
            max_height: 90,
            ..SizeHints::default()
        };
        assert_eq!(apply_size_hints(&client, 20, 120), (80, 90));
    }

    #[test]
    fn negative_gap_uses_wide_internal_geometry() {
        let mut settings = Settings::default();
        settings.padding = Padding::default();
        settings.window_gap = -10;
        settings.border_width = 0;
        let (mut world, monitor, desktop) = world_with_desktop(
            &settings,
            Rectangle::new(i32::from(i16::MIN), i32::from(i16::MAX), 5, 5),
        );
        add_client(&mut world, desktop, &settings, 1);

        let action = arrange(&mut world, monitor, desktop, &settings)[0];
        assert_eq!(action.rectangle, Rectangle::new(-32_778, 32_757, 25, 25));
    }

    #[test]
    fn excessive_bleed_clamps_dimensions_to_one() {
        assert_eq!(bleed_dimension(10, 9), 1);
        assert_eq!(bleed_dimension(10, 10), 1);
        assert_eq!(bleed_dimension(10, 11), 1);
        assert_eq!(bleed_dimension(10, -2), 12);
    }
}
