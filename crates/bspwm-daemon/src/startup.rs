use std::collections::HashMap;

const MAX_MESSAGE_BYTES: usize = 4096;
const MAX_SEQUENCES: usize = 512;

#[derive(Debug, Default)]
pub struct StartupTracker {
    fragments: HashMap<u32, Vec<u8>>,
    timestamps: HashMap<String, u32>,
}

impl StartupTracker {
    pub fn ingest(&mut self, sender: u32, begin: bool, payload: &[u8]) {
        if begin {
            self.fragments.insert(sender, Vec::new());
        }
        let Some(message) = self.fragments.get_mut(&sender) else {
            return;
        };
        let end = payload.iter().position(|byte| *byte == 0);
        let fragment = end.map_or(payload, |index| &payload[..index]);
        if message.len().saturating_add(fragment.len()) > MAX_MESSAGE_BYTES {
            self.fragments.remove(&sender);
            return;
        }
        message.extend_from_slice(fragment);
        if end.is_none() {
            return;
        }
        let message = self.fragments.remove(&sender).unwrap_or_default();
        if let Some((id, timestamp)) = parse_message(&message) {
            if self.timestamps.len() >= MAX_SEQUENCES && !self.timestamps.contains_key(&id) {
                self.timestamps.clear();
            }
            self.timestamps.insert(id, timestamp);
        }
    }

    #[must_use]
    pub fn timestamp(&self, id: &str) -> Option<u32> {
        self.timestamps.get(id).copied()
    }
}

fn parse_message(message: &[u8]) -> Option<(String, u32)> {
    let message = std::str::from_utf8(message).ok()?;
    let (_, fields) = message.split_once(':')?;
    let fields = parse_fields(fields);
    let id = fields.get("ID")?.clone();
    let timestamp = fields
        .get("TIMESTAMP")
        .and_then(|value| value.parse().ok())
        .or_else(|| timestamp_from_id(&id))?;
    Some((id, timestamp))
}

fn parse_fields(mut input: &str) -> HashMap<&str, String> {
    let mut fields = HashMap::new();
    while !input.trim_start().is_empty() {
        input = input.trim_start();
        let Some(equals) = input.find('=') else {
            break;
        };
        let key = &input[..equals];
        input = &input[equals + 1..];
        let (value, rest) = if let Some(quoted) = input.strip_prefix('"') {
            parse_quoted(quoted)
        } else {
            let end = input.find(char::is_whitespace).unwrap_or(input.len());
            (input[..end].to_owned(), &input[end..])
        };
        fields.insert(key, value);
        input = rest;
    }
    fields
}

fn parse_quoted(input: &str) -> (String, &str) {
    let mut value = String::new();
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return (value, &input[index + character.len_utf8()..]);
        } else {
            value.push(character);
        }
    }
    (value, "")
}

fn timestamp_from_id(id: &str) -> Option<u32> {
    id.rsplit_once("_TIME")?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassembles_interleaved_messages_and_parses_timestamps() {
        let mut tracker = StartupTracker::default();
        tracker.ingest(1, true, b"new: ID=first_TIME42\0");
        tracker.ingest(2, true, b"new: ID=\"second id\" TIME");
        tracker.ingest(2, false, b"STAMP=77\0");
        assert_eq!(tracker.timestamp("first_TIME42"), Some(42));
        assert_eq!(tracker.timestamp("second id"), Some(77));
    }

    #[test]
    fn quoted_fields_unescape_backslashes_and_quotes() {
        let fields = parse_fields(r#" ID="an\ id\"value_TIME9" NAME="hello world""#);
        assert_eq!(
            fields.get("ID").map(String::as_str),
            Some("an id\"value_TIME9")
        );
        assert_eq!(fields.get("NAME").map(String::as_str), Some("hello world"));
    }
}
