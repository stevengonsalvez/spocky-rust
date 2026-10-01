//! Agent timeline items and fetched timeline entries (emit-only).
//!
//! The pinned daemon stores items as providers and the projection build them,
//! so one item type can carry different key orders. Each layout below names
//! its construction site at Paseo `5de45e2`. `agent_stream` events are zod
//! parsed before sending, so they use the schema layouts.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::field::{Nullable, optional};
use crate::json::{JsRecord, JsonValue};
use crate::number::{JsNumber, NonNegativeInt};
use crate::text::JsText;

/// Key order of a `user_message` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageLayout {
    /// `recordSubmittedPrompt` in `agent-manager.ts`:
    /// `type, text, clientMessageId, messageId?`.
    Submitted,
    /// Schema order, also `mapCodexThreadUserMessageItem`:
    /// `type, text, messageId?, clientMessageId?`.
    Schema,
}

/// Key order of an `assistant_message` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantMessageLayout {
    /// A Codex `agent_message_delta`: `type, messageId, text`.
    Delta,
    /// Schema order, also completed and merged items: `type, text, messageId?`.
    Schema,
}

/// Key order of a `tool_call` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallLayout {
    /// `codex/tool-call-mapper.ts`:
    /// `type, callId, name, status, error, detail, metadata?`.
    Mapper,
    /// Schema order (`ToolCallBasePayloadSchema.extend`):
    /// `type, callId, name, detail, metadata?, status, error`.
    Schema,
}

/// Key order of a `todo` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoLayout {
    /// `mapCodexPlanUpdateToTodo`: `id, text, status?, completed`.
    Codex,
    /// Schema order: `text, completed, id?, status?, activeForm?`.
    Schema,
}

/// `todo` entry status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One `todo` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoEntry {
    pub text: JsText,
    pub completed: bool,
    pub id: Option<JsText>,
    pub status: Option<TodoStatus>,
    pub active_form: Option<JsText>,
    pub layout: TodoLayout,
}

impl Serialize for TodoEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("TodoEntry", 5)?;
        match self.layout {
            TodoLayout::Codex => {
                write_some(&mut out, "id", self.id.as_ref())?;
                out.serialize_field("text", &self.text)?;
                write_some(&mut out, "status", self.status.as_ref())?;
                out.serialize_field("completed", &self.completed)?;
                write_some(&mut out, "activeForm", self.active_form.as_ref())?;
            }
            TodoLayout::Schema => {
                out.serialize_field("text", &self.text)?;
                out.serialize_field("completed", &self.completed)?;
                write_some(&mut out, "id", self.id.as_ref())?;
                write_some(&mut out, "status", self.status.as_ref())?;
                write_some(&mut out, "activeForm", self.active_form.as_ref())?;
            }
        }
        out.end()
    }
}

/// `tool_call.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    Running,
    Completed,
    Failed,
    Canceled,
}

/// `{ type: "shell", command, cwd?, output?, exitCode? }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShellDetail {
    pub command: JsText,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub cwd: Option<JsText>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub output: Option<JsText>,
    #[serde(
        rename = "exitCode",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub exit_code: Option<Nullable<JsNumber>>,
}

/// `ToolCallDetail`. Shell and unknown details are typed; other kinds are
/// carried exactly as the provider built them.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallDetail {
    Shell(ShellDetail),
    /// `{ type: "unknown", input, output }` from the Codex mapper.
    Unknown {
        input: JsonValue,
        output: JsonValue,
    },
    /// Any other detail object, `type` included.
    Other(JsonValue),
}

impl Serialize for ToolCallDetail {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Tagged<'a, T> {
            #[serde(rename = "type")]
            kind: &'static str,
            #[serde(flatten)]
            rest: &'a T,
        }
        #[derive(Serialize)]
        struct UnknownFields<'a> {
            input: &'a JsonValue,
            output: &'a JsonValue,
        }
        match self {
            Self::Shell(detail) => Tagged {
                kind: "shell",
                rest: detail,
            }
            .serialize(serializer),
            Self::Unknown { input, output } => Tagged {
                kind: "unknown",
                rest: &UnknownFields { input, output },
            }
            .serialize(serializer),
            Self::Other(value) => value.serialize(serializer),
        }
    }
}

/// A `tool_call` item. `error` is `null` unless `status` is `failed`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallItem {
    pub call_id: JsText,
    pub name: JsText,
    pub status: ToolCallStatus,
    pub error: JsonValue,
    pub detail: ToolCallDetail,
    pub metadata: Option<JsRecord<JsonValue>>,
    pub layout: ToolCallLayout,
}

/// `AgentTimelineItem`.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineItem {
    UserMessage {
        text: JsText,
        message_id: Option<JsText>,
        client_message_id: Option<JsText>,
        layout: UserMessageLayout,
    },
    AssistantMessage {
        text: JsText,
        message_id: Option<JsText>,
        layout: AssistantMessageLayout,
    },
    Reasoning {
        text: JsText,
    },
    ToolCall(Box<ToolCallItem>),
    Todo {
        items: Vec<TodoEntry>,
    },
    Error {
        message: JsText,
    },
    Notification {
        level: JsText,
        message: JsText,
    },
    /// `createContextCompactionTimelineItem`: `type, status, trigger?`.
    Compaction {
        status: JsText,
        trigger: Option<JsText>,
        pre_tokens: Option<JsNumber>,
    },
    Plugin {
        id: JsText,
        plugin_id: JsText,
        kind: JsText,
        version: JsNumber,
        data: JsonValue,
    },
}

fn write_some<S: SerializeStruct, T: Serialize>(
    out: &mut S,
    key: &'static str,
    value: Option<&T>,
) -> Result<(), S::Error> {
    match value {
        Some(value) => out.serialize_field(key, value),
        None => Ok(()),
    }
}

impl Serialize for TimelineItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("AgentTimelineItem", 8)?;
        match self {
            Self::UserMessage {
                text,
                message_id,
                client_message_id,
                layout,
            } => {
                out.serialize_field("type", "user_message")?;
                out.serialize_field("text", text)?;
                match layout {
                    UserMessageLayout::Submitted => {
                        write_some(&mut out, "clientMessageId", client_message_id.as_ref())?;
                        write_some(&mut out, "messageId", message_id.as_ref())?;
                    }
                    UserMessageLayout::Schema => {
                        write_some(&mut out, "messageId", message_id.as_ref())?;
                        write_some(&mut out, "clientMessageId", client_message_id.as_ref())?;
                    }
                }
            }
            Self::AssistantMessage {
                text,
                message_id,
                layout,
            } => {
                out.serialize_field("type", "assistant_message")?;
                match layout {
                    AssistantMessageLayout::Delta => {
                        write_some(&mut out, "messageId", message_id.as_ref())?;
                        out.serialize_field("text", text)?;
                    }
                    AssistantMessageLayout::Schema => {
                        out.serialize_field("text", text)?;
                        write_some(&mut out, "messageId", message_id.as_ref())?;
                    }
                }
            }
            Self::Reasoning { text } => {
                out.serialize_field("type", "reasoning")?;
                out.serialize_field("text", text)?;
            }
            Self::ToolCall(call) => {
                out.serialize_field("type", "tool_call")?;
                out.serialize_field("callId", &call.call_id)?;
                out.serialize_field("name", &call.name)?;
                match call.layout {
                    ToolCallLayout::Mapper => {
                        out.serialize_field("status", &call.status)?;
                        out.serialize_field("error", &call.error)?;
                        out.serialize_field("detail", &call.detail)?;
                        write_some(&mut out, "metadata", call.metadata.as_ref())?;
                    }
                    ToolCallLayout::Schema => {
                        out.serialize_field("detail", &call.detail)?;
                        write_some(&mut out, "metadata", call.metadata.as_ref())?;
                        out.serialize_field("status", &call.status)?;
                        out.serialize_field("error", &call.error)?;
                    }
                }
            }
            Self::Todo { items } => {
                out.serialize_field("type", "todo")?;
                out.serialize_field("items", items)?;
            }
            Self::Error { message } => {
                out.serialize_field("type", "error")?;
                out.serialize_field("message", message)?;
            }
            Self::Notification { level, message } => {
                out.serialize_field("type", "notification")?;
                out.serialize_field("level", level)?;
                out.serialize_field("message", message)?;
            }
            Self::Compaction {
                status,
                trigger,
                pre_tokens,
            } => {
                out.serialize_field("type", "compaction")?;
                out.serialize_field("status", status)?;
                write_some(&mut out, "trigger", trigger.as_ref())?;
                write_some(&mut out, "preTokens", pre_tokens.as_ref())?;
            }
            Self::Plugin {
                id,
                plugin_id,
                kind,
                version,
                data,
            } => {
                out.serialize_field("type", "plugin")?;
                out.serialize_field("id", id)?;
                out.serialize_field("pluginId", plugin_id)?;
                out.serialize_field("kind", kind)?;
                out.serialize_field("version", version)?;
                out.serialize_field("data", data)?;
            }
        }
        out.end()
    }
}

/// `{ startSeq, endSeq }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SeqRange {
    #[serde(rename = "startSeq")]
    pub start_seq: NonNegativeInt,
    #[serde(rename = "endSeq")]
    pub end_seq: NonNegativeInt,
}

/// `collapsed` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCollapse {
    AssistantMerge,
    ReasoningMerge,
    ToolLifecycle,
    Identity,
}

/// A `fetch_agent_timeline_response` entry as `handleFetchAgentTimelineRequest`
/// builds it: `turnId` keeps its slot before `collapsed` and is dropped when
/// unset.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEntry {
    pub provider: JsText,
    pub item: TimelineItem,
    pub timestamp: JsText,
    #[serde(rename = "seqStart")]
    pub seq_start: NonNegativeInt,
    #[serde(rename = "seqEnd")]
    pub seq_end: NonNegativeInt,
    #[serde(rename = "sourceSeqRanges")]
    pub source_seq_ranges: Vec<SeqRange>,
    #[serde(
        rename = "turnId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub turn_id: Option<JsText>,
    pub collapsed: Vec<TimelineCollapse>,
}

/// `{ minSeq, maxSeq, nextSeq }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimelineWindow {
    #[serde(rename = "minSeq")]
    pub min_seq: NonNegativeInt,
    #[serde(rename = "maxSeq")]
    pub max_seq: NonNegativeInt,
    #[serde(rename = "nextSeq")]
    pub next_seq: NonNegativeInt,
}

/// `{ epoch, seq }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineCursor {
    pub epoch: JsText,
    pub seq: NonNegativeInt,
}
