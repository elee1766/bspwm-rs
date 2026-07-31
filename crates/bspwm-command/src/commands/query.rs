use super::{
    ArgCursor, CommandHandler, Coordinates, Response, fail, handle_failure, io,
    parse_desktop_modifiers, parse_monitor_modifiers, parse_node_modifiers, text, unknown_option,
};
use crate::query::{
    NodeIdFilters, query_desktop, query_desktop_ids, query_monitor, query_monitor_ids, query_node,
    query_node_ids,
};

#[derive(Clone, Copy, PartialEq)]
enum QueryDomain {
    Tree,
    Monitor,
    Desktop,
    Node,
}

enum QueryOption<'a> {
    Tree,
    Monitors(Option<&'a [u8]>),
    Desktops(Option<&'a [u8]>),
    Nodes(Option<&'a [u8]>),
    Names,
    Monitor(Option<&'a [u8]>),
    Desktop(Option<&'a [u8]>),
    Node(Option<&'a [u8]>),
}

impl<'a> QueryOption<'a> {
    fn next(cursor: &mut ArgCursor<'_, 'a>) -> Result<Option<Self>, &'a [u8]> {
        let Some(option) = cursor.next() else {
            return Ok(None);
        };
        let mut optional_argument = || {
            cursor
                .peek()
                .filter(|argument| !argument.starts_with(b"-"))
                .and_then(|_| cursor.next())
        };
        let option = match option {
            b"-T" | b"--tree" => Self::Tree,
            b"-M" | b"--monitors" => Self::Monitors(optional_argument()),
            b"-D" | b"--desktops" => Self::Desktops(optional_argument()),
            b"-N" | b"--nodes" => Self::Nodes(optional_argument()),
            b"--names" => Self::Names,
            b"-m" | b"--monitor" => Self::Monitor(optional_argument()),
            b"-d" | b"--desktop" => Self::Desktop(optional_argument()),
            b"-n" | b"--node" => Self::Node(optional_argument()),
            _ => return Err(option),
        };
        Ok(Some(option))
    }
}

impl CommandHandler<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn handle_query(
        &mut self,
        args: &[&[u8]],
        rsp: &mut dyn Response,
    ) -> io::Result<()> {
        if args.is_empty() {
            return fail(rsp, b"query: Missing arguments.\n");
        }
        let reference = self.reference();
        let mut monitor_reference = Coordinates {
            desktop: None,
            node: None,
            ..reference
        };
        let mut desktop_reference = Coordinates {
            node: None,
            ..reference
        };
        let mut node_reference = reference;
        let mut target = Coordinates::default();
        let mut monitor_selector = None;
        let mut desktop_selector = None;
        let mut node_selector = None;
        let mut domain = QueryDomain::Tree;
        let mut commands = 0_u8;
        let mut names = false;
        // The -M/-D/-N arms differ only in the domain they select, their
        // diagnostic label, and the reference they rebind.
        macro_rules! domain_command {
            ($argument:expr, $domain:ident, $label:literal, $resolve:ident, $reference:ident, |$loc:ident| $rebind:expr $(,)?) => {{
                domain = QueryDomain::$domain;
                commands += 1;
                if let Some(argument) = $argument {
                    let Some($loc) = Self::selector_failure(
                        self.$resolve(argument, $reference),
                        $label,
                        argument,
                        rsp,
                    )?
                    else {
                        return Ok(());
                    };
                    $reference = $rebind;
                }
            }};
        }

        // The -m/-d/-n arms differ only in their diagnostic label, modifier
        // parser, selector slot, resolver, reference and target assignment.
        macro_rules! constrained_selector {
            (
                $label:literal, $modifiers:expr, $slot:ident,
                $resolve:ident, $reference:expr,
                $argument:expr,
                |$loc:ident| $apply:expr,
                $bare:expr $(,)?
            ) => {
                if let Some(argument) = $argument {
                    if argument.starts_with(b".") {
                        let Some((_, selector)) = text(argument).and_then($modifiers) else {
                            handle_failure(
                                crate::messages::SELECTOR_BAD_MODIFIERS,
                                $label,
                                argument,
                                rsp,
                            )?;
                            return Ok(());
                        };
                        $slot = Some(selector);
                    } else {
                        let Some($loc) = Self::selector_failure(
                            self.$resolve(argument, $reference),
                            $label,
                            argument,
                            rsp,
                        )?
                        else {
                            return Ok(());
                        };
                        $apply;
                    }
                } else {
                    $bare;
                }
            };
        }
        let mut cursor = ArgCursor::new(args);
        loop {
            let option = match QueryOption::next(&mut cursor) {
                Ok(Some(option)) => option,
                Ok(None) => break,
                Err(option) => return unknown_option(rsp, b"query", option),
            };
            match option {
                QueryOption::Tree => {
                    domain = QueryDomain::Tree;
                    commands += 1;
                }
                QueryOption::Monitors(argument) => domain_command!(
                    argument,
                    Monitor,
                    b"query -M",
                    resolve_monitor,
                    monitor_reference,
                    |loc| Coordinates {
                        monitor: loc.monitor,
                        ..Coordinates::default()
                    },
                ),
                QueryOption::Desktops(argument) => domain_command!(
                    argument,
                    Desktop,
                    b"query -D",
                    resolve_desktop,
                    desktop_reference,
                    |loc| Coordinates { node: None, ..loc },
                ),
                QueryOption::Nodes(argument) => {
                    domain_command!(
                        argument,
                        Node,
                        b"query -N",
                        resolve_node,
                        node_reference,
                        |loc| loc
                    );
                }
                QueryOption::Names => names = true,
                QueryOption::Monitor(argument) => constrained_selector!(
                    b"query -m",
                    parse_monitor_modifiers,
                    monitor_selector,
                    resolve_monitor,
                    monitor_reference,
                    argument,
                    |loc| target.monitor = loc.monitor,
                    target = monitor_reference,
                ),
                QueryOption::Desktop(argument) => constrained_selector!(
                    b"query -d",
                    parse_desktop_modifiers,
                    desktop_selector,
                    resolve_desktop,
                    desktop_reference,
                    argument,
                    |loc| {
                        target.monitor = loc.monitor;
                        target.desktop = loc.desktop;
                    },
                    target = desktop_reference,
                ),
                QueryOption::Node(argument) => constrained_selector!(
                    b"query -n",
                    parse_node_modifiers,
                    node_selector,
                    resolve_node,
                    node_reference,
                    argument,
                    |loc| target = loc,
                    if node_reference.node.is_some() {
                        target = node_reference;
                    } else {
                        return fail(rsp, b"");
                    },
                ),
            }
        }
        if commands == 0 {
            return fail(rsp, b"query: No commands given.\n");
        }
        if commands > 1 {
            return fail(rsp, b"query: Multiple commands given.\n");
        }
        if domain == QueryDomain::Tree && target.monitor.is_none() {
            return fail(rsp, b"query -T: No options given.\n");
        }
        if names && matches!(domain, QueryDomain::Node | QueryDomain::Tree) {
            return fail(
                rsp,
                if domain == QueryDomain::Node {
                    b"query -N: --names only applies to -M and -D.\n"
                } else {
                    b"query -T: --names only applies to -M and -D.\n"
                },
            );
        }
        if domain == QueryDomain::Monitor && (desktop_selector.is_some() || node_selector.is_some())
            || domain == QueryDomain::Desktop && node_selector.is_some()
        {
            return fail(
                rsp,
                if domain == QueryDomain::Monitor {
                    b"query -M: Incompatible descriptor-free constraints.\n"
                } else {
                    b"query -D: Incompatible descriptor-free constraints.\n"
                },
            );
        }
        let mut output = String::new();
        let count = match domain {
            QueryDomain::Node => query_node_ids(
                &self.state.world,
                desktop_reference,
                node_reference,
                target,
                NodeIdFilters {
                    monitor: monitor_selector.as_ref(),
                    desktop: desktop_selector.as_ref(),
                    node: node_selector.as_ref(),
                },
                &mut output,
            ),
            QueryDomain::Desktop => query_desktop_ids(
                &self.state.world,
                desktop_reference,
                target,
                monitor_selector.as_ref(),
                desktop_selector.as_ref(),
                names,
                &mut output,
            ),
            QueryDomain::Monitor => query_monitor_ids(
                &self.state.world,
                target,
                monitor_selector.as_ref(),
                names,
                &mut output,
            ),
            QueryDomain::Tree => {
                if let Some(node) = target.node {
                    query_node(&self.state.world, Some(node), &mut output);
                } else if let Some(desktop) = target.desktop {
                    output.push_str(&query_desktop(&self.state.world, desktop));
                } else if let Some(monitor) = target.monitor {
                    output.push_str(&query_monitor(&self.state.world, monitor));
                }
                output.push('\n');
                1
            }
        };
        rsp.write_all(output.as_bytes())?;
        if count == 0 {
            fail(rsp, b"")?;
        }
        Ok(())
    }
}
