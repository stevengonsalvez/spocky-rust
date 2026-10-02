//! `subagents/live-source.ts`: `ClaudeTaskProtocolSource`, which reads
//! Claude Code's task announcements (`task_started`, `task_updated`,
//! `task_notification`, `task_progress`) and hook inputs into subagent
//! observations.

use std::collections::{HashMap, HashSet};

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_contracts::text::js_trim;

use super::observation::{SubagentObservation, SubagentStatus};
use super::presentation::{PresentationFacts, build_claude_subagent_subtitle};
use crate::models::resolve_observed_claude_model_id;

const CLAUDE_SUBAGENT_TASK_TYPE: &str = "local_agent";
const CLAUDE_WORKFLOW_TASK_TYPE: &str = "local_workflow";

/// A JavaScript `Map` key for a `task_id` of any type (`undefined` too).
fn task_key(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "u".to_owned(),
        Some(JsValue::String(text)) => format!("s{text}"),
        // ponytail: objects compare by identity in a Map; every frame is
        // freshly parsed, so equal JSON text stands in for identity.
        Some(other) => format!("v{}", stringify(other)),
    }
}

/// `readString(value)`: a trimmed non-empty string.
fn read_string(value: Option<&JsValue>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn truthy(value: Option<&JsValue>) -> bool {
    spocky_contracts::js::truthy(value)
}

/// `mapTaskStatus(status)`.
fn map_task_status(status: Option<&JsValue>) -> Option<SubagentStatus> {
    match status?.as_str()? {
        "pending" | "running" | "paused" => Some(SubagentStatus::Running),
        "completed" => Some(SubagentStatus::Completed),
        "failed" => Some(SubagentStatus::Failed),
        "killed" | "stopped" => Some(SubagentStatus::Canceled),
        _ => None,
    }
}

/// `readUsage(usage)`: the `total_tokens` patch, if any.
fn read_usage(usage: Option<&JsValue>) -> Option<f64> {
    if !truthy(usage) {
        return None;
    }
    usage?.get("total_tokens")?.as_f64()
}

/// Looks up a parent tool call's input by `tool_use` id.
pub type ToolInputLookup = Box<dyn Fn(&str) -> Option<JsObject> + Send + Sync>;
/// Reads a workflow's result text from its output file.
pub type WorkflowResultReader = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// `ClaudeTaskProtocolSource`.
pub struct ClaudeTaskProtocolSource {
    subagent_id_by_task_id: HashMap<String, String>,
    canonical_id_by_tool_use_id: HashMap<String, String>,
    owner_subagent_id_by_tool_use_id: HashMap<String, String>,
    owner_subagent_id_by_task_id: HashMap<String, String>,
    /// `declaredIds`, in insertion order.
    declared_ids: Vec<String>,
    workflow_task_ids: HashSet<String>,
    last_workflow_result_by_task_id: HashMap<String, String>,
    ids_with_existing_parent_tool_card: HashSet<String>,
    backgrounded_ids: HashSet<String>,
    last_status_by_id: HashMap<String, SubagentStatus>,
    presentation_by_id: HashMap<String, PresentationFacts>,
    last_subtitle_by_id: HashMap<String, String>,
    saw_task_started: bool,
    saw_any_task: bool,
    get_tool_input: ToolInputLookup,
    read_workflow_result: WorkflowResultReader,
}

impl ClaudeTaskProtocolSource {
    /// `new ClaudeTaskProtocolSource({ getToolInput, readWorkflowResult })`.
    #[must_use]
    pub fn new(
        get_tool_input: ToolInputLookup,
        read_workflow_result: WorkflowResultReader,
    ) -> Self {
        Self {
            subagent_id_by_task_id: HashMap::new(),
            canonical_id_by_tool_use_id: HashMap::new(),
            owner_subagent_id_by_tool_use_id: HashMap::new(),
            owner_subagent_id_by_task_id: HashMap::new(),
            declared_ids: Vec::new(),
            workflow_task_ids: HashSet::new(),
            last_workflow_result_by_task_id: HashMap::new(),
            ids_with_existing_parent_tool_card: HashSet::new(),
            backgrounded_ids: HashSet::new(),
            last_status_by_id: HashMap::new(),
            presentation_by_id: HashMap::new(),
            last_subtitle_by_id: HashMap::new(),
            saw_task_started: false,
            saw_any_task: false,
            get_tool_input,
            read_workflow_result,
        }
    }

    fn is_declared_id(&self, id: &str) -> bool {
        self.declared_ids.iter().any(|declared| declared == id)
    }

    /// `isActive`.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.saw_task_started
    }

    /// `announcesTasks`.
    #[must_use]
    pub const fn announces_tasks(&self) -> bool {
        self.saw_any_task
    }

    /// `isDeclared(subagentId)`.
    #[must_use]
    pub fn is_declared(&self, subagent_id: &str) -> bool {
        self.is_declared_id(subagent_id)
    }

    /// `resolveSubagentId(toolUseId)`.
    #[must_use]
    pub fn resolve_subagent_id(&self, tool_use_id: &str) -> Option<String> {
        self.canonical_id_by_tool_use_id
            .get(tool_use_id)
            .filter(|id| self.is_declared_id(id))
            .cloned()
    }

    /// `isDeclaredTask(taskId)`.
    #[must_use]
    pub fn is_declared_task(&self, task_id: Option<&JsValue>) -> bool {
        self.subagent_id_by_task_id
            .get(&task_key(task_id))
            .is_some_and(|id| self.is_declared_id(id))
    }

    /// `resolveTaskOwner(taskId, toolUseId)`.
    #[must_use]
    pub fn resolve_task_owner(
        &self,
        task_id: Option<&JsValue>,
        tool_use_id: Option<&str>,
    ) -> Option<String> {
        self.owner_subagent_id_by_task_id
            .get(&task_key(task_id))
            .or_else(|| {
                tool_use_id
                    .filter(|id| !id.is_empty())
                    .and_then(|id| self.owner_subagent_id_by_tool_use_id.get(id))
            })
            .cloned()
    }

    /// `needsSyntheticParentToolCard(subagentId)`.
    #[must_use]
    pub fn needs_synthetic_parent_tool_card(&self, subagent_id: &str) -> bool {
        !self
            .ids_with_existing_parent_tool_card
            .contains(subagent_id)
    }

    /// `observe(message)`.
    pub fn observe(&mut self, message: &JsValue) -> Vec<SubagentObservation> {
        if message.get("type").and_then(JsValue::as_str) != Some("system") {
            return Vec::new();
        }
        let Some(record) = message.as_object() else {
            return Vec::new();
        };
        match record.get("subtype").and_then(JsValue::as_str) {
            Some("task_started") => self.observe_task_started(record),
            Some("task_updated") => self.observe_task_updated(record),
            Some("task_notification") => self.observe_task_notification(record),
            Some("task_progress") => self.observe_usage(record.get("task_id"), record.get("usage")),
            _ => Vec::new(),
        }
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        self.subagent_id_by_task_id.clear();
        self.canonical_id_by_tool_use_id.clear();
        self.owner_subagent_id_by_tool_use_id.clear();
        self.owner_subagent_id_by_task_id.clear();
        self.declared_ids.clear();
        self.workflow_task_ids.clear();
        self.last_workflow_result_by_task_id.clear();
        self.ids_with_existing_parent_tool_card.clear();
        self.backgrounded_ids.clear();
        self.last_status_by_id.clear();
        self.presentation_by_id.clear();
        self.last_subtitle_by_id.clear();
        self.saw_task_started = false;
        self.saw_any_task = false;
    }

    /// `cancelRunningForegroundTasks()`.
    pub fn cancel_running_foreground_tasks(&mut self) -> Vec<SubagentObservation> {
        let mut observations = Vec::new();
        for id in &self.declared_ids {
            if self.backgrounded_ids.contains(id) {
                continue;
            }
            if self.last_status_by_id.get(id) != Some(&SubagentStatus::Running) {
                continue;
            }
            self.last_status_by_id
                .insert(id.clone(), SubagentStatus::Canceled);
            observations.push(SubagentObservation::Status {
                id: id.clone(),
                status: SubagentStatus::Canceled,
                timestamp: None,
            });
        }
        observations
    }

    /// `failRunningTasks()`.
    pub fn fail_running_tasks(&mut self) -> Vec<SubagentObservation> {
        let mut observations = Vec::new();
        for id in &self.declared_ids {
            if self.last_status_by_id.get(id) != Some(&SubagentStatus::Running) {
                continue;
            }
            self.last_status_by_id
                .insert(id.clone(), SubagentStatus::Failed);
            observations.push(SubagentObservation::Status {
                id: id.clone(),
                status: SubagentStatus::Failed,
                timestamp: None,
            });
        }
        observations
    }

    fn is_provider_subagent_task(message: &JsObject) -> bool {
        if truthy(message.get("task_type")) {
            return matches!(
                message.get("task_type").and_then(JsValue::as_str),
                Some(CLAUDE_SUBAGENT_TASK_TYPE | CLAUDE_WORKFLOW_TASK_TYPE)
            );
        }
        read_string(message.get("subagent_type")).is_some()
    }

    fn observe_task_started(&mut self, message: &JsObject) -> Vec<SubagentObservation> {
        self.saw_any_task = true;
        let task_id = task_key(message.get("task_id"));
        let id = read_string(message.get("tool_use_id"));
        let parent_subagent_id = id
            .as_ref()
            .and_then(|id| self.owner_subagent_id_by_tool_use_id.get(id))
            .cloned();
        if let Some(parent) = &parent_subagent_id {
            self.owner_subagent_id_by_task_id
                .insert(task_id.clone(), parent.clone());
        }
        let Some(id) = id else {
            return Vec::new();
        };
        if message.get("skip_transcript") == Some(&JsValue::Bool(true))
            || !Self::is_provider_subagent_task(message)
        {
            return Vec::new();
        }
        self.saw_task_started = true;
        if let Some(existing) = self.subagent_id_by_task_id.get(&task_id).cloned() {
            return self.observe_existing_task_start(message, &id, &existing);
        }
        self.observe_new_task_start(message, task_id, &id, parent_subagent_id)
    }

    fn is_workflow(message: &JsObject) -> bool {
        message.get("task_type").and_then(JsValue::as_str) == Some(CLAUDE_WORKFLOW_TASK_TYPE)
    }

    fn observe_existing_task_start(
        &mut self,
        message: &JsObject,
        tool_use_id: &str,
        existing_id: &str,
    ) -> Vec<SubagentObservation> {
        self.canonical_id_by_tool_use_id
            .insert(tool_use_id.to_owned(), existing_id.to_owned());
        let mut observations = Vec::new();
        if self.last_status_by_id.get(existing_id) != Some(&SubagentStatus::Running) {
            self.last_status_by_id
                .insert(existing_id.to_owned(), SubagentStatus::Running);
            observations.push(SubagentObservation::Status {
                id: existing_id.to_owned(),
                status: SubagentStatus::Running,
                timestamp: None,
            });
        }
        let prompt = if Self::is_workflow(message) {
            read_string(message.get("description"))
        } else {
            read_string(message.get("prompt"))
        };
        if let Some(prompt) = prompt {
            observations.push(user_message(existing_id, &prompt));
        }
        observations
    }

    fn observe_new_task_start(
        &mut self,
        message: &JsObject,
        task_id: String,
        id: &str,
        parent_subagent_id: Option<String>,
    ) -> Vec<SubagentObservation> {
        let id = id.to_owned();
        self.subagent_id_by_task_id
            .insert(task_id.clone(), id.clone());
        self.canonical_id_by_tool_use_id
            .insert(id.clone(), id.clone());
        if !self.is_declared_id(&id) {
            self.declared_ids.push(id.clone());
        }
        self.last_status_by_id
            .insert(id.clone(), SubagentStatus::Running);
        let is_workflow = Self::is_workflow(message);
        if is_workflow || parent_subagent_id.is_some() {
            self.ids_with_existing_parent_tool_card.insert(id.clone());
        }
        if is_workflow {
            self.workflow_task_ids.insert(task_id);
        }
        let title = if is_workflow {
            Some("Workflow".to_owned())
        } else {
            (self.get_tool_input)(&id)
                .and_then(|input| read_string(input.get("name")))
                .or_else(|| read_string(message.get("subagent_type")))
        };
        let description = read_string(message.get("description"));
        let mut observations = vec![SubagentObservation::Declared {
            id: id.clone(),
            title: title.clone(),
            description: description.clone(),
            tool_call_id: Some(id.clone()),
            parent_subagent_id,
            timestamp: None,
        }];
        let initial = PresentationFacts {
            title,
            ..PresentationFacts::default()
        };
        if let Some(subtitle) = build_claude_subagent_subtitle(&initial) {
            self.last_subtitle_by_id.insert(id.clone(), subtitle);
        }
        self.presentation_by_id.insert(id.clone(), initial);
        let prompt = if is_workflow {
            description
        } else {
            read_string(message.get("prompt"))
        };
        if let Some(prompt) = prompt {
            observations.push(user_message(&id, &prompt));
        }
        observations
    }

    fn observe_task_updated(&mut self, message: &JsObject) -> Vec<SubagentObservation> {
        let task_id = task_key(message.get("task_id"));
        let patch = message.get("patch").filter(|patch| truthy(Some(patch)));
        if let Some(id) = self.subagent_id_by_task_id.get(&task_id).cloned()
            && let Some(backgrounded) = patch
                .and_then(|patch| patch.get("is_backgrounded"))
                .and_then(JsValue::as_bool)
        {
            if backgrounded {
                self.backgrounded_ids.insert(id);
            } else {
                self.backgrounded_ids.remove(&id);
            }
        }
        self.observe_status(&task_id, patch.and_then(|patch| patch.get("status")))
    }

    fn observe_task_notification(&mut self, message: &JsObject) -> Vec<SubagentObservation> {
        let mut observations = self.observe_workflow_result(message);
        observations.extend(self.observe_usage(message.get("task_id"), message.get("usage")));
        let task_id = task_key(message.get("task_id"));
        observations.extend(self.observe_status(&task_id, message.get("status")));
        observations
    }

    fn observe_workflow_result(&mut self, message: &JsObject) -> Vec<SubagentObservation> {
        let task_id = task_key(message.get("task_id"));
        if !self.workflow_task_ids.contains(&task_id) {
            return Vec::new();
        }
        let (Some(id), Some(output_file)) = (
            self.subagent_id_by_task_id.get(&task_id).cloned(),
            read_string(message.get("output_file")),
        ) else {
            return Vec::new();
        };
        let Some(text) = (self.read_workflow_result)(&output_file).filter(|text| !text.is_empty())
        else {
            return Vec::new();
        };
        if self.last_workflow_result_by_task_id.get(&task_id) == Some(&text) {
            return Vec::new();
        }
        self.last_workflow_result_by_task_id
            .insert(task_id, text.clone());
        let mut item = JsObject::new();
        item.insert("type", JsValue::String("assistant_message".to_owned()));
        item.insert("text", JsValue::String(text));
        vec![SubagentObservation::Timeline {
            id,
            item: JsValue::Object(item),
            timestamp: None,
        }]
    }

    fn observe_usage(
        &mut self,
        task_id: Option<&JsValue>,
        usage: Option<&JsValue>,
    ) -> Vec<SubagentObservation> {
        let id = self.subagent_id_by_task_id.get(&task_key(task_id)).cloned();
        let (Some(id), Some(total_tokens)) = (id, read_usage(usage)) else {
            return Vec::new();
        };
        self.update_presentation(
            &id,
            &PresentationFacts {
                total_tokens: Some(total_tokens),
                ..PresentationFacts::default()
            },
        )
    }

    /// `observeHook(input)`.
    pub fn observe_hook(&mut self, input: &JsValue) -> Vec<SubagentObservation> {
        let task_id = read_string(input.get("agent_id"));
        let effort = read_string(input.get("effort").and_then(|effort| effort.get("level")));
        let (Some(task_id), Some(effort)) = (task_id, effort) else {
            return Vec::new();
        };
        let Some(id) = self
            .subagent_id_by_task_id
            .get(&task_key(Some(&JsValue::String(task_id))))
            .cloned()
        else {
            return Vec::new();
        };
        self.update_presentation(
            &id,
            &PresentationFacts {
                effort: Some(effort),
                ..PresentationFacts::default()
            },
        )
    }

    /// `observeSidechainFrame(message, subagentId)`.
    pub fn observe_sidechain_frame(
        &mut self,
        message: &JsValue,
        subagent_id: &str,
    ) -> Vec<SubagentObservation> {
        if message.get("type").and_then(JsValue::as_str) != Some("assistant")
            || !self.is_declared_id(subagent_id)
        {
            return Vec::new();
        }
        let inner = message.get("message");
        if let Some(content) = inner
            .and_then(|inner| inner.get("content"))
            .and_then(JsValue::as_array)
        {
            for block in content {
                if block.get("type").and_then(JsValue::as_str) == Some("tool_use")
                    && let Some(id) = block.get("id").and_then(JsValue::as_str)
                {
                    self.owner_subagent_id_by_tool_use_id
                        .insert(id.to_owned(), subagent_id.to_owned());
                }
            }
        }
        let model = resolve_observed_claude_model_id(
            inner
                .and_then(|inner| inner.get("model"))
                .and_then(JsValue::as_str),
        );
        let Some(model) = model else {
            return Vec::new();
        };
        self.update_presentation(
            subagent_id,
            &PresentationFacts {
                model: Some(model),
                ..PresentationFacts::default()
            },
        )
    }

    fn update_presentation(
        &mut self,
        id: &str,
        patch: &PresentationFacts,
    ) -> Vec<SubagentObservation> {
        let next = self
            .presentation_by_id
            .get(id)
            .cloned()
            .unwrap_or_default()
            .merged(patch);
        let subtitle = build_claude_subagent_subtitle(&next);
        self.presentation_by_id.insert(id.to_owned(), next);
        let Some(subtitle) = subtitle else {
            return Vec::new();
        };
        if self.last_subtitle_by_id.get(id) == Some(&subtitle) {
            return Vec::new();
        }
        self.last_subtitle_by_id
            .insert(id.to_owned(), subtitle.clone());
        vec![SubagentObservation::Subtitle {
            id: id.to_owned(),
            subtitle,
            timestamp: None,
        }]
    }

    fn observe_status(
        &mut self,
        task_id: &str,
        raw_status: Option<&JsValue>,
    ) -> Vec<SubagentObservation> {
        let Some(id) = self.subagent_id_by_task_id.get(task_id).cloned() else {
            return Vec::new();
        };
        let Some(status) = map_task_status(raw_status) else {
            return Vec::new();
        };
        if self.last_status_by_id.get(&id) == Some(&status) {
            return Vec::new();
        }
        self.last_status_by_id.insert(id.clone(), status);
        vec![SubagentObservation::Status {
            id,
            status,
            timestamp: None,
        }]
    }
}

fn user_message(id: &str, prompt: &str) -> SubagentObservation {
    let mut item = JsObject::new();
    item.insert("type", JsValue::String("user_message".to_owned()));
    item.insert("text", JsValue::String(prompt.to_owned()));
    SubagentObservation::Timeline {
        id: id.to_owned(),
        item: JsValue::Object(item),
        timestamp: None,
    }
}
