//! Workspace label RPC messages: `workspace.label.*` of pinned Paseo
//! `5de45e2` (`packages/protocol/src/messages.ts`, the colour set in
//! `workspace-labels.ts`).
//!
//! Requests mirror zod output: shape key order, unknown keys stripped.
//! Responses and the live update mirror the object construction order in
//! `session.ts` (`handleWorkspaceLabel*`) and `owned-subscriptions`
//! (`withSubscriptionId` appends `subscriptionId` last).
//!
//! The five requests are not in the generated `WSInboundMessageSchema`
//! (`zod_schemas.rs` marks them `Unmodeled`), so [`check_session_message`]
//! judges them with the same zod port, nested as `{ type: "session", message }`
//! so issue paths carry the `message` prefix.

use std::sync::LazyLock;

use serde::{Deserialize, Deserializer, Serialize};

use crate::field::optional;
use crate::js_value::JsValue;
use crate::number::NonNegativeInt;
use crate::text::JsText;
use crate::zod::{NumberCheck, Outcome, Schema, UnknownKeys, check};

/// `WORKSPACE_LABEL_COLORS`, in the protocol's order.
pub const WORKSPACE_LABEL_COLORS: [&str; 10] = [
    "violet", "sky", "emerald", "orange", "pink", "indigo", "teal", "red", "amber", "blue",
];

/// `WorkspaceLabelColorSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceLabelColor {
    Violet,
    Sky,
    Emerald,
    Orange,
    Pink,
    Indigo,
    Teal,
    Red,
    Amber,
    Blue,
}

/// `WorkspaceLabelDefinitionSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelDefinition {
    pub name: JsText,
    pub color: WorkspaceLabelColor,
}

/// `WorkspaceLabelSyncCursorSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelSyncCursor {
    pub generation: JsText,
    #[serde(rename = "afterSeq")]
    pub after_seq: NonNegativeInt,
}

/// The `subscribe` member of a list request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelSubscribe {
    #[serde(
        rename = "subscriptionId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<JsText>,
}

/// `WorkspaceLabelListRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelListRequest {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub subscribe: Option<WorkspaceLabelSubscribe>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub sync: Option<WorkspaceLabelSyncCursor>,
}

/// `WorkspaceLabelAssignmentSetRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelAssignmentSetRequest {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(rename = "workspaceId")]
    pub workspace_id: JsText,
    pub label: WorkspaceLabelDefinition,
    pub assigned: bool,
}

/// `WorkspaceLabelUpdateRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelUpdateRequest {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub name: JsText,
    #[serde(
        rename = "newName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub new_name: Option<JsText>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub color: Option<WorkspaceLabelColor>,
}

/// `WorkspaceLabelDeleteRequestSchema` and
/// `WorkspaceLabelDeleteInspectRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLabelNameRequest {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub name: JsText,
}

/// The five `workspace.label.*.request` messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum WorkspaceLabelInbound {
    #[serde(rename = "workspace.label.list.request")]
    List(WorkspaceLabelListRequest),
    #[serde(rename = "workspace.label.assignment.set.request")]
    AssignmentSet(WorkspaceLabelAssignmentSetRequest),
    #[serde(rename = "workspace.label.update.request")]
    Update(WorkspaceLabelUpdateRequest),
    #[serde(rename = "workspace.label.delete.request")]
    Delete(WorkspaceLabelNameRequest),
    #[serde(rename = "workspace.label.delete.inspect.request")]
    DeleteInspect(WorkspaceLabelNameRequest),
}

impl WorkspaceLabelInbound {
    /// Deserializes the request named `kind`, the `type` the caller already
    /// read; any other name is an unknown variant.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error for a body `kind` does not accept.
    pub fn deserialize_as<'de, D: Deserializer<'de>>(
        kind: &str,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        Ok(match kind {
            "workspace.label.list.request" => {
                Self::List(WorkspaceLabelListRequest::deserialize(deserializer)?)
            }
            "workspace.label.assignment.set.request" => Self::AssignmentSet(
                WorkspaceLabelAssignmentSetRequest::deserialize(deserializer)?,
            ),
            "workspace.label.update.request" => {
                Self::Update(WorkspaceLabelUpdateRequest::deserialize(deserializer)?)
            }
            "workspace.label.delete.request" => {
                Self::Delete(WorkspaceLabelNameRequest::deserialize(deserializer)?)
            }
            "workspace.label.delete.inspect.request" => {
                Self::DeleteInspect(WorkspaceLabelNameRequest::deserialize(deserializer)?)
            }
            other => {
                return Err(serde::de::Error::unknown_variant(
                    other,
                    &[
                        "workspace.label.list.request",
                        "workspace.label.assignment.set.request",
                        "workspace.label.update.request",
                        "workspace.label.delete.request",
                        "workspace.label.delete.inspect.request",
                    ],
                ));
            }
        })
    }

    /// `msg.type`.
    #[must_use]
    pub const fn request_type(&self) -> &'static str {
        match self {
            Self::List(_) => "workspace.label.list.request",
            Self::AssignmentSet(_) => "workspace.label.assignment.set.request",
            Self::Update(_) => "workspace.label.update.request",
            Self::Delete(_) => "workspace.label.delete.request",
            Self::DeleteInspect(_) => "workspace.label.delete.inspect.request",
        }
    }

    /// `msg.requestId`.
    #[must_use]
    pub const fn request_id(&self) -> &JsText {
        match self {
            Self::List(request) => &request.request_id,
            Self::AssignmentSet(request) => &request.request_id,
            Self::Update(request) => &request.request_id,
            Self::Delete(request) | Self::DeleteInspect(request) => &request.request_id,
        }
    }
}

/// `WorkspaceLabelSyncMetadataSchema.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceLabelSyncMode {
    Snapshot,
    Changes,
}

/// One `removals` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceLabelRemoval {
    pub name: JsText,
    pub seq: u64,
}

/// `WorkspaceLabelSyncMetadataSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceLabelSyncMetadata {
    pub mode: WorkspaceLabelSyncMode,
    pub generation: JsText,
    #[serde(rename = "headSeq")]
    pub head_seq: u64,
    pub removals: Vec<WorkspaceLabelRemoval>,
}

/// `workspace.label.list.response`: `requestId, subscriptionId?, labels, sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceLabelListResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "subscriptionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subscription_id: Option<JsText>,
    pub labels: Vec<WorkspaceLabelDefinition>,
    pub sync: WorkspaceLabelSyncMetadata,
}

/// `workspace.label.update` payload: `{ ...change, generation, seq }` with
/// `subscriptionId` appended last for an owned subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum WorkspaceLabelUpdate {
    Upsert {
        kind: UpsertKind,
        label: WorkspaceLabelDefinition,
        #[serde(
            rename = "previousName",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        previous_name: Option<JsText>,
        generation: JsText,
        seq: u64,
        #[serde(
            rename = "subscriptionId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        subscription_id: Option<JsText>,
    },
    Remove {
        kind: RemoveKind,
        name: JsText,
        generation: JsText,
        seq: u64,
        #[serde(
            rename = "subscriptionId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        subscription_id: Option<JsText>,
    },
}

/// `kind: "upsert"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum UpsertKind {
    #[serde(rename = "upsert")]
    Upsert,
}

/// `kind: "remove"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RemoveKind {
    #[serde(rename = "remove")]
    Remove,
}

/// `workspace.label.assignment.set.response`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceLabelAssignmentSetResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub label: WorkspaceLabelDefinition,
    #[serde(rename = "workspaceLabels")]
    pub workspace_labels: Vec<JsText>,
}

/// `workspace.label.update.response`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceLabelUpdateResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub label: WorkspaceLabelDefinition,
    #[serde(rename = "affectedWorkspaceCount")]
    pub affected_workspace_count: u64,
}

/// `workspace.label.delete.response` and
/// `workspace.label.delete.inspect.response`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceLabelCountResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(rename = "affectedWorkspaceCount")]
    pub affected_workspace_count: u64,
}

/// The six `workspace.label.*` messages the daemon sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum WorkspaceLabelOutbound {
    #[serde(rename = "workspace.label.list.response")]
    ListResponse { payload: WorkspaceLabelListResponse },
    #[serde(rename = "workspace.label.update")]
    Update { payload: WorkspaceLabelUpdate },
    #[serde(rename = "workspace.label.assignment.set.response")]
    AssignmentSetResponse {
        payload: WorkspaceLabelAssignmentSetResponse,
    },
    #[serde(rename = "workspace.label.update.response")]
    UpdateResponse {
        payload: WorkspaceLabelUpdateResponse,
    },
    #[serde(rename = "workspace.label.delete.response")]
    DeleteResponse {
        payload: WorkspaceLabelCountResponse,
    },
    #[serde(rename = "workspace.label.delete.inspect.response")]
    DeleteInspectResponse {
        payload: WorkspaceLabelCountResponse,
    },
}

// ---- zod ------------------------------------------------------------------

fn string() -> Schema {
    Schema::String(Vec::new())
}

fn literal(value: &str) -> Schema {
    Schema::Literal(vec![JsValue::String(value.to_owned())])
}

fn optional_of(schema: Schema) -> Schema {
    Schema::Optional(Box::new(schema))
}

fn object(shape: Vec<(&'static str, Schema)>) -> Schema {
    Schema::Object(shape, UnknownKeys::Strip)
}

fn color() -> Schema {
    Schema::Enum(&WORKSPACE_LABEL_COLORS)
}

fn definition() -> Schema {
    object(vec![("name", string()), ("color", color())])
}

fn request(kind: &str, mut shape: Vec<(&'static str, Schema)>) -> Schema {
    let mut fields = vec![("type", literal(kind)), ("requestId", string())];
    fields.append(&mut shape);
    object(fields)
}

fn cursor() -> Schema {
    object(vec![
        ("generation", string()),
        (
            "afterSeq",
            Schema::Number(vec![NumberCheck::Int, NumberCheck::Gte(0.0)]),
        ),
    ])
}

/// The session message schema of the five requests, keyed by `type`.
fn message_options() -> Vec<(&'static str, Schema)> {
    let name_only = |kind: &'static str| (kind, request(kind, vec![("name", string())]));
    vec![
        (
            "workspace.label.list.request",
            request(
                "workspace.label.list.request",
                vec![
                    (
                        "subscribe",
                        optional_of(object(vec![("subscriptionId", optional_of(string()))])),
                    ),
                    ("sync", optional_of(cursor())),
                ],
            ),
        ),
        (
            "workspace.label.assignment.set.request",
            request(
                "workspace.label.assignment.set.request",
                vec![
                    ("workspaceId", string()),
                    ("label", definition()),
                    ("assigned", Schema::Boolean),
                ],
            ),
        ),
        (
            "workspace.label.update.request",
            request(
                "workspace.label.update.request",
                vec![
                    ("name", string()),
                    ("newName", optional_of(string())),
                    ("color", optional_of(color())),
                ],
            ),
        ),
        name_only("workspace.label.delete.request"),
        name_only("workspace.label.delete.inspect.request"),
    ]
}

/// `WSInboundMessageSchema` for `{ type: "session", message }`, with only the
/// five label requests as session message options.
static SESSION_LABEL_MESSAGE: LazyLock<Schema> = LazyLock::new(|| {
    object(vec![
        ("type", literal("session")),
        ("message", Schema::Discriminated("type", message_options())),
    ])
});

/// Whether `message` is one of the five label requests, by `type`.
#[must_use]
pub fn is_label_request(message: &JsValue) -> bool {
    message
        .get("type")
        .and_then(JsValue::as_str)
        .is_some_and(|kind| {
            matches!(
                kind,
                "workspace.label.list.request"
                    | "workspace.label.assignment.set.request"
                    | "workspace.label.update.request"
                    | "workspace.label.delete.request"
                    | "workspace.label.delete.inspect.request"
            )
        })
}

/// Judges a session message of one of the five label requests as
/// `WSInboundMessageSchema.safeParse({ type: "session", message })` does;
/// `None` for any other message.
#[must_use]
pub fn check_session_message(message: &JsValue) -> Option<Outcome> {
    if !is_label_request(message) {
        return None;
    }
    let mut envelope = crate::js_value::JsObject::new();
    envelope.insert("type", JsValue::String("session".to_owned()));
    envelope.insert("message", message.clone());
    Some(check(&SESSION_LABEL_MESSAGE, &JsValue::Object(envelope)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js_value::parse;

    fn outcome(text: &str) -> Outcome {
        check_session_message(&parse(text).expect("json")).expect("label request")
    }

    #[test]
    fn colors_match_the_protocol_order() {
        let colors = [
            WorkspaceLabelColor::Violet,
            WorkspaceLabelColor::Sky,
            WorkspaceLabelColor::Emerald,
            WorkspaceLabelColor::Orange,
            WorkspaceLabelColor::Pink,
            WorkspaceLabelColor::Indigo,
            WorkspaceLabelColor::Teal,
            WorkspaceLabelColor::Red,
            WorkspaceLabelColor::Amber,
            WorkspaceLabelColor::Blue,
        ];
        for (color, name) in colors.iter().zip(WORKSPACE_LABEL_COLORS) {
            assert_eq!(serde_json::to_string(color).unwrap(), format!("\"{name}\""));
        }
    }

    #[test]
    fn list_request_accepts_subscribe_and_cursor() {
        let request: WorkspaceLabelInbound = WorkspaceLabelInbound::deserialize_as(
            "workspace.label.list.request",
            crate::json::JsValueDeserializer(
                &parse(r#"{"type":"workspace.label.list.request","requestId":"r","subscribe":{},"sync":{"generation":"g","afterSeq":3},"extra":1}"#)
                    .unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(request.request_type(), "workspace.label.list.request");
        assert_eq!(request.request_id().as_str(), "r");
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"type":"workspace.label.list.request","requestId":"r","subscribe":{},"sync":{"generation":"g","afterSeq":3}}"#
        );
    }

    #[test]
    fn responses_keep_construction_order() {
        let list = WorkspaceLabelOutbound::ListResponse {
            payload: WorkspaceLabelListResponse {
                request_id: "r".into(),
                subscription_id: Some("s".into()),
                labels: vec![WorkspaceLabelDefinition {
                    name: "A".into(),
                    color: WorkspaceLabelColor::Red,
                }],
                sync: WorkspaceLabelSyncMetadata {
                    mode: WorkspaceLabelSyncMode::Snapshot,
                    generation: "g".into(),
                    head_seq: 0,
                    removals: vec![],
                },
            },
        };
        assert_eq!(
            serde_json::to_string(&list).unwrap(),
            r#"{"type":"workspace.label.list.response","payload":{"requestId":"r","subscriptionId":"s","labels":[{"name":"A","color":"red"}],"sync":{"mode":"snapshot","generation":"g","headSeq":0,"removals":[]}}}"#
        );
        let update = WorkspaceLabelOutbound::Update {
            payload: WorkspaceLabelUpdate::Upsert {
                kind: UpsertKind::Upsert,
                label: WorkspaceLabelDefinition {
                    name: "B".into(),
                    color: WorkspaceLabelColor::Sky,
                },
                previous_name: Some("A".into()),
                generation: "g".into(),
                seq: 2,
                subscription_id: Some("s".into()),
            },
        };
        assert_eq!(
            serde_json::to_string(&update).unwrap(),
            r#"{"type":"workspace.label.update","payload":{"kind":"upsert","label":{"name":"B","color":"sky"},"previousName":"A","generation":"g","seq":2,"subscriptionId":"s"}}"#
        );
    }

    #[test]
    fn zod_judges_label_requests_with_the_session_path() {
        assert_eq!(
            outcome(r#"{"type":"workspace.label.delete.request","requestId":"r","name":"x"}"#),
            Outcome::Valid
        );
        let Outcome::Invalid(message) =
            outcome(r#"{"type":"workspace.label.delete.request","name":1}"#)
        else {
            panic!("invalid");
        };
        assert!(message.contains("\"message\",\n      \"requestId\""));
        assert!(check_session_message(&parse(r#"{"type":"ping"}"#).unwrap()).is_none());
    }
}
