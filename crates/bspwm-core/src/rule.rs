use std::fmt::{self, Write};

use crate::parse::{
    parse_bool, parse_client_state, parse_direction, parse_honor_size_hints_mode, parse_rectangle,
    parse_stack_layer,
};
use crate::types::{ClientState, Direction, HonorSizeHintsMode, Rectangle, StackLayer};

pub const MATCH_ANY: &str = "*";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rule {
    pub class_name: String,
    pub instance_name: String,
    pub name: String,
    pub effect: String,
    pub one_shot: bool,
}

impl Rule {
    #[must_use]
    pub fn from_cause(cause: &str, effect: impl Into<String>, one_shot: bool) -> Self {
        let [class_name, instance_name, name] = parse_cause(cause);
        Self {
            class_name,
            instance_name: if instance_name.is_empty() {
                MATCH_ANY.into()
            } else {
                instance_name
            },
            name: if name.is_empty() {
                MATCH_ANY.into()
            } else {
                name
            },
            effect: effect.into(),
            one_shot,
        }
    }

    #[must_use]
    pub fn matches(&self, properties: &WindowProperties) -> bool {
        matches_field(&self.class_name, &properties.class_name)
            && matches_field(&self.instance_name, &properties.instance_name)
            && matches_field(&self.name, &properties.name)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WindowProperties {
    pub class_name: String,
    pub instance_name: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinWindowType {
    Toolbar,
    Utility,
    Dialog,
    Dock,
    Desktop,
    Notification,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinWindowState {
    Fullscreen,
    Below,
    Above,
    Sticky,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuiltinRuleProperties {
    pub window_types: Vec<BuiltinWindowType>,
    pub window_states: Vec<BuiltinWindowState>,
    pub transient: bool,
    pub fixed_size: bool,
}

impl WindowProperties {
    #[must_use]
    pub fn new(
        class_name: impl Into<String>,
        instance_name: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            class_name: class_name.into(),
            instance_name: instance_name.into(),
            name: name.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct RuleConsequence {
    pub class_name: String,
    pub instance_name: String,
    pub name: String,
    pub monitor_desc: String,
    pub desktop_desc: String,
    pub node_desc: String,
    pub split_dir: Option<Direction>,
    pub split_ratio: f64,
    pub layer: Option<StackLayer>,
    pub state: Option<ClientState>,
    pub honor_size_hints: HonorSizeHintsMode,
    pub hidden: bool,
    pub sticky: bool,
    pub private: bool,
    pub locked: bool,
    pub marked: bool,
    pub center: bool,
    pub follow: bool,
    pub manage: bool,
    pub focus: bool,
    pub border: bool,
    pub rect: Option<Rectangle>,
}

impl Default for RuleConsequence {
    fn default() -> Self {
        Self {
            class_name: String::new(),
            instance_name: String::new(),
            name: String::new(),
            monitor_desc: String::new(),
            desktop_desc: String::new(),
            node_desc: String::new(),
            split_dir: None,
            split_ratio: 0.0,
            layer: None,
            state: None,
            honor_size_hints: HonorSizeHintsMode::Default,
            hidden: false,
            sticky: false,
            private: false,
            locked: false,
            marked: false,
            center: false,
            follow: false,
            manage: true,
            focus: true,
            border: true,
            rect: None,
        }
    }
}

impl RuleConsequence {
    pub fn set_window_properties(&mut self, properties: &WindowProperties) {
        self.class_name.clone_from(&properties.class_name);
        self.instance_name.clone_from(&properties.instance_name);
        self.name.clone_from(&properties.name);
    }

    #[must_use]
    pub fn window_properties(&self) -> WindowProperties {
        WindowProperties::new(&self.class_name, &self.instance_name, &self.name)
    }
}

pub fn apply_builtin_rules(
    properties: &BuiltinRuleProperties,
    put_dialogs_above: bool,
    consequence: &mut RuleConsequence,
) {
    for window_type in &properties.window_types {
        match window_type {
            BuiltinWindowType::Toolbar | BuiltinWindowType::Utility => consequence.focus = false,
            BuiltinWindowType::Dialog => {
                consequence.state = Some(ClientState::Floating);
                consequence.center = true;
                if put_dialogs_above {
                    consequence.layer = Some(StackLayer::Above);
                }
            }
            BuiltinWindowType::Dock
            | BuiltinWindowType::Desktop
            | BuiltinWindowType::Notification => consequence.manage = false,
        }
    }
    for window_state in &properties.window_states {
        match window_state {
            BuiltinWindowState::Fullscreen => consequence.state = Some(ClientState::Fullscreen),
            BuiltinWindowState::Below => consequence.layer = Some(StackLayer::Below),
            BuiltinWindowState::Above => consequence.layer = Some(StackLayer::Above),
            BuiltinWindowState::Sticky => consequence.sticky = true,
        }
    }
    if properties.transient || properties.fixed_size {
        consequence.state = Some(ClientState::Floating);
    }
}

impl fmt::Display for RuleConsequence {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_rule_consequence(output, self)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuleList {
    rules: Vec<Rule>,
}

impl RuleList {
    pub fn add_rule(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    pub fn remove_rule(&mut self, index: usize) -> Option<Rule> {
        (index < self.rules.len()).then(|| self.rules.remove(index))
    }

    pub fn remove_rule_by_cause(&mut self, cause: &str) {
        let [class_name, instance_name, name] = parse_cause(cause);
        self.rules.retain(|rule| {
            !(matches_field(&class_name, &rule.class_name)
                && matches_field(&instance_name, &rule.instance_name)
                && matches_field(&name, &rule.name))
        });
    }

    pub fn remove_rule_by_index(&mut self, index: usize) -> bool {
        self.remove_rule(index).is_some()
    }

    pub fn apply_rules(&mut self, consequence: &mut RuleConsequence) {
        let properties = consequence.window_properties();
        let mut index = 0;
        while index < self.rules.len() {
            if self.rules[index].matches(&properties) {
                parse_keys_values(&self.rules[index].effect, consequence);
                if self.rules[index].one_shot {
                    self.remove_rule(index);
                    break;
                }
            }
            index += 1;
        }
    }

    #[must_use]
    pub fn list_rules(&self) -> String {
        let mut output = String::new();
        for rule in &self.rules {
            let operator = if rule.one_shot { '-' } else { '=' };
            let _ = writeln!(
                output,
                "{}:{}:{} {operator}> {}",
                rule.class_name, rule.instance_name, rule.name, rule.effect
            );
        }
        output
    }

    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

pub fn parse_keys_values(input: &str, consequence: &mut RuleConsequence) {
    let mut tokens = input
        .split([' ', '=', ',', '\n'])
        .filter(|token| !token.is_empty());
    while let (Some(key), Some(value)) = (tokens.next(), tokens.next()) {
        parse_key_value(key, value, consequence);
    }
}

pub fn parse_key_value(key: &str, value: &str, consequence: &mut RuleConsequence) {
    match key {
        "monitor" => consequence.monitor_desc = value.into(),
        "desktop" => consequence.desktop_desc = value.into(),
        "node" => consequence.node_desc = value.into(),
        "split_dir" => {
            if let Some(direction) = parse_direction(value) {
                consequence.split_dir = Some(direction);
            }
        }
        "state" => {
            if let Some(state) = parse_client_state(value) {
                consequence.state = Some(state);
            }
        }
        "layer" => {
            if let Some(layer) = parse_stack_layer(value) {
                consequence.layer = Some(layer);
            }
        }
        "split_ratio" => {
            if let Some(ratio) =
                parse_float_prefix(value).filter(|ratio| *ratio > 0.0 && *ratio < 1.0)
            {
                consequence.split_ratio = ratio;
            }
        }
        "rectangle" => consequence.rect = parse_rectangle(value),
        "honor_size_hints" => {
            consequence.honor_size_hints =
                parse_honor_size_hints_mode(value).unwrap_or(HonorSizeHintsMode::Default);
        }
        _ => {
            let Some(value) = parse_bool(value) else {
                return;
            };
            match key {
                "hidden" => consequence.hidden = value,
                "sticky" => consequence.sticky = value,
                "private" => consequence.private = value,
                "locked" => consequence.locked = value,
                "marked" => consequence.marked = value,
                "center" => consequence.center = value,
                "follow" => consequence.follow = value,
                "manage" => consequence.manage = value,
                "focus" => consequence.focus = value,
                "border" => consequence.border = value,
                _ => {}
            }
        }
    }
}

/// Formats a rule consequence into the given writer.
#[allow(clippy::missing_errors_doc)]
pub fn write_rule_consequence(
    output: &mut impl fmt::Write,
    consequence: &RuleConsequence,
) -> fmt::Result {
    write!(
        output,
        "monitor={} desktop={} node={} state={} layer={} honor_size_hints={} split_dir={} split_ratio={:.6} hidden={} sticky={} private={} locked={} marked={} center={} follow={} manage={} focus={} border={} rectangle=",
        consequence.monitor_desc,
        consequence.desktop_desc,
        consequence.node_desc,
        consequence.state.map_or("", ClientState::protocol_name),
        consequence.layer.map_or("", StackLayer::protocol_name),
        consequence.honor_size_hints.protocol_name(),
        consequence.split_dir.map_or("", Direction::protocol_name),
        consequence.split_ratio,
        on_off(consequence.hidden),
        on_off(consequence.sticky),
        on_off(consequence.private),
        on_off(consequence.locked),
        on_off(consequence.marked),
        on_off(consequence.center),
        on_off(consequence.follow),
        on_off(consequence.manage),
        on_off(consequence.focus),
        on_off(consequence.border),
    )?;
    if let Some(r) = consequence.rect {
        write!(output, "{r}")?;
    }
    Ok(())
}

/// Formats a rule consequence as a `String`.
#[must_use]
pub fn print_rule_consequence(consequence: &RuleConsequence) -> String {
    let mut output = String::new();
    // Writing to a String cannot fail.
    let _ = write_rule_consequence(&mut output, consequence);
    output
}

fn parse_cause(cause: &str) -> [String; 3] {
    let mut fields = [String::new(), String::new(), String::new()];
    let mut field = 0;
    let mut escaped = false;
    for character in cause.chars() {
        if escaped {
            fields[field].push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ':' && field < fields.len() - 1 {
            field += 1;
        } else if character == ':' {
            break;
        } else {
            fields[field].push(character);
        }
    }
    fields
}

fn matches_field(pattern: &str, value: &str) -> bool {
    pattern == MATCH_ANY || pattern == value
}

fn parse_float_prefix(input: &str) -> Option<f64> {
    (1..=input.len())
        .rev()
        .filter(|end| input.is_char_boundary(*end))
        .find_map(|end| input[..end].parse().ok())
}

const fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp, clippy::field_reassign_with_default)]

    use super::*;

    fn rule(cause: &str, effect: &str, one_shot: bool) -> Rule {
        Rule::from_cause(cause, effect, one_shot)
    }

    #[test]
    fn constructors_match_upstream_defaults() {
        assert_eq!(Rule::default(), Rule::default());
        let consequence = RuleConsequence::default();
        assert!(consequence.manage);
        assert!(consequence.focus);
        assert!(consequence.border);
        assert_eq!(consequence.honor_size_hints, HonorSizeHintsMode::Default);
        assert_eq!(consequence.split_ratio, 0.0);
        assert_eq!(consequence.state, None);
        assert_eq!(consequence.rect, None);
    }

    #[test]
    fn cause_construction_unescapes_colons_and_defaults_omitted_fields() {
        assert_eq!(
            Rule::from_cause(r"XTerm:term\:special", "focus=off", false),
            Rule {
                class_name: "XTerm".into(),
                instance_name: "term:special".into(),
                name: MATCH_ANY.into(),
                effect: "focus=off".into(),
                one_shot: false,
            }
        );
    }

    #[test]
    fn rules_are_ordered_and_list_format_matches_upstream() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("A:*:*", "focus=off", false));
        rules.add_rule(rule("B:b:title", "state=floating", true));
        assert_eq!(
            rules.list_rules(),
            "A:*:* => focus=off\nB:b:title -> state=floating\n"
        );
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.class_name.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
    }

    #[test]
    fn removal_by_index_reports_success_and_preserves_order() {
        let mut rules = RuleList::default();
        for class in ["A", "B", "C"] {
            let cause = format!("{class}:*:*");
            rules.add_rule(rule(&cause, "", false));
        }
        assert!(rules.remove_rule_by_index(1));
        assert!(!rules.remove_rule_by_index(9));
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.class_name.as_str())
                .collect::<Vec<_>>(),
            ["A", "C"]
        );
        assert_eq!(rules.remove_rule(0).unwrap().class_name, "A");
    }

    #[test]
    fn removal_by_cause_uses_only_exact_fields_or_a_whole_field_wildcard() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("Firefox:main:Docs", "", false));
        rules.add_rule(rule("Firefox:private:Docs", "", false));
        rules.add_rule(rule("Fire*:main:Docs", "", false));
        rules.remove_rule_by_cause("Firefox:*:Docs");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules.iter().next().unwrap().class_name, "Fire*");
    }

    #[test]
    fn parse_keys_values_has_strtok_pairing_and_ignores_a_dangling_key() {
        let mut consequence = RuleConsequence::default();
        parse_keys_values(
            "monitor=one, desktop two\nstate=floating focus=off dangling",
            &mut consequence,
        );
        assert_eq!(consequence.monitor_desc, "one");
        assert_eq!(consequence.desktop_desc, "two");
        assert_eq!(consequence.state, Some(ClientState::Floating));
        assert!(!consequence.focus);
    }

    #[test]
    fn parse_key_value_covers_typed_and_boolean_consequences() {
        let mut consequence = RuleConsequence::default();
        parse_keys_values(
            "split_dir=west layer=above split_ratio=0.625junk rectangle=80x60+-2+3 honor_size_hints=tiled hidden=on sticky=true private=off locked=true marked=on center=true follow=off manage=false focus=off border=false",
            &mut consequence,
        );
        assert_eq!(consequence.split_dir, Some(Direction::West));
        assert_eq!(consequence.layer, Some(StackLayer::Above));
        assert_eq!(consequence.split_ratio, 0.625);
        assert_eq!(consequence.rect, Some(Rectangle::new(-2, 3, 80, 60)));
        assert_eq!(consequence.honor_size_hints, HonorSizeHintsMode::Tiled);
        assert!(consequence.hidden && consequence.sticky && consequence.locked);
        assert!(consequence.marked && consequence.center);
        assert!(!consequence.private || consequence.follow || consequence.manage);
        assert!(!consequence.focus && !consequence.border);
    }

    #[test]
    fn invalid_values_follow_upstream_reset_and_preservation_rules() {
        let mut consequence = RuleConsequence::default();
        consequence.state = Some(ClientState::Tiled);
        consequence.split_ratio = 0.4;
        consequence.rect = Some(Rectangle::default());
        consequence.honor_size_hints = HonorSizeHintsMode::Yes;
        parse_keys_values(
            "state=invalid split_ratio=1 rectangle=invalid honor_size_hints=invalid focus=invalid",
            &mut consequence,
        );
        assert_eq!(consequence.state, Some(ClientState::Tiled));
        assert_eq!(consequence.split_ratio, 0.4);
        assert_eq!(consequence.rect, None);
        assert_eq!(consequence.honor_size_hints, HonorSizeHintsMode::Default);
        assert!(consequence.focus);
    }

    #[test]
    fn applying_rules_matches_exactly_and_one_shot_removes_then_stops() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("App:*:*", "focus=off", false));
        rules.add_rule(rule("App:main:*", "state=floating", true));
        rules.add_rule(rule("App:*:*", "border=off", false));
        let mut consequence = RuleConsequence::default();
        consequence.set_window_properties(&WindowProperties::new("App", "main", "title"));

        rules.apply_rules(&mut consequence);
        assert!(!consequence.focus);
        assert_eq!(consequence.state, Some(ClientState::Floating));
        assert!(consequence.border);
        assert_eq!(rules.len(), 2);

        rules.apply_rules(&mut consequence);
        assert!(!consequence.border);
    }

    #[test]
    fn later_rules_override_matching_wildcard_defaults() {
        let mut rules = RuleList::default();
        rules.add_rule(rule("*", "state=floating focus=off", false));
        rules.add_rule(rule("App:main", "state=tiled focus=on", false));
        let mut consequence = RuleConsequence::default();
        consequence.set_window_properties(&WindowProperties::new("App", "main", "title"));

        rules.apply_rules(&mut consequence);

        assert_eq!(consequence.state, Some(ClientState::Tiled));
        assert!(consequence.focus);
    }

    #[test]
    fn consequence_format_matches_external_rule_protocol() {
        let mut consequence = RuleConsequence::default();
        parse_keys_values(
            "monitor=one desktop=two node=three state=pseudo_tiled layer=below honor_size_hints=on split_dir=south split_ratio=0.5 hidden=on rectangle=100x80+-1+2",
            &mut consequence,
        );
        assert_eq!(
            print_rule_consequence(&consequence),
            "monitor=one desktop=two node=three state=pseudo_tiled layer=below honor_size_hints=true split_dir=south split_ratio=0.500000 hidden=on sticky=off private=off locked=off marked=off center=off follow=off manage=on focus=on border=on rectangle=100x80+-1+2"
        );
        assert_eq!(
            consequence.to_string(),
            print_rule_consequence(&consequence)
        );
    }

    #[test]
    fn built_in_rules_follow_upstream_order() {
        let mut consequence = RuleConsequence::default();
        apply_builtin_rules(
            &BuiltinRuleProperties {
                window_types: vec![BuiltinWindowType::Utility, BuiltinWindowType::Dialog],
                window_states: vec![
                    BuiltinWindowState::Fullscreen,
                    BuiltinWindowState::Below,
                    BuiltinWindowState::Sticky,
                ],
                transient: true,
                ..BuiltinRuleProperties::default()
            },
            true,
            &mut consequence,
        );
        assert!(!consequence.focus);
        assert_eq!(consequence.state, Some(ClientState::Floating));
        assert_eq!(consequence.layer, Some(StackLayer::Below));
        assert!(consequence.center && consequence.sticky);
    }

    #[test]
    fn dialog_layer_setting_precedes_user_rule_effects() {
        let properties = BuiltinRuleProperties {
            window_types: vec![BuiltinWindowType::Dialog],
            ..BuiltinRuleProperties::default()
        };

        let mut disabled = RuleConsequence::default();
        apply_builtin_rules(&properties, false, &mut disabled);
        assert_eq!(disabled.state, Some(ClientState::Floating));
        assert_eq!(disabled.layer, None);

        let mut enabled = RuleConsequence::default();
        apply_builtin_rules(&properties, true, &mut enabled);
        assert_eq!(enabled.layer, Some(StackLayer::Above));
        parse_keys_values("layer=below", &mut enabled);
        assert_eq!(enabled.layer, Some(StackLayer::Below));
    }
}
