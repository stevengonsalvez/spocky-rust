//! `subagents/observation.ts`: the facts a source observes about one
//! provider subagent, and their fold into `provider_subagent` store events.

use spocky_contracts::js_value::{JsObject, JsValue};

/// `ProviderSubagentStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Canceled,
}

impl SubagentStatus {
    /// The wire text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// `SubagentObservation`.
#[derive(Debug, Clone, PartialEq)]
pub enum SubagentObservation {
    Declared {
        id: String,
        title: Option<String>,
        description: Option<String>,
        tool_call_id: Option<String>,
        parent_subagent_id: Option<String>,
        timestamp: Option<String>,
    },
    Status {
        id: String,
        status: SubagentStatus,
        timestamp: Option<String>,
    },
    Subtitle {
        id: String,
        subtitle: String,
        timestamp: Option<String>,
    },
    Timeline {
        id: String,
        /// `AgentTimelineItem`.
        item: JsValue,
        timestamp: Option<String>,
    },
}

impl SubagentObservation {
    /// The subagent the observation is about.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Declared { id, .. }
            | Self::Status { id, .. }
            | Self::Subtitle { id, .. }
            | Self::Timeline { id, .. } => id,
        }
    }
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn insert_timestamp(event: &mut JsObject, timestamp: Option<&String>) {
    if let Some(timestamp) = timestamp.filter(|timestamp| !timestamp.is_empty()) {
        event.insert("timestamp", text(timestamp));
    }
}

/// `foldSubagentObservations(observations)`: `ProviderSubagentInputEvent`s.
#[must_use]
pub fn fold_subagent_observations(observations: &[SubagentObservation]) -> Vec<JsValue> {
    observations
        .iter()
        .map(|observation| {
            let mut event = JsObject::new();
            match observation {
                SubagentObservation::Timeline {
                    id,
                    item,
                    timestamp,
                } => {
                    event.insert("type", text("timeline"));
                    event.insert("id", text(id));
                    event.insert("item", item.clone());
                    insert_timestamp(&mut event, timestamp.as_ref());
                }
                SubagentObservation::Subtitle {
                    id,
                    subtitle,
                    timestamp,
                } => {
                    event.insert("type", text("upsert"));
                    event.insert("id", text(id));
                    event.insert("subtitle", text(subtitle));
                    insert_timestamp(&mut event, timestamp.as_ref());
                }
                SubagentObservation::Status {
                    id,
                    status,
                    timestamp,
                } => {
                    event.insert("type", text("upsert"));
                    event.insert("id", text(id));
                    event.insert("status", text(status.as_str()));
                    insert_timestamp(&mut event, timestamp.as_ref());
                }
                SubagentObservation::Declared {
                    id,
                    title,
                    description,
                    tool_call_id,
                    parent_subagent_id,
                    timestamp,
                } => {
                    event.insert("type", text("upsert"));
                    event.insert("id", text(id));
                    event.insert("status", text("running"));
                    for (key, value) in [
                        ("title", title),
                        ("description", description),
                        ("toolCallId", tool_call_id),
                        ("parentSubagentId", parent_subagent_id),
                    ] {
                        if let Some(value) = value {
                            event.insert(key, text(value));
                        }
                    }
                    insert_timestamp(&mut event, timestamp.as_ref());
                }
            }
            JsValue::Object(event)
        })
        .collect()
}
