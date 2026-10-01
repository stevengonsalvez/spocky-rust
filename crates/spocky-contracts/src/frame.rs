//! Whole WebSocket text frames: `WSInboundMessageSchema` and
//! `WSOutboundMessageSchema`, dispatched on the top-level `type`.
//!
//! Text is parsed as `JSON.parse` does before typing: a repeated key keeps
//! its first position and its last value.

use serde::de::{self, Deserializer};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::{SessionInbound, SessionOutbound};
use crate::ws::{WsControlInbound, WsControlOutbound};

/// A frame a client sends.
#[derive(Debug, Clone, PartialEq)]
pub enum WsInbound {
    Control(WsControlInbound),
    Session(Box<SessionInbound>),
}

/// A frame the daemon sends.
#[derive(Debug, Clone, PartialEq)]
pub enum WsOutbound {
    Control(WsControlOutbound),
    Session(Box<SessionOutbound>),
}

#[derive(Deserialize)]
struct SessionEnvelope<T> {
    message: T,
}

fn serialize_session<S: Serializer, T: Serialize>(
    message: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut envelope = serializer.serialize_struct("SessionEnvelope", 2)?;
    envelope.serialize_field("type", "session")?;
    envelope.serialize_field("message", message)?;
    envelope.end()
}

fn frame_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

impl Serialize for WsInbound {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Control(control) => control.serialize(serializer),
            Self::Session(message) => serialize_session(message, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WsInbound {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if frame_type(&value) == Some("session") {
            SessionEnvelope::<SessionInbound>::deserialize(value)
                .map(|envelope| Self::Session(Box::new(envelope.message)))
                .map_err(de::Error::custom)
        } else {
            WsControlInbound::deserialize(value)
                .map(Self::Control)
                .map_err(de::Error::custom)
        }
    }
}

impl Serialize for WsOutbound {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Control(control) => control.serialize(serializer),
            Self::Session(message) => serialize_session(message, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WsOutbound {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if frame_type(&value) == Some("session") {
            SessionEnvelope::<SessionOutbound>::deserialize(value)
                .map(|envelope| Self::Session(Box::new(envelope.message)))
                .map_err(de::Error::custom)
        } else {
            WsControlOutbound::deserialize(value)
                .map(Self::Control)
                .map_err(de::Error::custom)
        }
    }
}

/// Parses frame text into `T` with `JSON.parse` key semantics.
///
/// # Errors
///
/// Returns an error for invalid JSON or a frame `T` does not accept.
pub fn parse_frame<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str::<Value>(text).and_then(serde_json::from_value)
}

/// Writes a frame exactly as `JSON.stringify` writes the equivalent object.
///
/// # Errors
///
/// Returns an error only for a non-finite number.
pub fn frame_text<T: Serialize>(frame: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(frame)
}
