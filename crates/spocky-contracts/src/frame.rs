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
use crate::zod::Outcome;
use crate::zod_schemas::check_inbound;

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

/// Why the daemon rejects an inbound frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundRejection {
    /// `JSON.parse` throws.
    Syntax(JsonSyntaxError),
    /// `WSInboundMessageSchema.safeParse` fails with this `error.message`.
    Invalid(String),
    /// A session request type without a Rust model.
    Unmodeled,
    /// The Rust model rejects a frame zod accepts, or one nested too deep
    /// for zod to judge (DIV-001); the text is the model's error.
    Divergent(String),
}

impl InboundRejection {
    /// `Invalid message: ${error.message}`, the error text the daemon sends
    /// for a zod rejection that is not an unknown session request.
    #[must_use]
    pub fn invalid_message(&self) -> Option<String> {
        match self {
            Self::Invalid(message) => Some(format!("Invalid message: {message}")),
            Self::Syntax(_) | Self::Unmodeled | Self::Divergent(_) => None,
        }
    }
}

impl Display for InboundRejection {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(formatter, "invalid JSON: {error}"),
            Self::Invalid(message) => write!(formatter, "Invalid message: {message}"),
            Self::Unmodeled => formatter.write_str("session request type without a Rust model"),
            Self::Divergent(message) => write!(formatter, "Rust model rejects frame: {message}"),
        }
    }
}

impl std::error::Error for InboundRejection {}

/// Parses an inbound frame as the daemon does: `JSON.parse`, then
/// `WSInboundMessageSchema.safeParse`, whose rejection text the zod port
/// reproduces, then the Rust model.
///
/// # Errors
///
/// Returns an [`InboundRejection`] for each way the frame can fail.
pub fn parse_inbound(text: &str) -> Result<WsInbound, InboundRejection> {
    let value = parse(text).map_err(InboundRejection::Syntax)?;
    match check_inbound(&value) {
        Outcome::Invalid(message) => return Err(InboundRejection::Invalid(message)),
        Outcome::Unmodeled => return Err(InboundRejection::Unmodeled),
        Outcome::Valid | Outcome::TooDeep => {}
    }
    WsInbound::deserialize(JsValueDeserializer(&value))
        .map_err(|error| InboundRejection::Divergent(error.to_string()))
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

#[cfg(test)]
mod tests {
    use super::{InboundRejection, WsInbound, parse_frame, parse_inbound};
    use crate::js_value::stringify;
    use crate::session::SessionInbound;
    use crate::ws::WsControlInbound;

    const DEPTH: usize = 100_000;

    fn nested() -> String {
        format!("{}1{}", "[".repeat(DEPTH), "]".repeat(DEPTH))
    }

    // JSON.parse accepts this depth and zod's z.unknown() passthrough keeps
    // the value (node v22.20.0); serde visitors would overflow the stack.
    #[test]
    fn deep_passthrough_value_parses_like_json_parse() {
        let deep = nested();
        let text = format!(
            r#"{{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1,"capabilities":{{"x":{deep},"voice":true}}}}"#
        );
        let Ok(WsInbound::Control(WsControlInbound::Hello(hello))) = parse_frame(&text) else {
            panic!("deep hello must parse");
        };
        let capabilities = hello.capabilities.expect("capabilities");
        assert_eq!(capabilities.flags.voice, Some(true));
        let extra = capabilities.extra.get("x").expect("extra kept");
        assert_eq!(stringify(extra.as_value()), deep);
    }

    // zod strips unknown keys without reading them, at any depth.
    #[test]
    fn deep_unknown_keys_are_skipped() {
        let deep = nested();
        let text = format!(
            r#"{{"type":"session","message":{{"type":"send_agent_message_request","junk":{deep},"requestId":"r","agentId":"a","text":"t","attachments":[{{"type":"uploaded_file","id":"f","fileName":"n","mimeType":"m","size":1,"path":"/p","junk":{deep}}}]}}}}"#
        );
        let Ok(WsInbound::Session(message)) = parse_frame::<WsInbound>(&text) else {
            panic!("deep unknown keys must parse");
        };
        let SessionInbound::SendAgentMessage(request) = *message else {
            panic!("send_agent_message_request");
        };
        assert_eq!(request.attachments.expect("attachments").0.len(), 1);
    }

    fn provider_options(depth: usize, leaf: &str) -> String {
        format!(
            r#"{{"type":"session","message":{{"type":"agent.create.request","requestId":"r","config":{{"provider":"codex","cwd":"/c","providerOptions":{{"n":{}{leaf}{}}}}}}}}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        )
    }

    // z.json() recursion: pinned zod judges 990 levels; deeper, the port
    // stops before the Rust stack does and the model decides (DIV-001). A
    // rejection is judged on the deep-stack thread too; its exact text is a
    // golden case, since at 990 levels zod's indentation makes it 151 MB.
    #[test]
    fn deep_json_options_are_judged_without_overflow() {
        assert!(parse_inbound(&provider_options(990, "1")).is_ok());
        assert!(matches!(
            parse_inbound(&provider_options(30, "1e400")),
            Err(InboundRejection::Invalid(_))
        ));
        assert!(parse_inbound(&provider_options(DEPTH, "1")).is_ok());
    }

    #[test]
    fn deep_record_values_parse_inside_tagged_unions() {
        let deep = nested();
        let text = format!(
            r#"{{"type":"session","message":{{"type":"agent.create.request","requestId":"r","config":{{"provider":"codex","cwd":"/c","featureValues":{{"f":{deep}}}}}}}}}"#
        );
        let Ok(WsInbound::Session(message)) = parse_frame::<WsInbound>(&text) else {
            panic!("deep z.unknown() record value must parse");
        };
        let SessionInbound::AgentCreate(request) = *message else {
            panic!("agent.create.request");
        };
        let values = request.config.feature_values.expect("featureValues");
        assert_eq!(stringify(values.get("f").expect("f").as_value()), deep);
    }

    #[test]
    fn lone_surrogates_and_overflowing_numbers_parse_like_json_parse() {
        let text = r#"{"type":"hello","clientId":"\ud800","clientType":"cli","protocolVersion":1,"capabilities":{"n":1e400,"s":"\udfff"}}"#;
        let Ok(WsInbound::Control(WsControlInbound::Hello(hello))) = parse_frame(text) else {
            panic!("hello must parse");
        };
        let extra = hello.capabilities.expect("capabilities").extra;
        assert_eq!(
            extra.get("n").and_then(|value| value.as_value().as_f64()),
            Some(f64::INFINITY)
        );
        assert_eq!(
            stringify(extra.get("s").expect("s").as_value()),
            r#""\udfff""#
        );
        assert!(
            parse_frame::<WsInbound>(
                r#"{"type":"hello","clientId":"\ud8","clientType":"cli","protocolVersion":1}"#
            )
            .is_err()
        );
    }
}
