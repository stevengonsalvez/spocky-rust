//! Control messages from the relay (`tryParseControlMessage`) and the conversion of raw
//! `ws` message data the relay transport adapter applies (`normalizeMessageData`).

use crate::js_json::{self, Value};
use spocky_crypto::channel::Data;
use spocky_crypto::js_string::{JsString, trim};

/// The data a `ws` `message` event carries: a `Buffer`, an `ArrayBuffer`, an array of
/// `Buffer` fragments, or (for fakes) a string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageData {
    Buffer(Vec<u8>),
    Text(String),
    ArrayBuffer(Vec<u8>),
    Fragments(Vec<Vec<u8>>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlMessage {
    Sync { connection_ids: Vec<JsString> },
    Connected { connection_id: JsString },
    Disconnected { connection_id: JsString },
    Ping,
    Pong,
}

/// `String(raw)` the way `tryParseControlMessage` reaches it.
fn control_text(raw: &MessageData) -> String {
    match raw {
        MessageData::Buffer(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        MessageData::Text(text) => text.clone(),
        MessageData::ArrayBuffer(_) => "[object ArrayBuffer]".to_owned(),
        // `String([buffer, ...])` joins the elements' `toString()` with commas.
        MessageData::Fragments(parts) => parts
            .iter()
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
            .join(","),
    }
}

fn string_member<'a>(value: &'a Value, key: &str) -> Option<&'a JsString> {
    match value.get(key) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

/// `tryParseControlMessage`: `None` is the original's `null`.
#[must_use]
pub fn try_parse_control_message(raw: &MessageData) -> Option<ControlMessage> {
    let parsed = js_json::parse(&control_text(raw))?;
    if !parsed.is_record() {
        return None;
    }
    let kind = string_member(&parsed, "type")?;
    let kind = String::from_utf16_lossy(kind);
    // A lone surrogate never equals one of the ASCII type names, so the lossy conversion
    // cannot create a false match.
    match kind.as_str() {
        "ping" => Some(ControlMessage::Ping),
        "pong" => Some(ControlMessage::Pong),
        "sync" => {
            let Some(Value::Array(elements)) = parsed.get("connectionIds") else {
                return None;
            };
            let connection_ids = elements
                .iter()
                .filter_map(|element| match element {
                    Value::String(id) if !trim(id).is_empty() => Some(id.clone()),
                    _ => None,
                })
                .collect();
            Some(ControlMessage::Sync { connection_ids })
        }
        "connected" | "disconnected" => {
            let id = string_member(&parsed, "connectionId")?;
            let trimmed = trim(id);
            if trimmed.is_empty() {
                return None;
            }
            let connection_id = trimmed.to_vec();
            Some(if kind == "connected" {
                ControlMessage::Connected { connection_id }
            } else {
                ControlMessage::Disconnected { connection_id }
            })
        }
        _ => None,
    }
}

/// `normalizeMessageData`: what the end-to-end channel receives for a physical frame.
#[must_use]
pub fn normalize_message_data(data: &MessageData, is_binary: bool) -> Data {
    if !is_binary {
        return match data {
            MessageData::Text(text) => Data::Text(text.clone()),
            other => Data::Text(String::from_utf8_lossy(&concatenated(other)).into_owned()),
        };
    }
    match data {
        MessageData::Text(text) => Data::Text(text.clone()),
        other => Data::Binary(concatenated(other)),
    }
}

fn concatenated(data: &MessageData) -> Vec<u8> {
    match data {
        MessageData::Buffer(bytes) | MessageData::ArrayBuffer(bytes) => bytes.clone(),
        MessageData::Fragments(parts) => parts.concat(),
        MessageData::Text(text) => text.as_bytes().to_vec(),
    }
}
