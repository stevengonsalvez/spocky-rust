//! Whole WebSocket text frames: `WSInboundMessageSchema` and
//! `WSOutboundMessageSchema`, dispatched on the top-level `type`.
//!
//! Text is parsed by [`crate::js_value::parse`], which accepts exactly what
//! `JSON.parse` accepts: a repeated key keeps its first position and its last
//! value, lone surrogates survive, and overflowing numbers become infinities.
//! Outbound frames are emit-only.

use std::fmt::{self, Display, Formatter};

use serde::de::{self, Deserializer};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

use crate::js_value::{JsValue, JsonSyntaxError, parse};
use crate::json::{JsValueDeserializer, JsonValue, js_wire_text};

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

fn frame_type(value: &JsValue) -> Option<&str> {
    value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(JsValue::as_str)
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
        let JsonValue(value) = JsonValue::deserialize(deserializer)?;
        let typed = JsValueDeserializer(&value);
        if frame_type(&value) == Some("session") {
            SessionEnvelope::<SessionInbound>::deserialize(typed)
                .map(|envelope| Self::Session(Box::new(envelope.message)))
                .map_err(de::Error::custom)
        } else {
            WsControlInbound::deserialize(typed)
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

/// Why a frame failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// `JSON.parse` would throw this `SyntaxError`.
    Syntax(JsonSyntaxError),
    /// The JSON is valid but the frame does not match its schema.
    Invalid(String),
}

impl Display for FrameError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(formatter, "invalid JSON: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid frame: {message}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Parses frame text into `T` with `JSON.parse` semantics. This is the entry
/// point for inbound frames: derived `Deserialize` impls fed by other
/// deserializers reject a repeated key, which `JSON.parse` and zod accept.
///
/// # Errors
///
/// Returns [`FrameError::Syntax`] where `JSON.parse` throws and
/// [`FrameError::Invalid`] for a frame `T` does not accept.
pub fn parse_frame<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, FrameError> {
    let value = parse(text).map_err(FrameError::Syntax)?;
    T::deserialize(JsValueDeserializer(&value))
        .map_err(|error| FrameError::Invalid(error.to_string()))
}

/// Writes a frame exactly as `JSON.stringify` writes the equivalent object,
/// including `\udXXX` escapes for lone surrogates.
///
/// # Errors
///
/// Returns an error only for a non-finite number in a typed number field.
pub fn frame_text<T: Serialize>(frame: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(frame).map(|text| js_wire_text(&text))
}
