//! Timeline item size limits from pinned Paseo
//! `agent/agent-timeline-content.ts`: tool-call shell output, failed shell
//! error content, and plain-text detail text are cut to 64 Ki UTF-16 units.

use spocky_store::js_value::{JsObject, JsValue, stringify};

use crate::agent_sdk::AgentError;

use crate::text::slice_utf16;
use crate::timeline::JsTypeError;
use spocky_contracts::js::spread;
use spocky_contracts::text::js_length;

/// `TOOL_CALL_CONTENT_MAX_LENGTH`.
pub const TOOL_CALL_CONTENT_MAX_LENGTH: usize = 64 * 1024;

/// `PLUGIN_TIMELINE_DATA_MAX_BYTES`.
pub const PLUGIN_TIMELINE_DATA_MAX_BYTES: usize = 64 * 1024;

/// `assertPluginTimelineDataSize`: the UTF-8 length of `JSON.stringify(data)`
/// may not exceed 64 KiB.
///
/// # Errors
///
/// Returns the baseline's `Error` for oversized data, and Node's `TypeError`
/// from `Buffer.byteLength` when `data` is `undefined` (`JSON.stringify`
/// returns no string).
pub fn assert_plugin_timeline_data_size(data: &JsValue) -> Result<(), AgentError> {
    if matches!(data, JsValue::Undefined) {
        return Err(AgentError::named(
            "TypeError".to_owned(),
            "The \"string\" argument must be of type string or an instance of \
                      Buffer or ArrayBuffer. Received undefined"
                .to_owned(),
        ));
    }
    if stringify(data).len() > PLUGIN_TIMELINE_DATA_MAX_BYTES {
        return Err(AgentError::new(format!(
            "Plugin timeline item data exceeds {PLUGIN_TIMELINE_DATA_MAX_BYTES} bytes"
        )));
    }
    Ok(())
}

/// `item.detail.type`: reading `type` of a missing or `null` detail throws.
fn detail_type(item: &JsValue) -> Result<Option<&str>, JsTypeError> {
    match item.get("detail") {
        None | Some(JsValue::Undefined) => Err(JsTypeError(
            "Cannot read properties of undefined (reading 'type')".to_owned(),
        )),
        Some(JsValue::Null) => Err(JsTypeError(
            "Cannot read properties of null (reading 'type')".to_owned(),
        )),
        Some(detail) => Ok(detail.get("type").and_then(JsValue::as_str)),
    }
}

fn is_tool_call(item: &JsValue) -> bool {
    item.get("type").and_then(JsValue::as_str) == Some("tool_call")
}

/// The string at `object[key]` when it is longer than the limit.
fn oversized<'a>(object: Option<&'a JsValue>, key: &str) -> Option<&'a str> {
    object
        .and_then(|object| object.get(key))
        .and_then(JsValue::as_str)
        .filter(|text| js_length(text) > TOOL_CALL_CONTENT_MAX_LENGTH)
}

/// `{ ...item, [outer]: { ...item[outer], [inner]: text.slice(0, max) } }`.
fn with_cut(item: &JsValue, outer: &str, inner: &str, text: &str) -> JsValue {
    let mut nested = spread(item.get(outer));
    nested.insert(
        inner,
        JsValue::String(slice_utf16(text, TOOL_CALL_CONTENT_MAX_LENGTH)),
    );
    let mut copy: JsObject = spread(Some(item));
    copy.insert(outer, JsValue::Object(nested));
    JsValue::Object(copy)
}

/// `limitFailedShellError`.
fn limit_failed_shell_error(item: JsValue) -> Result<JsValue, JsTypeError> {
    if !is_tool_call(&item)
        || detail_type(&item)? != Some("shell")
        || item.get("status").and_then(JsValue::as_str) != Some("failed")
    {
        return Ok(item);
    }
    // `typeof error === "object"` and `"content" in error`; an array has no
    // `content` property.
    let error = item.get("error").filter(|error| error.is_object());
    Ok(match oversized(error, "content") {
        Some(content) => with_cut(&item, "error", "content", content),
        None => item,
    })
}

/// `limitPlainText`.
fn limit_plain_text(item: JsValue) -> Result<JsValue, JsTypeError> {
    if !is_tool_call(&item) || detail_type(&item)? != Some("plain_text") {
        return Ok(item);
    }
    Ok(match oversized(item.get("detail"), "text") {
        Some(text) => with_cut(&item, "detail", "text", text),
        None => item,
    })
}

/// `limitAgentTimelineItemContent`.
///
/// # Errors
///
/// Returns the baseline's [`JsTypeError`] for a `tool_call` item whose
/// `detail` is missing or `null`.
pub fn limit_agent_timeline_item_content(item: JsValue) -> Result<JsValue, JsTypeError> {
    let item = limit_plain_text(limit_failed_shell_error(item)?)?;
    if !is_tool_call(&item) || detail_type(&item)? != Some("shell") {
        return Ok(item);
    }
    Ok(match oversized(item.get("detail"), "output") {
        Some(output) => with_cut(&item, "detail", "output", output),
        None => item,
    })
}
