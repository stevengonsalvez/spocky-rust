//! Agent permission requests as the pinned daemon emits them (emit-only).
//!
//! Fields follow `AgentPermissionRequestPayloadSchema` order, which is also
//! the construction order of the Codex shell approval request
//! (`codex-app-server-agent.ts:6895-6944`): `id, provider, name, kind,
//! title, description?, input?, detail, metadata`. The daemon does not zod
//! parse it before sending.
//!
//! `sanitizePendingPermissions` (`agent-projections.ts:331-342`) keeps that
//! order: it reassigns `input`, `suggestions`, `actions`, and `metadata` in
//! place and drops any that sanitize to `undefined`, such as an `input` whose
//! members are all undefined. The raw `agent_permission_request` event keeps
//! such an `input` as `{}`.

use serde::Serialize;

use crate::field::optional;
use crate::json::{JsRecord, JsonValue};
use crate::text::JsText;
use crate::timeline::ToolCallDetail;

/// `AgentPermissionRequestPayloadSchema.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionKind {
    Tool,
    Plan,
    Question,
    Mode,
    Other,
}

/// `AgentPermissionActionSchema.behavior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionBehavior {
    Allow,
    Deny,
}

/// `AgentPermissionActionSchema.variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionActionVariant {
    Primary,
    Secondary,
    Danger,
}

/// `AgentPermissionActionSchema.intent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionActionIntent {
    Implement,
    ImplementResume,
    Dismiss,
}

/// `AgentPermissionActionSchema`, copied with `Object.assign({}, action)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionAction {
    pub id: JsText,
    pub label: JsText,
    pub behavior: PermissionBehavior,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub variant: Option<PermissionActionVariant>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub intent: Option<PermissionActionIntent>,
}

/// An agent permission request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PermissionRequest {
    pub id: JsText,
    pub provider: JsText,
    pub name: JsText,
    pub kind: PermissionKind,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub title: Option<JsText>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub description: Option<JsText>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub input: Option<JsRecord<JsonValue>>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub detail: Option<ToolCallDetail>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub suggestions: Option<Vec<JsRecord<JsonValue>>>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub actions: Option<Vec<PermissionAction>>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub metadata: Option<JsRecord<JsonValue>>,
}

#[cfg(test)]
mod tests {
    use super::{PermissionKind, PermissionRequest};
    use crate::frame::frame_text;
    use crate::js_value::JsValue;
    use crate::json::{JsRecord, JsonValue};
    use crate::text::JsText;
    use crate::timeline::{ShellDetail, ToolCallDetail};

    #[test]
    fn codex_shell_request_keeps_construction_order() {
        let text = |value: &str| JsonValue(JsValue::String(value.to_owned()));
        let metadata: JsRecord<JsonValue> = [
            ("itemId", text("item-1")),
            ("threadId", text("thr-1")),
            ("turnId", text("turn-1")),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
        let request = PermissionRequest {
            id: JsText::new("permission-item-1"),
            provider: JsText::new("codex"),
            name: JsText::new("CodexBash"),
            kind: PermissionKind::Tool,
            title: Some(JsText::new("Run command: ls")),
            description: None,
            input: None,
            detail: Some(ToolCallDetail::Shell(ShellDetail {
                command: JsText::new("ls"),
                cwd: Some(JsText::new("/tmp/project")),
                output: None,
                exit_code: None,
            })),
            suggestions: None,
            actions: None,
            metadata: Some(metadata),
        };
        assert_eq!(
            frame_text(&request).unwrap(),
            concat!(
                r#"{"id":"permission-item-1","provider":"codex","name":"CodexBash","kind":"tool","#,
                r#""title":"Run command: ls","detail":{"type":"shell","command":"ls","cwd":"/tmp/project"},"#,
                r#""metadata":{"itemId":"item-1","threadId":"thr-1","turnId":"turn-1"}}"#
            )
        );
    }
}
