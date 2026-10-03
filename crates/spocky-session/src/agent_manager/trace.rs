//! The bindings of the manager's `logger.trace` calls.
//!
//! Each call site lists its keys in the pinned order. A value that is
//! `undefined` in the baseline stays in the object as [`JsValue::Undefined`]:
//! it keeps its key slot and `JSON.stringify` leaves it out.

use spocky_store::js_value::{JsObject, JsValue};

use super::ManagedAgentSnapshot;

/// An object with the keys in the order given.
pub(super) fn bindings<const N: usize>(pairs: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in pairs {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

pub(super) fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `agent.persistence?.sessionId ?? undefined`.
pub(super) fn session_id(agent: &ManagedAgentSnapshot) -> JsValue {
    agent
        .persistence
        .as_ref()
        .and_then(|handle| handle.get("sessionId"))
        .filter(|id| !matches!(id, JsValue::Null))
        .cloned()
        .unwrap_or(JsValue::Undefined)
}

/// `agent.activeForegroundTurnId`.
pub(super) fn foreground_turn_id(agent: &ManagedAgentSnapshot) -> JsValue {
    agent
        .active_foreground_turn_id
        .as_deref()
        .map_or(JsValue::Null, text)
}

/// `agent.activeForegroundTurnId ?? undefined`.
pub(super) fn foreground_turn_id_or_undefined(agent: &ManagedAgentSnapshot) -> JsValue {
    agent
        .active_foreground_turn_id
        .as_deref()
        .map_or(JsValue::Undefined, text)
}

/// `agent.lifecycle`.
pub(super) fn lifecycle(agent: &ManagedAgentSnapshot) -> JsValue {
    text(agent.lifecycle.as_str())
}

/// A count, such as `agent.pendingPermissions.size`.
pub(super) fn count(value: usize) -> JsValue {
    #[allow(clippy::cast_precision_loss, reason = "a count is a double in node")]
    JsValue::Number(value as f64)
}
