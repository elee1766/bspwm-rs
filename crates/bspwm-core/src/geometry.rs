use std::cmp::Ordering;

use crate::types::{Direction, Point, Rectangle, Tightness};

const fn max_x(rect: Rectangle) -> i32 {
    rect.right().saturating_sub(1)
}

const fn max_y(rect: Rectangle) -> i32 {
    rect.bottom().saturating_sub(1)
}

#[must_use]
pub const fn is_inside(point: Point, rect: Rectangle) -> bool {
    point.x >= rect.x && point.x < rect.right() && point.y >= rect.y && point.y < rect.bottom()
}

#[must_use]
pub const fn contains(outer: Rectangle, inner: Rectangle) -> bool {
    outer.x <= inner.x
        && outer.right() >= inner.right()
        && outer.y <= inner.y
        && outer.bottom() >= inner.bottom()
}

#[must_use]
pub fn area(rect: Rectangle) -> u64 {
    let width = u64::try_from(rect.width).unwrap_or(0);
    let height = u64::try_from(rect.height).unwrap_or(0);
    width.saturating_mul(height)
}

#[must_use]
pub fn boundary_distance(first: Rectangle, second: Rectangle, direction: Direction) -> u64 {
    let distance = match direction {
        Direction::North => i64::from(max_y(second)) - i64::from(first.y),
        Direction::West => i64::from(max_x(second)) - i64::from(first.x),
        Direction::South => i64::from(second.y) - i64::from(max_y(first)),
        Direction::East => i64::from(second.x) - i64::from(max_x(first)),
    };
    distance.unsigned_abs()
}

#[must_use]
pub const fn on_dir_side(
    first: Rectangle,
    second: Rectangle,
    direction: Direction,
    tightness: Tightness,
) -> bool {
    let first_max_x = max_x(first);
    let first_max_y = max_y(first);
    let second_max_x = max_x(second);
    let second_max_y = max_y(second);

    let on_requested_side = match (tightness, direction) {
        (Tightness::Low, Direction::North) => second.y <= first_max_y,
        (Tightness::Low, Direction::West) => second.x <= first_max_x,
        (Tightness::Low, Direction::South) => second_max_y >= first.y,
        (Tightness::Low, Direction::East) => second_max_x >= first.x,
        (Tightness::High, Direction::North) => second.y < first.y,
        (Tightness::High, Direction::West) => second.x < first.x,
        (Tightness::High, Direction::South) => second_max_y > first_max_y,
        (Tightness::High, Direction::East) => second_max_x > first_max_x,
    };
    if !on_requested_side {
        return false;
    }

    // The two "candidate straddles us" arms below are deliberately NOT mirror
    // images: North/South ends with `first.x < second_max_x` while West/East
    // ends with `first_max_y < second_max_y`. That asymmetry is verbatim
    // upstream `geometry.c` `on_dir_side`. "Fixing" it silently changes which
    // window directional focus picks -- leave it alone.
    match direction {
        Direction::North | Direction::South => {
            (second.x >= first.x && second.x <= first_max_x)
                || (second_max_x >= first.x && second_max_x <= first_max_x)
                || (first.x > second.x && first.x < second_max_x)
        }
        Direction::West | Direction::East => {
            (second.y >= first.y && second.y <= first_max_y)
                || (second_max_y >= first.y && second_max_y <= first_max_y)
                || (first.y > second.y && first_max_y < second_max_y)
        }
    }
}

/// Upstream `rect_cmp`: spatial order first, then descending area.
///
/// # This is not a total order
///
/// It is neither transitive nor antisymmetric -- two disjoint rectangles laid
/// out diagonally each compare [`Ordering::Greater`] to the other. It returns
/// [`Ordering`] only because that is the natural shape for the upstream
/// insertion-sort passes that use it (`World::create_monitor` and
/// `World::update_monitor_rectangle`), which tolerate an inconsistent
/// comparator. **Never pass it to `slice::sort_by`**: `sort_by` documents a
/// panic (or an arbitrary permutation) for comparators that are not total
/// orders.
#[must_use]
pub fn rect_cmp(first: Rectangle, second: Rectangle) -> Ordering {
    if first.y >= second.bottom() {
        Ordering::Greater
    } else if second.y >= first.bottom() {
        Ordering::Less
    } else if first.x >= second.right() {
        Ordering::Greater
    } else if second.x >= first.right() {
        Ordering::Less
    } else {
        area(second).cmp(&area(first))
    }
}

/// Centers a floating rectangle in `area`, preserving bspwm's border offset.
#[must_use]
#[allow(clippy::cast_lossless)]
pub const fn center(mut rectangle: Rectangle, area: Rectangle, border_width: u16) -> Rectangle {
    rectangle.x = if rectangle.width >= area.width {
        area.x
    } else {
        area.left()
            .saturating_add((area.width - rectangle.width) / 2)
    };
    rectangle.y = if rectangle.height >= area.height {
        area.y
    } else {
        area.top()
            .saturating_add((area.height - rectangle.height) / 2)
    };
    let border_width = border_width as i32;
    rectangle.x = rectangle.left().saturating_sub(border_width);
    rectangle.y = rectangle.top().saturating_sub(border_width);
    rectangle
}

/// Moves a rectangle that is wholly beyond an edge back to touch that edge.
#[must_use]
pub const fn embrace(mut rectangle: Rectangle, area: Rectangle) -> Rectangle {
    if rectangle.right() <= area.left() {
        rectangle.x = area.x;
    } else if rectangle.left() >= area.right() {
        rectangle.x = area.right().saturating_sub(rectangle.width);
    }

    if rectangle.bottom() <= area.top() {
        rectangle.y = area.y;
    } else if rectangle.top() >= area.bottom() {
        rectangle.y = area.bottom().saturating_sub(rectangle.height);
    }
    rectangle
}

/// Adapts a floating rectangle from one monitor geometry to another.
///
/// Clipping is temporary, so portions outside the source monitor remain outside
/// the destination monitor.
#[must_use]
#[allow(clippy::similar_names)]
pub const fn adapt_geometry(
    mut rectangle: Rectangle,
    source: Rectangle,
    destination: Rectangle,
) -> Rectangle {
    let left_adjust = const_max(source.left() - rectangle.left(), 0);
    let top_adjust = const_max(source.top() - rectangle.top(), 0);
    let right_adjust = const_max(rectangle.right() - source.right(), 0);
    let bottom_adjust = const_max(rectangle.bottom() - source.bottom(), 0);

    rectangle.x = rectangle.left().saturating_add(left_adjust);
    rectangle.y = rectangle.top().saturating_add(top_adjust);
    rectangle.width = rectangle.width - left_adjust - right_adjust;
    rectangle.height = rectangle.height - top_adjust - bottom_adjust;

    let source_dx = rectangle.left() - source.left();
    let source_dy = rectangle.top() - source.top();
    let denominator_x = source.width - rectangle.width;
    let denominator_y = source.height - rectangle.height;
    let destination_dx = if denominator_x == 0 {
        0
    } else {
        source_dx.saturating_mul(destination.width - rectangle.width) / denominator_x
    };
    let destination_dy = if denominator_y == 0 {
        0
    } else {
        source_dy.saturating_mul(destination.height - rectangle.height) / denominator_y
    };

    rectangle.width = rectangle
        .width
        .saturating_add(left_adjust.saturating_add(right_adjust));
    rectangle.height = rectangle
        .height
        .saturating_add(top_adjust.saturating_add(bottom_adjust));
    rectangle.x = destination
        .left()
        .saturating_add(destination_dx)
        .saturating_sub(left_adjust);
    rectangle.y = destination
        .top()
        .saturating_add(destination_dy)
        .saturating_sub(top_adjust);
    rectangle
}

const fn const_max(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CENTER: Rectangle = Rectangle::new(10, 10, 10, 10);

    #[test]
    fn point_inside_uses_half_open_edges() {
        assert!(is_inside(Point { x: 10, y: 10 }, CENTER));
        assert!(is_inside(Point { x: 19, y: 19 }, CENTER));
        assert!(!is_inside(Point { x: 20, y: 19 }, CENTER));
        assert!(!is_inside(Point { x: 19, y: 20 }, CENTER));
    }

    #[test]
    fn containment_accepts_equal_edges() {
        assert!(contains(CENTER, CENTER));
        assert!(contains(CENTER, Rectangle::new(12, 12, 3, 3)));
        assert!(!contains(CENTER, Rectangle::new(9, 12, 3, 3)));
    }

    #[test]
    fn computes_area_and_boundary_distance() {
        assert_eq!(area(CENTER), 100);
        let north = Rectangle::new(10, 0, 10, 5);
        assert_eq!(boundary_distance(CENTER, north, Direction::North), 6);
        assert_eq!(boundary_distance(north, CENTER, Direction::South), 6);
    }

    #[test]
    fn low_tightness_accepts_overlapping_candidates() {
        let overlap = Rectangle::new(12, 15, 5, 10);
        assert!(on_dir_side(
            CENTER,
            overlap,
            Direction::North,
            Tightness::Low
        ));
        assert!(on_dir_side(
            CENTER,
            overlap,
            Direction::South,
            Tightness::Low
        ));
    }

    #[test]
    fn high_tightness_requires_extension_beyond_requested_edge() {
        let north = Rectangle::new(12, 5, 5, 10);
        let same_top = Rectangle::new(12, 10, 5, 5);
        assert!(on_dir_side(
            CENTER,
            north,
            Direction::North,
            Tightness::High
        ));
        assert!(!on_dir_side(
            CENTER,
            same_top,
            Direction::North,
            Tightness::High
        ));
    }

    #[test]
    fn directional_candidates_need_a_shared_cross_axis_range() {
        let west = Rectangle::new(0, 12, 5, 5);
        let northwest = Rectangle::new(0, 0, 5, 5);
        assert!(on_dir_side(CENTER, west, Direction::West, Tightness::High));
        assert!(!on_dir_side(
            CENTER,
            northwest,
            Direction::West,
            Tightness::High
        ));
    }

    #[test]
    fn rectangle_order_matches_upstream_spatial_then_area_order() {
        let above = Rectangle::new(10, 0, 10, 5);
        let left = Rectangle::new(0, 10, 5, 10);
        let larger_overlap = Rectangle::new(10, 10, 20, 20);
        assert_eq!(rect_cmp(above, CENTER), Ordering::Less);
        assert_eq!(rect_cmp(left, CENTER), Ordering::Less);
        assert_eq!(rect_cmp(larger_overlap, CENTER), Ordering::Less);
        assert_eq!(CENTER, CENTER);
    }
}
