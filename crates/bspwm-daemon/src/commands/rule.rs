use std::io;

use super::{ArgCursor, CommandHandler, CommandParseError, Response, fail, parse_index, text};
use crate::rule::Rule;

struct AddRuleArguments<'a> {
    cause: &'a str,
    effect: Vec<&'a [u8]>,
}

fn parse_add_rule_arguments<'a>(
    cursor: &mut ArgCursor<'_, 'a>,
    command: &'a [u8],
) -> Result<AddRuleArguments<'a>, CommandParseError<'a>> {
    let cause = cursor.required(command)?;
    let Some(cause) = text(cause) else {
        return Err(CommandParseError::NotEnoughArguments { command });
    };
    if cursor.is_empty() {
        return Err(CommandParseError::NotEnoughArguments { command });
    }
    Ok(AddRuleArguments {
        cause,
        effect: cursor.take_remaining(),
    })
}

command_set! {
    domain: b"rule";
    enum RuleCommand<'a> {
        Add {
            arguments: AddRuleArguments<'a> = custom(parse_add_rule_arguments),
        } => [b"-a", b"--add"],
        Remove {
            selectors: Vec<&'a [u8]> = rest1,
        } => [b"-r", b"--remove"],
        List => [b"-l", b"--list"],
    }
}

impl CommandHandler<'_> {
    pub(super) fn handle_rule(&mut self, args: &[&[u8]], rsp: &mut dyn Response) -> io::Result<()> {
        if args.is_empty() {
            return CommandParseError::MissingCommands.respond(rsp, b"rule");
        }
        let mut cursor = ArgCursor::new(args);
        while let Some(command) = RuleCommand::next(&mut cursor, rsp)? {
            match command {
                RuleCommand::Add { arguments } => {
                    let AddRuleArguments {
                        cause,
                        effect: arguments,
                    } = arguments;
                    let mut effect = Vec::new();
                    let mut one_shot = false;
                    for argument in arguments {
                        if matches!(argument, b"-o" | b"--one-shot") {
                            one_shot = true;
                        } else if let Some(value) = text(argument) {
                            effect.push(value);
                        } else {
                            return fail(rsp, b"");
                        }
                    }
                    self.state
                        .rules
                        .add_rule(Rule::from_cause(cause, effect.join(" "), one_shot));
                    break;
                }
                RuleCommand::Remove { selectors } => {
                    for argument in selectors {
                        let Some(value) = text(argument) else {
                            continue;
                        };
                        if value == "head" {
                            self.state.rules.remove_rule(0);
                        } else if value == "tail" {
                            self.state
                                .rules
                                .len()
                                .checked_sub(1)
                                .and_then(|last| self.state.rules.remove_rule(last));
                        } else if let Some(rule_index) = parse_index(value) {
                            self.state
                                .rules
                                .remove_rule_by_index(usize::from(rule_index.wrapping_sub(1)));
                        } else {
                            self.state.rules.remove_rule_by_cause(value);
                        }
                    }
                    break;
                }
                RuleCommand::List => {
                    rsp.write_all(self.state.rules.list_rules().as_bytes())?;
                }
            }
        }
        Ok(())
    }
}
