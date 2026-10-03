//! v2 daemon control socket messages: the one inbound ping and the outbound notifications,
//! byte-for-byte as Jason writes them. Small maps iterate in atom-table order, which puts
//! `type` (an OTP atom) before the relay's own keys; strings use `escape: :json`.

use crate::erlang_map;
use crate::json::{Field, scan};
use std::fmt::Write as _;

/// `Jason.encode!` raised: an identifier was not valid UTF-8.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidUtf8;

/// Whether an inbound control text frame is the legacy ping,
/// `{:ok, %{"type" => "ping"}} <- Jason.decode(payload)`. Anything else is ignored.
#[must_use]
pub fn is_ping(payload: &[u8]) -> bool {
    scan(payload).is_some_and(|fields| fields.type_field == Field::Text("ping".to_owned()))
}

/// `{"type":"sync","connectionIds":[...]}` for the Owner's client map; the ids are listed
/// in the order `Map.keys/1` returns them.
///
/// # Errors
///
/// Fails when an identifier is not valid UTF-8.
pub fn sync(connection_ids: &[&[u8]]) -> Result<String, InvalidUtf8> {
    let mut ids = Vec::with_capacity(connection_ids.len());
    for id in erlang_map::keys(connection_ids) {
        ids.push(encode_string(&id)?);
    }
    Ok(format!(
        r#"{{"type":"sync","connectionIds":[{}]}}"#,
        ids.join(",")
    ))
}

/// `{"type":"connected","connectionId":"..."}`.
///
/// # Errors
///
/// Fails when the identifier is not valid UTF-8.
pub fn connected(connection_id: &[u8]) -> Result<String, InvalidUtf8> {
    Ok(format!(
        r#"{{"type":"connected","connectionId":{}}}"#,
        encode_string(connection_id)?
    ))
}

/// `{"type":"disconnected","connectionId":"..."}`.
///
/// # Errors
///
/// Fails when the identifier is not valid UTF-8.
pub fn disconnected(connection_id: &[u8]) -> Result<String, InvalidUtf8> {
    Ok(format!(
        r#"{{"type":"disconnected","connectionId":{}}}"#,
        encode_string(connection_id)?
    ))
}

/// `{"type":"pong","ts":<milliseconds>}`.
#[must_use]
pub fn pong(unix_milliseconds: i64) -> String {
    format!(r#"{{"type":"pong","ts":{unix_milliseconds}}}"#)
}

fn encode_string(value: &[u8]) -> Result<String, InvalidUtf8> {
    let text = std::str::from_utf8(value).map_err(|_| InvalidUtf8)?;
    let mut encoded = String::with_capacity(text.len() + 2);
    encoded.push('"');
    for character in text.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{8}' => encoded.push_str("\\b"),
            '\t' => encoded.push_str("\\t"),
            '\n' => encoded.push_str("\\n"),
            '\u{c}' => encoded.push_str("\\f"),
            '\r' => encoded.push_str("\\r"),
            '\u{0}'..='\u{1f}' => {
                let _ = write!(encoded, "\\u{:04X}", u32::from(character));
            }
            other => encoded.push(other),
        }
    }
    encoded.push('"');
    Ok(encoded)
}
