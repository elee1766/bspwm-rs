use crate::tree::Client;
use crate::types::{ClientState, StackLayer};

#[must_use]
pub const fn stack_level(client: &Client) -> u8 {
    let layer = match client.layer {
        StackLayer::Below => 0,
        StackLayer::Normal => 1,
        StackLayer::Above => 2,
    };
    let state = if client.state.is_tiled() {
        0
    } else if matches!(client.state, ClientState::Floating) {
        1
    } else {
        2
    };
    3 * layer + state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn stack_levels_preserve_layer_before_state_order() {
        let mut client = Client::from_settings(&Settings::default());
        assert_eq!(stack_level(&client), 3);
        client.state = ClientState::Fullscreen;
        assert_eq!(stack_level(&client), 5);
        client.layer = StackLayer::Above;
        client.state = ClientState::Tiled;
        assert_eq!(stack_level(&client), 6);
    }
}
