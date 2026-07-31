use crate::types::{
    AutomaticScheme, ButtonIndex, ChildPolarity, CirculateDirection, ClientState, CycleDirection,
    DesktopSelect, Direction, Flip, HistoryDirection, HonorSizeHintsMode, Layout, MonitorSelect,
    NodeSelect, OptionBool, PointerAction, PointerModifier, Rectangle, ResizeHandle, SplitType,
    StackLayer, StateTransitions, SubscriberMask, Tightness,
};

/// A string did not match the canonical protocol values for a domain type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseProtocolValueError {
    kind: &'static str,
    value: String,
}

impl ParseProtocolValueError {
    fn new(kind: &'static str, value: &str) -> Self {
        Self {
            kind,
            value: value.to_owned(),
        }
    }
}

impl std::fmt::Display for ParseProtocolValueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid {} value {:?}", self.kind, self.value)
    }
}

impl std::error::Error for ParseProtocolValueError {}

macro_rules! serde_string {
    ($type:ty) => {
        impl serde::Serialize for $type {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $type {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = <String as serde::Deserialize>::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

/// Builds an exact (whole-string) parser from a symbol table.
///
/// The `printable` form additionally implements canonical protocol naming,
/// `Display`, `FromStr`, and string-based Serde from the same table.
macro_rules! exact_parser {
    ($name:ident, $type:ty, {$($input:literal => $value:expr),+ $(,)?}) => {
        #[must_use]
        pub fn $name(input: &str) -> Option<$type> {
            match input {
                $($input => Some($value),)+
                _ => None,
            }
        }
    };
    ($name:ident, $type:ident, printable {$($input:literal => $variant:ident),+ $(,)?}) => {
        exact_parser!($name, $type, {$($input => $type::$variant),+});

        impl $type {
            /// Upstream protocol spelling of this value.
            #[must_use]
            pub const fn protocol_name(self) -> &'static str {
                match self {
                    $(Self::$variant => $input,)+
                }
            }
        }

        impl std::fmt::Display for $type {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.protocol_name())
            }
        }

        impl std::str::FromStr for $type {
            type Err = ParseProtocolValueError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                $name(value)
                    .ok_or_else(|| ParseProtocolValueError::new(stringify!($type), value))
            }
        }

        serde_string!($type);
    };
}

exact_parser!(parse_bool, bool, {
    "true" => true,
    "on" => true,
    "false" => false,
    "off" => false,
});
exact_parser!(parse_split_type, SplitType, printable {
    "horizontal" => Horizontal,
    "vertical" => Vertical,
});
exact_parser!(parse_layout, Layout, printable {
    "monocle" => Monocle,
    "tiled" => Tiled,
});
exact_parser!(parse_client_state, ClientState, printable {
    "tiled" => Tiled,
    "pseudo_tiled" => PseudoTiled,
    "floating" => Floating,
    "fullscreen" => Fullscreen,
});
exact_parser!(parse_stack_layer, StackLayer, printable {
    "below" => Below,
    "normal" => Normal,
    "above" => Above,
});
exact_parser!(parse_direction, Direction, printable {
    "north" => North,
    "west" => West,
    "south" => South,
    "east" => East,
});
exact_parser!(parse_cycle_direction, CycleDirection, {
    "next" => CycleDirection::Next,
    "prev" => CycleDirection::Prev,
});
exact_parser!(parse_circulate_direction, CirculateDirection, {
    "forward" => CirculateDirection::Forward,
    "backward" => CirculateDirection::Backward,
});
exact_parser!(parse_history_direction, HistoryDirection, {
    "older" => HistoryDirection::Older,
    "newer" => HistoryDirection::Newer,
});
exact_parser!(parse_flip, Flip, {
    "horizontal" => Flip::Horizontal,
    "vertical" => Flip::Vertical,
});
exact_parser!(parse_resize_handle, ResizeHandle, {
    "left" => ResizeHandle::LEFT,
    "top" => ResizeHandle::TOP,
    "right" => ResizeHandle::RIGHT,
    "bottom" => ResizeHandle::BOTTOM,
    "top_left" => ResizeHandle::TOP_LEFT,
    "top_right" => ResizeHandle::TOP_RIGHT,
    "bottom_right" => ResizeHandle::BOTTOM_RIGHT,
    "bottom_left" => ResizeHandle::BOTTOM_LEFT,
});
exact_parser!(parse_pointer_modifier, PointerModifier, printable {
    "shift" => Shift,
    "lock" => Lock,
    "control" => Control,
    "mod1" => Mod1,
    "mod2" => Mod2,
    "mod3" => Mod3,
    "mod4" => Mod4,
    "mod5" => Mod5,
});
exact_parser!(parse_button_index, ButtonIndex, printable {
    "any" => Any,
    "button1" => Button1,
    "button2" => Button2,
    "button3" => Button3,
    "none" => None,
});
exact_parser!(parse_pointer_action, PointerAction, printable {
    "move" => Move,
    "resize_corner" => ResizeCorner,
    "resize_side" => ResizeSide,
    "focus" => Focus,
    "none" => None,
});
exact_parser!(parse_child_polarity, ChildPolarity, printable {
    "first_child" => FirstChild,
    "second_child" => SecondChild,
});
exact_parser!(parse_automatic_scheme, AutomaticScheme, printable {
    "longest_side" => LongestSide,
    "alternate" => Alternate,
    "spiral" => Spiral,
});
exact_parser!(parse_tightness, Tightness, printable {
    "high" => High,
    "low" => Low,
});

#[must_use]
pub fn is_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

/// Converts `#RRGGBB` to the opaque pixel used by bspwm on 32-bit visuals.
#[must_use]
pub fn color_pixel(value: &str) -> u32 {
    u32::from_str_radix(value.trim_start_matches('#'), 16).unwrap_or_default() | 0xFF00_0000
}

#[must_use]
pub fn parse_honor_size_hints_mode(input: &str) -> Option<HonorSizeHintsMode> {
    match input {
        "true" | "on" => Some(HonorSizeHintsMode::Yes),
        "false" | "off" => Some(HonorSizeHintsMode::No),
        "floating" => Some(HonorSizeHintsMode::Floating),
        "tiled" => Some(HonorSizeHintsMode::Tiled),
        _ => None,
    }
}

impl std::fmt::Display for HonorSizeHintsMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.protocol_name())
    }
}

impl std::str::FromStr for HonorSizeHintsMode {
    type Err = ParseProtocolValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Ok(Self::Default);
        }
        parse_honor_size_hints_mode(value)
            .ok_or_else(|| ParseProtocolValueError::new("HonorSizeHintsMode", value))
    }
}

serde_string!(HonorSizeHintsMode);

#[must_use]
pub fn parse_state_transition(input: &str) -> Option<StateTransitions> {
    match input {
        "none" => Some(StateTransitions::NONE),
        "all" => Some(StateTransitions::ALL),
        _ => {
            let mut transitions = StateTransitions::NONE;
            let mut found = false;
            for token in input.split(',').filter(|token| !token.is_empty()) {
                found = true;
                transitions = transitions.union(match token {
                    "enter" => StateTransitions::ENTER,
                    "exit" => StateTransitions::EXIT,
                    _ => return None,
                });
            }
            found.then_some(transitions)
        }
    }
}

impl std::fmt::Display for StateTransitions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return formatter.write_str("none");
        }
        if self.contains(Self::ENTER) {
            formatter.write_str("enter")?;
        }
        if self.contains(Self::EXIT) {
            if self.contains(Self::ENTER) {
                formatter.write_str(",")?;
            }
            formatter.write_str("exit")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for StateTransitions {
    type Err = ParseProtocolValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_state_transition(value)
            .ok_or_else(|| ParseProtocolValueError::new("StateTransitions", value))
    }
}

serde_string!(StateTransitions);

/// A C-style integer literal at the front of a byte slice: leading ASCII
/// whitespace, an optional sign, an optional radix prefix, then a run of digits.
///
/// This is the shared core of the hand-rolled scanners in the port; callers pick
/// their own overflow policy by accumulating `digits` themselves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScannedInt<'a> {
    negative: bool,
    radix: u32,
    /// Offset of the first digit byte, past any sign and `0x` prefix.
    start: usize,
    /// The digit bytes that were consumed; empty when there were none.
    digits: &'a [u8],
}

/// Scans one C integer prefix.
///
/// `base_zero` enables `strtol(.., 0)`-style radix sniffing. `strict_hex_prefix`
/// additionally requires a hex digit right after `0x`, as `strtol` and
/// `scanf("%i")` do; without it a bare `0x` is a digitless hex literal rather
/// than a fallback to octal `0`.
pub(crate) fn scan_c_int(bytes: &[u8], base_zero: bool, strict_hex_prefix: bool) -> ScannedInt<'_> {
    let mut index = 0;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    let negative = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let radix = if !base_zero || bytes.get(index) != Some(&b'0') {
        10
    } else if matches!(bytes.get(index + 1), Some(b'x' | b'X'))
        && (!strict_hex_prefix || bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
    {
        index += 2;
        16
    } else {
        8
    };
    let start = index;
    while bytes.get(index).is_some_and(|byte| is_digit(*byte, radix)) {
        index += 1;
    }
    ScannedInt {
        negative,
        radix,
        start,
        digits: &bytes[start..index],
    }
}

const fn is_digit(byte: u8, radix: u32) -> bool {
    match radix {
        8 => byte.is_ascii_digit() && byte < b'8',
        16 => byte.is_ascii_hexdigit(),
        _ => byte.is_ascii_digit(),
    }
}

const fn digit_value(byte: u8) -> u32 {
    match byte {
        b'a'..=b'f' => (byte - b'a') as u32 + 10,
        b'A'..=b'F' => (byte - b'A') as u32 + 10,
        _ => (byte - b'0') as u32,
    }
}

impl ScannedInt<'_> {
    /// Offset just past the literal.
    const fn end(&self) -> usize {
        self.start + self.digits.len()
    }

    /// `atoi`/`scanf` accumulation: silently wraps on overflow, 0 when empty.
    #[allow(clippy::cast_possible_wrap)]
    fn wrapping_i32(&self) -> i32 {
        let radix = self.radix as i32;
        let value = self.digits.iter().fold(0_i32, |value, byte| {
            value
                .wrapping_mul(radix)
                .wrapping_add(digit_value(*byte) as i32)
        });
        if self.negative {
            value.wrapping_neg()
        } else {
            value
        }
    }

    /// `strtol`-style accumulation: `None` on overflow.
    fn checked_i128(&self) -> Option<i128> {
        let radix = i128::from(self.radix);
        let value = self.digits.iter().try_fold(0_i128, |value, byte| {
            value
                .checked_mul(radix)?
                .checked_add(i128::from(digit_value(*byte)))
        })?;
        Some(if self.negative { -value } else { value })
    }
}

/// `scanf("%i")`: base-0 prefix scan with wrapping accumulation.
#[must_use]
pub fn scan_wrapping_i32(bytes: &[u8]) -> Option<i32> {
    let number = scan_c_int(bytes, true, true);
    (!number.digits.is_empty()).then(|| number.wrapping_i32())
}

/// `atoi`: never fails, yields 0 when there is no number at all.
fn c_atoi(input: &str) -> i32 {
    scan_c_int(input.as_bytes(), false, false).wrapping_i32()
}

#[must_use]
pub fn parse_degree(input: &str) -> Option<i32> {
    let degree = c_atoi(input).rem_euclid(360);
    (degree % 90 == 0).then_some(degree)
}

#[must_use]
pub fn parse_id(input: &str) -> Option<u32> {
    let input = input.strip_suffix('\0').unwrap_or(input);
    if input.is_empty() {
        return Some(0);
    }
    // Unlike the prefix scanners this one demands the whole remainder be digits,
    // by way of `from_str_radix` -- which also tolerates a second sign byte.
    let number = scan_c_int(input.as_bytes(), true, true);
    let digits = &input[number.start..];
    if digits.is_empty() {
        return None;
    }
    let magnitude = u64::from_str_radix(digits, number.radix).ok()?;
    #[allow(clippy::cast_sign_loss)]
    let limit = if number.negative {
        1_u64 << 63
    } else {
        i64::MAX as u64
    };
    if magnitude > limit {
        return None;
    }
    let signed = if number.negative {
        -(i128::from(magnitude))
    } else {
        i128::from(magnitude)
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = signed as u32;
    Some(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoolDeclaration<'a> {
    Toggle { key: Option<&'a str> },
    Set { key: Option<&'a str>, value: bool },
}

#[must_use]
pub fn parse_bool_declaration(input: &str) -> Option<BoolDeclaration<'_>> {
    let mut tokens = input.split('=').filter(|token| !token.is_empty());
    let key = tokens.next();
    match tokens.next() {
        Some(value) => Some(BoolDeclaration::Set {
            key,
            value: parse_bool(value)?,
        }),
        None => Some(BoolDeclaration::Toggle { key }),
    }
}

#[must_use]
pub fn parse_index(input: &str) -> Option<u16> {
    let (value, _) = parse_integer_prefix(input.strip_prefix('^')?, false)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = value as u16;
    Some(value)
}

fn parse_integer_prefix(input: &str, base_zero: bool) -> Option<(i128, usize)> {
    let number = scan_c_int(input.as_bytes(), base_zero, false);
    if number.digits.is_empty() {
        return None;
    }
    Some((number.checked_i128()?, number.end()))
}

#[must_use]
pub fn parse_rectangle(input: &str) -> Option<Rectangle> {
    let (width, used_width) = parse_integer_prefix(input, false)?;
    let rest = input.get(used_width..)?.strip_prefix('x')?;
    let (height, used_height) = parse_integer_prefix(rest, false)?;
    let rest = rest.get(used_height..)?.strip_prefix('+')?;
    let (x, used_x) = parse_integer_prefix(rest, true)?;
    let rest = rest.get(used_x..)?.strip_prefix('+')?;
    let (y, _) = parse_integer_prefix(rest, true)?;
    let (x, y, width, height) = (
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        i32::try_from(width).ok()?,
        i32::try_from(height).ok()?,
    );
    (width >= 0 && height >= 0).then(|| Rectangle::new(x, y, width, height))
}

#[must_use]
pub fn parse_subscriber_mask(input: &str) -> Option<SubscriberMask> {
    Some(match input {
        "report" => SubscriberMask::REPORT,
        "monitor_add" => SubscriberMask::MONITOR_ADD,
        "monitor_rename" => SubscriberMask::MONITOR_RENAME,
        "monitor_remove" => SubscriberMask::MONITOR_REMOVE,
        "monitor_swap" => SubscriberMask::MONITOR_SWAP,
        "monitor_focus" => SubscriberMask::MONITOR_FOCUS,
        "monitor_geometry" => SubscriberMask::MONITOR_GEOMETRY,
        "desktop_add" => SubscriberMask::DESKTOP_ADD,
        "desktop_rename" => SubscriberMask::DESKTOP_RENAME,
        "desktop_remove" => SubscriberMask::DESKTOP_REMOVE,
        "desktop_swap" => SubscriberMask::DESKTOP_SWAP,
        "desktop_transfer" => SubscriberMask::DESKTOP_TRANSFER,
        "desktop_focus" => SubscriberMask::DESKTOP_FOCUS,
        "desktop_activate" => SubscriberMask::DESKTOP_ACTIVATE,
        "desktop_layout" => SubscriberMask::DESKTOP_LAYOUT,
        "node_add" => SubscriberMask::NODE_ADD,
        "node_remove" => SubscriberMask::NODE_REMOVE,
        "node_swap" => SubscriberMask::NODE_SWAP,
        "node_transfer" => SubscriberMask::NODE_TRANSFER,
        "node_focus" => SubscriberMask::NODE_FOCUS,
        "node_presel" => SubscriberMask::NODE_PRESEL,
        "node_stack" => SubscriberMask::NODE_STACK,
        "node_activate" => SubscriberMask::NODE_ACTIVATE,
        "node_geometry" => SubscriberMask::NODE_GEOMETRY,
        "node_state" => SubscriberMask::NODE_STATE,
        "node_flag" => SubscriberMask::NODE_FLAG,
        "node_layer" => SubscriberMask::NODE_LAYER,
        "pointer_action" => SubscriberMask::POINTER_ACTION,
        "monitor" => SubscriberMask::MONITOR,
        "desktop" => SubscriberMask::DESKTOP,
        "node" => SubscriberMask::NODE,
        "all" => SubscriberMask::ALL,
        _ => return None,
    })
}

fn split_modifiers(input: &str) -> (&str, Vec<&str>) {
    let mut descriptor = input;
    let mut modifiers = Vec::new();
    while let Some(index) = descriptor.rfind('.') {
        modifiers.push(&descriptor[index + 1..]);
        descriptor = &descriptor[..index];
    }
    (descriptor, modifiers)
}

fn modifier_value(token: &str) -> (&str, OptionBool) {
    token
        .strip_prefix('!')
        .map_or((token, OptionBool::True), |name| (name, OptionBool::False))
}

macro_rules! apply_modifier {
    ($selection:ident, $name:ident, $value:ident, [$($field:ident),+ $(,)?]) => {
        match $name {
            $(stringify!($field) => $selection.$field = $value,)+
            _ => return None,
        }
    };
}

#[must_use]
pub fn parse_monitor_modifiers(input: &str) -> Option<(&str, MonitorSelect)> {
    let (descriptor, modifiers) = split_modifiers(input);
    let mut selection = MonitorSelect::default();
    for token in modifiers {
        let (name, value) = modifier_value(token);
        apply_modifier!(selection, name, value, [occupied, focused]);
    }
    Some((descriptor, selection))
}

#[must_use]
pub fn parse_desktop_modifiers(input: &str) -> Option<(&str, DesktopSelect)> {
    let (descriptor, modifiers) = split_modifiers(input);
    let mut selection = DesktopSelect::default();
    for token in modifiers {
        let (name, value) = modifier_value(token);
        apply_modifier!(
            selection,
            name,
            value,
            [
                occupied,
                focused,
                active,
                urgent,
                local,
                tiled,
                monocle,
                user_tiled,
                user_monocle,
            ]
        );
    }
    Some((descriptor, selection))
}

#[must_use]
pub fn parse_node_modifiers(input: &str) -> Option<(&str, NodeSelect)> {
    let (descriptor, modifiers) = split_modifiers(input);
    let mut selection = NodeSelect::default();
    for token in modifiers {
        let (name, value) = modifier_value(token);
        apply_modifier!(
            selection,
            name,
            value,
            [
                tiled,
                automatic,
                focused,
                active,
                local,
                leaf,
                window,
                pseudo_tiled,
                floating,
                fullscreen,
                hidden,
                sticky,
                private,
                locked,
                marked,
                urgent,
                same_class,
                descendant_of,
                ancestor_of,
                below,
                normal,
                above,
                horizontal,
                vertical,
            ]
        );
    }
    Some((descriptor, selection))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_parsers_are_exact() {
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("True"), None);
        assert_eq!(parse_client_state("tiled"), Some(ClientState::Tiled));
        assert_eq!(parse_client_state("Tiled"), None);
    }

    #[test]
    fn validates_only_full_rgb_hex_colors() {
        assert!(is_hex_color("#aBc123"));
        assert!(!is_hex_color("#abc"));
        assert!(!is_hex_color("112233"));
        assert!(!is_hex_color("#gg0000"));
        assert_eq!(color_pixel("#123456"), 0xFF12_3456);
    }

    #[test]
    fn generated_protocol_traits_share_one_canonical_spelling() {
        for name in ["tiled", "pseudo_tiled", "floating", "fullscreen"] {
            let state = name.parse::<ClientState>().unwrap();
            assert_eq!(state.to_string(), name);
            assert_eq!(parse_client_state(name), Some(state));
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{name}\"")
            );
            assert_eq!(
                serde_json::from_str::<ClientState>(&format!("\"{name}\"")).unwrap(),
                state
            );
        }
        for name in ["north", "west", "south", "east"] {
            assert_eq!(name.parse::<Direction>().unwrap().to_string(), name);
        }
        for name in ["horizontal", "vertical"] {
            assert_eq!(name.parse::<SplitType>().unwrap().to_string(), name);
        }
        for name in ["below", "normal", "above"] {
            assert_eq!(name.parse::<StackLayer>().unwrap().to_string(), name);
        }
        for name in ["tiled", "monocle"] {
            assert_eq!(name.parse::<Layout>().unwrap().to_string(), name);
        }
        assert_eq!("mod4".parse(), Ok(PointerModifier::Mod4));
        assert_eq!(PointerModifier::Mod4.mask(), 64);
        assert!("Mod4".parse::<PointerModifier>().is_err());

        for transitions in [StateTransitions::NONE, StateTransitions::ALL] {
            let json = serde_json::to_string(&transitions).unwrap();
            assert_eq!(
                serde_json::from_str::<StateTransitions>(&json).unwrap(),
                transitions
            );
        }
        for mode in [HonorSizeHintsMode::No, HonorSizeHintsMode::Default] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(
                serde_json::from_str::<HonorSizeHintsMode>(&json).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn state_transitions_skip_empty_comma_fields_like_strtok() {
        assert_eq!(
            parse_state_transition("enter,,exit"),
            Some(StateTransitions::ALL)
        );
        assert_eq!(
            parse_state_transition(",enter,"),
            Some(StateTransitions::ENTER)
        );
        assert_eq!(parse_state_transition(",,,"), None);
    }

    #[test]
    fn degree_parser_keeps_atoi_quirks() {
        assert_eq!(parse_degree("-90"), Some(270));
        assert_eq!(parse_degree("450junk"), Some(90));
        assert_eq!(parse_degree("garbage"), Some(0));
        assert_eq!(parse_degree("45"), None);
    }

    #[test]
    fn parses_base_zero_ids_with_wrapping_conversion() {
        assert_eq!(parse_id(""), Some(0));
        assert_eq!(parse_id("010"), Some(8));
        assert_eq!(parse_id("0x10"), Some(16));
        assert_eq!(parse_id("-1"), Some(u32::MAX));
        assert_eq!(parse_id("9223372036854775807"), Some(u32::MAX));
        assert_eq!(parse_id("-9223372036854775808"), Some(0));
        assert_eq!(parse_id("9223372036854775808"), None);
        assert_eq!(parse_id("-9223372036854775809"), None);
        assert_eq!(parse_id("   "), None);
        assert_eq!(parse_id("0x"), None);
        assert_eq!(parse_id("08"), None);
        assert_eq!(parse_id("10junk"), None);
    }

    #[test]
    fn bool_declarations_keep_strtok_semantics() {
        assert_eq!(
            parse_bool_declaration("flag==true"),
            Some(BoolDeclaration::Set {
                key: Some("flag"),
                value: true
            })
        );
        assert_eq!(
            parse_bool_declaration("=true"),
            Some(BoolDeclaration::Toggle { key: Some("true") })
        );
    }

    #[test]
    fn index_and_rectangle_accept_numeric_prefixes() {
        assert_eq!(parse_index("^-1junk"), Some(u16::MAX));
        assert_eq!(
            parse_rectangle("10x20+-1+0x10garbage"),
            Some(Rectangle::new(-1, 16, 10, 20))
        );
    }

    #[test]
    fn rectangles_use_wide_coordinates_and_nonnegative_dimensions() {
        assert_eq!(
            parse_rectangle("70000x80000+40000+-40000"),
            Some(Rectangle::new(40_000, -40_000, 70_000, 80_000))
        );
        assert_eq!(parse_rectangle("-1x1+0+0"), None);
        assert_eq!(parse_rectangle("1x-1+0+0"), None);
        assert_eq!(parse_rectangle("1x1+2147483648+0"), None);
    }

    #[test]
    fn subscriber_groups_have_upstream_ranges() {
        assert_eq!(
            parse_subscriber_mask("monitor"),
            Some(SubscriberMask::MONITOR)
        );
        assert_eq!(SubscriberMask::MONITOR.bits(), 0x7e);
        assert_eq!(SubscriberMask::DESKTOP.bits(), 0x7f80);
        assert_eq!(SubscriberMask::NODE.bits(), 0x07ff_8000);
        assert_eq!(SubscriberMask::ALL.bits(), 0x0fff_ffff);
    }

    #[test]
    fn modifiers_are_applied_right_to_left_so_leftmost_wins() {
        let (descriptor, selection) = parse_node_modifiers("focused.focused.!focused").unwrap();
        assert_eq!(descriptor, "focused");
        assert_eq!(selection.focused, OptionBool::True);
        assert!(parse_node_modifiers("node.unknown").is_none());
    }
}
