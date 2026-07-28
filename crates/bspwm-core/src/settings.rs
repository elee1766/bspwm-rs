use crate::types::{
    AutomaticScheme, ButtonIndex, ChildPolarity, HonorSizeHintsMode, Padding, PointerAction,
    PointerModifier, StateTransitions, Tightness,
};

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Settings {
    pub external_rules_command: String,
    pub status_prefix: String,
    pub normal_border_color: String,
    pub active_border_color: String,
    pub focused_border_color: String,
    pub presel_feedback_color: String,
    pub padding: Padding,
    pub monocle_padding: Padding,
    pub window_gap: i32,
    pub border_width: u32,
    pub split_ratio: f64,
    pub initial_polarity: ChildPolarity,
    pub automatic_scheme: AutomaticScheme,
    pub removal_adjustment: bool,
    pub directional_focus_tightness: Tightness,
    pub pointer_modifier: PointerModifier,
    pub pointer_motion_interval: u32,
    pub pointer_resize_sync: bool,
    pub pointer_actions: [PointerAction; 3],
    pub mapping_events_count: i8,
    pub presel_feedback: bool,
    pub borderless_monocle: bool,
    pub gapless_monocle: bool,
    pub single_monocle: bool,
    pub borderless_singleton: bool,
    pub focus_follows_pointer: bool,
    pub pointer_follows_focus: bool,
    pub pointer_follows_monitor: bool,
    pub click_to_focus: ButtonIndex,
    pub swallow_first_click: bool,
    pub enable_ewmh_ping: bool,
    pub enable_ewmh_allowed_actions: bool,
    pub ignore_ewmh_focus: bool,
    pub ignore_ewmh_struts: bool,
    pub ignore_ewmh_fullscreen: StateTransitions,
    pub center_pseudo_tiled: bool,
    pub honor_size_hints: HonorSizeHintsMode,
    pub remove_disabled_monitors: bool,
    pub remove_unplugged_monitors: bool,
    pub merge_overlapping_monitors: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            external_rules_command: String::new(),
            status_prefix: "W".into(),
            normal_border_color: "#30302f".into(),
            active_border_color: "#474645".into(),
            focused_border_color: "#817f7f".into(),
            presel_feedback_color: "#f4d775".into(),
            padding: Padding::default(),
            monocle_padding: Padding::default(),
            window_gap: 6,
            border_width: 1,
            split_ratio: 0.5,
            initial_polarity: ChildPolarity::SecondChild,
            automatic_scheme: AutomaticScheme::LongestSide,
            removal_adjustment: true,
            directional_focus_tightness: Tightness::High,
            pointer_modifier: PointerModifier::Mod4,
            pointer_motion_interval: 17,
            pointer_resize_sync: false,
            pointer_actions: [
                PointerAction::Move,
                PointerAction::ResizeSide,
                PointerAction::ResizeCorner,
            ],
            mapping_events_count: 1,
            presel_feedback: true,
            borderless_monocle: false,
            gapless_monocle: false,
            single_monocle: false,
            borderless_singleton: false,
            focus_follows_pointer: false,
            pointer_follows_focus: false,
            pointer_follows_monitor: false,
            click_to_focus: ButtonIndex::Button1,
            swallow_first_click: false,
            enable_ewmh_ping: false,
            enable_ewmh_allowed_actions: false,
            ignore_ewmh_focus: false,
            ignore_ewmh_struts: false,
            ignore_ewmh_fullscreen: StateTransitions::NONE,
            center_pseudo_tiled: true,
            honor_size_hints: HonorSizeHintsMode::No,
            remove_disabled_monitors: false,
            remove_unplugged_monitors: false,
            merge_overlapping_monitors: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_upstream_settings() {
        let settings = Settings::default();
        assert_eq!(settings.status_prefix, "W");
        assert_eq!(settings.window_gap, 6);
        assert_eq!(settings.border_width, 1);
        assert!((settings.split_ratio - 0.5).abs() < f64::EPSILON);
        assert_eq!(settings.pointer_modifier, PointerModifier::Mod4);
        assert_eq!(settings.pointer_motion_interval, 17);
        assert!(!settings.pointer_resize_sync);
        assert_eq!(settings.click_to_focus, ButtonIndex::Button1);
        assert!(!settings.enable_ewmh_ping);
        assert!(!settings.enable_ewmh_allowed_actions);
        assert_eq!(settings.ignore_ewmh_fullscreen, StateTransitions::NONE);
    }
}
