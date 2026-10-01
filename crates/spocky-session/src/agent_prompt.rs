//! Prompt helpers from pinned Paseo `agent/agent-prompt.ts`
//! (`isSystemInjectedEnvelope`) and `agent/agent-manager.ts`
//! (`submittedPromptText`).

use spocky_store::js_value::JsValue;

use crate::agent_sdk::AgentPromptInput;
use crate::js::js_string;
use crate::text::js_trim;

const ENVELOPE_OPEN: &str = "<paseo-system>\n";
const ENVELOPE_CLOSE: &str = "\n</paseo-system>";

/// `isSystemInjectedEnvelope`:
/// `/^<paseo-system>\n[\s\S]*\n<\/paseo-system>$/`.
#[must_use]
pub fn is_system_injected_envelope(text: &str) -> bool {
    text.len() >= ENVELOPE_OPEN.len() + ENVELOPE_CLOSE.len()
        && text.starts_with(ENVELOPE_OPEN)
        && text.ends_with(ENVELOPE_CLOSE)
}

/// `submittedPromptText`: the prompt string, or the text blocks without a
/// `mimeType` joined by newlines and trimmed.
#[must_use]
pub fn submitted_prompt_text(prompt: &AgentPromptInput) -> String {
    match prompt {
        AgentPromptInput::Text(text) => text.clone(),
        AgentPromptInput::Blocks(blocks) => {
            let texts: Vec<String> = blocks
                .iter()
                .filter(|block| {
                    block.get("type").and_then(JsValue::as_str) == Some("text")
                        && block.get("mimeType").is_none()
                })
                .map(|block| match block.get("text") {
                    // `Array.prototype.join` writes undefined and null as "".
                    None | Some(JsValue::Undefined | JsValue::Null) => String::new(),
                    text => js_string(text),
                })
                .collect();
            js_trim(&texts.join("\n")).to_owned()
        }
    }
}
