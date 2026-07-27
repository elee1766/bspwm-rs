use crate::types::{Point, PointerAction, Rectangle, ResizeHandle};

/// Selects the side or corner under `position`, preserving bspwm's boundaries.
#[must_use]
pub fn resize_handle(rectangle: Rectangle, position: Point, action: PointerAction) -> ResizeHandle {
    let mut handle = ResizeHandle::BOTTOM_RIGHT;
    if action == PointerAction::ResizeSide {
        let width = f64::from(rectangle.width);
        let height = f64::from(rectangle.height);
        let ratio = width / height;
        let x = f64::from(position.x.saturating_sub(rectangle.x));
        let y = f64::from(position.y.saturating_sub(rectangle.y));
        let diagonal_a = ratio * y;
        let diagonal_b = width - diagonal_a;
        handle = if x < diagonal_a {
            if x < diagonal_b {
                ResizeHandle::LEFT
            } else {
                ResizeHandle::BOTTOM
            }
        } else if x < diagonal_b {
            ResizeHandle::TOP
        } else {
            ResizeHandle::RIGHT
        };
    } else if action == PointerAction::ResizeCorner {
        let middle_x = rectangle.x.saturating_add(rectangle.width / 2);
        let middle_y = rectangle.y.saturating_add(rectangle.height / 2);
        handle = if position.x > middle_x {
            if position.y > middle_y {
                ResizeHandle::BOTTOM_RIGHT
            } else {
                ResizeHandle::TOP_RIGHT
            }
        } else if position.y > middle_y {
            ResizeHandle::BOTTOM_LEFT
        } else {
            ResizeHandle::TOP_LEFT
        };
    }
    handle
}

/// Coordinates supplied to resize planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeInput {
    Relative { dx: i32, dy: i32 },
    Absolute(Point),
}

/// Adds a motion delta to a floating rectangle with C-style coordinate wrapping.
#[must_use]
pub const fn plan_floating_move(mut rectangle: Rectangle, dx: i32, dy: i32) -> Rectangle {
    rectangle.x = rectangle.x.wrapping_add(dx);
    rectangle.y = rectangle.y.wrapping_add(dy);
    rectangle
}

/// Computes upstream's floating resize before optional ICCCM size-hint correction.
#[must_use]
pub fn plan_floating_resize(
    rectangle: Rectangle,
    handle: ResizeHandle,
    input: ResizeInput,
) -> Rectangle {
    let (mut width, mut height) = (rectangle.width, rectangle.height);
    match input {
        ResizeInput::Relative { dx, dy } => {
            width = width.saturating_add(dx.saturating_mul(handle_axis(
                handle,
                ResizeHandle::LEFT,
                ResizeHandle::RIGHT,
            )));
            height = height.saturating_add(dy.saturating_mul(handle_axis(
                handle,
                ResizeHandle::TOP,
                ResizeHandle::BOTTOM,
            )));
        }
        ResizeInput::Absolute(position) => {
            if handle.contains(ResizeHandle::LEFT) {
                width = rectangle
                    .x
                    .saturating_add(rectangle.width)
                    .saturating_sub(position.x);
            } else if handle.contains(ResizeHandle::RIGHT) {
                width = position.x.saturating_sub(rectangle.x);
            }
            if handle.contains(ResizeHandle::TOP) {
                height = rectangle
                    .y
                    .saturating_add(rectangle.height)
                    .saturating_sub(position.y);
            } else if handle.contains(ResizeHandle::BOTTOM) {
                height = position.y.saturating_sub(rectangle.y);
            }
        }
    }
    let width = width.max(1);
    let height = height.max(1);
    let mut result = Rectangle::new(rectangle.x, rectangle.y, width, height);
    if handle.contains(ResizeHandle::LEFT) {
        result.x = result
            .x
            .saturating_add(rectangle.width)
            .saturating_sub(width);
    }
    if handle.contains(ResizeHandle::TOP) {
        result.y = result
            .y
            .saturating_add(rectangle.height)
            .saturating_sub(height);
    }
    result
}

const fn handle_axis(handle: ResizeHandle, negative: ResizeHandle, positive: ResizeHandle) -> i32 {
    if handle.contains(negative) {
        -1
    } else if handle.contains(positive) {
        1
    } else {
        0
    }
}
