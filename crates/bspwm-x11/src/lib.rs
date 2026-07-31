use xcb::x;

pub(crate) use bspwm_core::{geometry, rule, settings, types};
pub(crate) use bspwm_model::{tree, world};

pub(crate) mod query {
    pub use bspwm_model::world::Coordinates;
}

pub const ROOT_EVENT_MASK: x::EventMask = x::EventMask::SUBSTRUCTURE_REDIRECT
    .union(x::EventMask::SUBSTRUCTURE_NOTIFY)
    .union(x::EventMask::STRUCTURE_NOTIFY)
    .union(x::EventMask::BUTTON_PRESS)
    .union(x::EventMask::PROPERTY_CHANGE)
    .union(x::EventMask::FOCUS_CHANGE);

pub mod events;
pub mod ewmh;
pub mod monitor;
pub mod pointer;
pub mod window;
pub mod x11;
