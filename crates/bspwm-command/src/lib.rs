pub(crate) use bspwm_core::{parse, rule, settings, types};
pub(crate) use bspwm_model::{arrange, history, monitor, stack, tree, world};
pub(crate) use bspwm_state::{query, restore, state};

pub(crate) mod common {
    pub use bspwm_ipc::FAILURE_MESSAGE;
}

pub(crate) mod pointer {
    pub use bspwm_core::pointer::{ResizeInput, plan_floating_move, plan_floating_resize};
    pub use bspwm_model::resize::{apply_tiled_resize_plan, plan_tiled_resize};
}

pub mod commands;
pub mod messages;
pub mod subscribe;

impl messages::Response for bspwm_ipc::UnixResponse {
    fn close(&mut self) -> std::io::Result<()> {
        bspwm_ipc::UnixResponse::close(self)
    }
}
