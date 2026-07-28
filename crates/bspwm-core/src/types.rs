pub const MAXLEN: usize = 256;
pub const SMALEN: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitType {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticScheme {
    LongestSide,
    Alternate,
    Spiral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HonorSizeHintsMode {
    No,
    Yes,
    Floating,
    Tiled,
    Default,
}

impl HonorSizeHintsMode {
    #[must_use]
    pub const fn protocol_name(self) -> &'static str {
        match self {
            Self::No => "false",
            Self::Yes => "true",
            Self::Floating => "floating",
            Self::Tiled => "tiled",
            Self::Default => "",
        }
    }

    #[must_use]
    pub const fn should_honor(self, state: ClientState) -> bool {
        matches!(self, Self::Yes) && !matches!(state, ClientState::Fullscreen)
            || matches!(self, Self::Tiled) && matches!(state, ClientState::Tiled)
            || matches!(self, Self::Floating)
                && matches!(state, ClientState::Floating | ClientState::PseudoTiled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientState {
    Tiled,
    PseudoTiled,
    Floating,
    Fullscreen,
}

impl ClientState {
    #[must_use]
    pub const fn is_tiled(self) -> bool {
        matches!(self, Self::Tiled | Self::PseudoTiled)
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct WmFlags: u16 {
        const MODAL = 1 << 0;
        const STICKY = 1 << 1;
        const MAXIMIZED_VERT = 1 << 2;
        const MAXIMIZED_HORZ = 1 << 3;
        const SHADED = 1 << 4;
        const SKIP_TASKBAR = 1 << 5;
        const SKIP_PAGER = 1 << 6;
        const HIDDEN = 1 << 7;
        const FULLSCREEN = 1 << 8;
        const ABOVE = 1 << 9;
        const BELOW = 1 << 10;
        const DEMANDS_ATTENTION = 1 << 11;
        const FOCUSED = 1 << 12;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackLayer {
    Below,
    Normal,
    Above,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OptionBool {
    #[default]
    None,
    True,
    False,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CycleDirection {
    Next,
    Prev,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CirculateDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryDirection {
    Older,
    Newer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    North,
    West,
    South,
    East,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct ResizeHandle: u8 {
        const LEFT = 1 << 0;
        const TOP = 1 << 1;
        const RIGHT = 1 << 2;
        const BOTTOM = 1 << 3;
        const TOP_LEFT = Self::TOP.bits() | Self::LEFT.bits();
        const TOP_RIGHT = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
        const BOTTOM_LEFT = Self::BOTTOM.bits() | Self::LEFT.bits();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerAction {
    None,
    Focus,
    Move,
    ResizeSide,
    ResizeCorner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    Tiled,
    Monocle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Flip {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildPolarity {
    FirstChild,
    SecondChild,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tightness {
    Low,
    High,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct StateTransitions: u8 {
        const NONE = 0;
        const ENTER = 1 << 0;
        const EXIT = 1 << 1;
        const ALL = Self::ENTER.bits() | Self::EXIT.bits();
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct SubscriberMask: u32 {
        const REPORT = 1 << 0;
        const MONITOR_ADD = 1 << 1;
        const MONITOR_RENAME = 1 << 2;
        const MONITOR_REMOVE = 1 << 3;
        const MONITOR_SWAP = 1 << 4;
        const MONITOR_FOCUS = 1 << 5;
        const MONITOR_GEOMETRY = 1 << 6;
        const DESKTOP_ADD = 1 << 7;
        const DESKTOP_RENAME = 1 << 8;
        const DESKTOP_REMOVE = 1 << 9;
        const DESKTOP_SWAP = 1 << 10;
        const DESKTOP_TRANSFER = 1 << 11;
        const DESKTOP_FOCUS = 1 << 12;
        const DESKTOP_ACTIVATE = 1 << 13;
        const DESKTOP_LAYOUT = 1 << 14;
        const NODE_ADD = 1 << 15;
        const NODE_REMOVE = 1 << 16;
        const NODE_SWAP = 1 << 17;
        const NODE_TRANSFER = 1 << 18;
        const NODE_FOCUS = 1 << 19;
        const NODE_PRESEL = 1 << 20;
        const NODE_STACK = 1 << 21;
        const NODE_ACTIVATE = 1 << 22;
        const NODE_GEOMETRY = 1 << 23;
        const NODE_STATE = 1 << 24;
        const NODE_FLAG = 1 << 25;
        const NODE_LAYER = 1 << 26;
        const POINTER_ACTION = 1 << 27;
        const MONITOR = (1 << 7) - (1 << 1);
        const DESKTOP = (1 << 15) - (1 << 7);
        const NODE = (1 << 27) - (1 << 15);
        const ALL = (1 << 28) - 1;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonIndex {
    None,
    Any,
    Button1,
    Button2,
    Button3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerModifier {
    Shift,
    Lock,
    Control,
    Mod1,
    Mod2,
    Mod3,
    Mod4,
    Mod5,
}

impl PointerModifier {
    #[must_use]
    pub const fn mask(self) -> u16 {
        match self {
            Self::Shift => 1,
            Self::Lock => 1 << 1,
            Self::Control => 1 << 2,
            Self::Mod1 => 1 << 3,
            Self::Mod2 => 1 << 4,
            Self::Mod3 => 1 << 5,
            Self::Mod4 => 1 << 6,
            Self::Mod5 => 1 << 7,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    #[must_use]
    pub fn from_x11(x: i16, y: i16) -> Self {
        Self {
            x: i32::from(x),
            y: i32::from(y),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Rectangle {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rectangle {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub fn from_x11(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self {
            x: i32::from(x),
            y: i32::from(y),
            width: i32::from(width),
            height: i32::from(height),
        }
    }

    #[must_use]
    pub const fn left(self) -> i32 {
        self.x
    }

    #[must_use]
    pub const fn top(self) -> i32 {
        self.y
    }

    #[must_use]
    pub const fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }
}

impl std::fmt::Display for Rectangle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }
}

#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub const fn wrapping_i16(value: i32) -> i16 {
    value as i16
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
pub const fn wrapping_u16(value: i32) -> u16 {
    value as u16
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Padding {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Constraints {
    pub min_width: u16,
    pub min_height: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeSelect {
    pub automatic: OptionBool,
    pub focused: OptionBool,
    pub active: OptionBool,
    pub local: OptionBool,
    pub leaf: OptionBool,
    pub window: OptionBool,
    pub tiled: OptionBool,
    pub pseudo_tiled: OptionBool,
    pub floating: OptionBool,
    pub fullscreen: OptionBool,
    pub hidden: OptionBool,
    pub sticky: OptionBool,
    pub private: OptionBool,
    pub locked: OptionBool,
    pub marked: OptionBool,
    pub urgent: OptionBool,
    pub same_class: OptionBool,
    pub descendant_of: OptionBool,
    pub ancestor_of: OptionBool,
    pub below: OptionBool,
    pub normal: OptionBool,
    pub above: OptionBool,
    pub horizontal: OptionBool,
    pub vertical: OptionBool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DesktopSelect {
    pub occupied: OptionBool,
    pub focused: OptionBool,
    pub active: OptionBool,
    pub urgent: OptionBool,
    pub local: OptionBool,
    pub tiled: OptionBool,
    pub monocle: OptionBool,
    pub user_tiled: OptionBool,
    pub user_monocle: OptionBool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MonitorSelect {
    pub occupied: OptionBool,
    pub focused: OptionBool,
}
