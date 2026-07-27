use crate::types::{Padding, Rectangle};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StrutPartial {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
    pub left_start_y: u32,
    pub left_end_y: u32,
    pub right_start_y: u32,
    pub right_end_y: u32,
    pub top_start_x: u32,
    pub top_end_x: u32,
    pub bottom_start_x: u32,
    pub bottom_end_x: u32,
}

#[must_use]
pub fn parse_strut_partial(values: &[u32]) -> Option<StrutPartial> {
    let values: &[u32; 12] = values.get(..12)?.try_into().ok()?;
    Some(StrutPartial {
        left: values[0],
        right: values[1],
        top: values[2],
        bottom: values[3],
        left_start_y: values[4],
        left_end_y: values[5],
        right_start_y: values[6],
        right_end_y: values[7],
        top_start_x: values[8],
        top_end_x: values[9],
        bottom_start_x: values[10],
        bottom_end_x: values[11],
    })
}

#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn apply_strut_partial(
    padding: &mut Padding,
    rectangle: Rectangle,
    screen_width: u16,
    screen_height: u16,
    strut: StrutPartial,
) -> bool {
    let x = rectangle.x;
    let y = rectangle.y;
    let right = rectangle.right();
    let bottom = rectangle.bottom();
    let mut changed = false;
    let mut apply = |side: &mut i32, amount: i32| {
        *side = if *side < 0 {
            side.saturating_add(amount)
        } else {
            (*side).max(amount)
        };
        changed = true;
    };
    let cast = |value: u32| i32::from(value as u16 as i16);
    let left = cast(strut.left);
    if x < left
        && left < right - 1
        && cast(strut.left_end_y) >= y
        && cast(strut.left_start_y) < bottom
    {
        apply(&mut padding.left, left - x);
    }
    let right_edge = cast(u32::from(screen_width).wrapping_sub(strut.right));
    if right > right_edge
        && right_edge > x
        && cast(strut.right_end_y) >= y
        && cast(strut.right_start_y) < bottom
    {
        apply(&mut padding.right, right - right_edge);
    }
    let top = cast(strut.top);
    if y < top && top < bottom - 1 && cast(strut.top_end_x) >= x && cast(strut.top_start_x) < right
    {
        apply(&mut padding.top, top - y);
    }
    let bottom_edge = cast(u32::from(screen_height).wrapping_sub(strut.bottom));
    if bottom > bottom_edge
        && bottom_edge > y
        && cast(strut.bottom_end_x) >= x
        && cast(strut.bottom_start_x) < right
    {
        apply(&mut padding.bottom, bottom - bottom_edge);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_partial_struts_to_intersecting_monitor_edges() {
        let strut = parse_strut_partial(&[0, 0, 30, 0, 0, 0, 0, 0, 100, 199, 0, 0]).unwrap();
        let mut left = Padding::default();
        let mut right = Padding::default();
        assert!(!apply_strut_partial(
            &mut left,
            Rectangle::new(0, 0, 100, 100),
            200,
            100,
            strut,
        ));
        assert!(apply_strut_partial(
            &mut right,
            Rectangle::new(100, 0, 100, 100),
            200,
            100,
            strut,
        ));
        assert_eq!(right.top, 30);
    }

    #[test]
    fn extends_negative_padding_and_rejects_malformed_payloads() {
        assert!(parse_strut_partial(&[1; 11]).is_none());
        let mut padding = Padding {
            top: -10,
            ..Padding::default()
        };
        let strut = parse_strut_partial(&[0, 0, 20, 0, 0, 0, 0, 0, 0, 99, 0, 0]).unwrap();
        assert!(apply_strut_partial(
            &mut padding,
            Rectangle::new(0, 0, 100, 100),
            100,
            100,
            strut,
        ));
        assert_eq!(padding.top, 10);
    }
}
