//! `task-state.ts`: accumulates Claude's snapshot (`TodoWrite`) and id-based
//! (`TaskCreate`, `TaskUpdate`, `TaskList`) task tools into `todo` timeline
//! snapshots.

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::js_trim;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskToolName {
    TodoWrite,
    TaskCreate,
    TaskUpdate,
    TaskList,
}

fn task_tool_name(value: &str) -> Option<TaskToolName> {
    match value {
        "TodoWrite" => Some(TaskToolName::TodoWrite),
        "TaskCreate" => Some(TaskToolName::TaskCreate),
        "TaskUpdate" => Some(TaskToolName::TaskUpdate),
        "TaskList" => Some(TaskToolName::TaskList),
        _ => None,
    }
}

fn record(value: Option<&JsValue>) -> Option<&JsObject> {
    value.and_then(JsValue::as_object)
}

/// `string(value)`: a trimmed non-empty string.
fn string(value: Option<&JsValue>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `taskStatus(value)`, with `deleted` kept as a status.
fn task_status(value: Option<&JsValue>) -> &'static str {
    match value.and_then(JsValue::as_str) {
        Some("completed") => "completed",
        Some("deleted") => "deleted",
        Some("in_progress") => "in_progress",
        _ => "pending",
    }
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `toTaskItem(value)`: an `AgentTaskItem`.
fn to_task_item(value: &JsValue) -> Option<JsObject> {
    let task = value.as_object()?;
    let item_text = string(task.get("subject"))
        .or_else(|| string(task.get("content")))
        .or_else(|| string(task.get("text")))?;
    let status = task_status(task.get("status"));
    if status == "deleted" {
        return None;
    }
    let id = string(task.get("id")).or_else(|| string(task.get("taskId")));
    let active_form = string(task.get("activeForm")).or_else(|| string(task.get("active_form")));
    let mut item = JsObject::new();
    if let Some(id) = id {
        item.insert("id", text(&id));
    }
    item.insert("text", text(&item_text));
    item.insert("status", text(status));
    item.insert("completed", JsValue::Bool(status == "completed"));
    if let Some(active_form) = active_form {
        item.insert("activeForm", text(&active_form));
    }
    Some(item)
}

fn content(message: &JsObject) -> Option<&[JsValue]> {
    record(message.get("message"))?.get("content")?.as_array()
}

fn tool_uses(message: &JsObject) -> Vec<&JsObject> {
    content(message)
        .unwrap_or_default()
        .iter()
        .filter_map(JsValue::as_object)
        .filter(|block| block.get("type").and_then(JsValue::as_str) == Some("tool_use"))
        .collect()
}

fn tool_result_id(message: &JsObject) -> Option<String> {
    content(message)?
        .iter()
        .filter_map(JsValue::as_object)
        .find(|block| block.get("type").and_then(JsValue::as_str) == Some("tool_result"))
        .and_then(|block| string(block.get("tool_use_id")))
}

fn structured_result(message: &JsObject) -> Option<&JsObject> {
    record(message.get("toolUseResult")).or_else(|| record(message.get("tool_use_result")))
}

/// `ClaudeTaskState`.
#[derive(Debug, Default)]
pub struct ClaudeTaskState {
    /// `tasks`: a `Map` in insertion order.
    tasks: Vec<(String, JsObject)>,
    calls: Vec<(String, (TaskToolName, JsObject))>,
    applied_results: Vec<String>,
}

impl ClaudeTaskState {
    fn set_task(&mut self, id: String, task: JsObject) {
        match self.tasks.iter_mut().find(|(existing, _)| *existing == id) {
            Some(slot) => slot.1 = task,
            None => self.tasks.push((id, task)),
        }
    }

    fn task(&self, id: &str) -> Option<&JsObject> {
        self.tasks
            .iter()
            .find(|(existing, _)| existing == id)
            .map(|(_, task)| task)
    }

    /// `observe(value)`: the `todo` item this message produces, if any.
    pub fn observe(&mut self, value: &JsValue) -> Option<JsValue> {
        let message = value.as_object()?;
        let mut snapshot = None;
        for block in tool_uses(message) {
            let (Some(id), Some(name)) = (
                string(block.get("id")),
                string(block.get("name")).and_then(|name| task_tool_name(&name)),
            ) else {
                continue;
            };
            let input = record(block.get("input")).cloned().unwrap_or_default();
            let todos = input.get("todos").cloned();
            match self.calls.iter_mut().find(|(existing, _)| *existing == id) {
                Some(slot) => slot.1 = (name, input),
                None => self.calls.push((id, (name, input))),
            }
            if name == TaskToolName::TodoWrite {
                snapshot = Some(self.replace_legacy_todos(todos.as_ref()));
            }
        }
        let Some(result_id) = tool_result_id(message) else {
            return snapshot;
        };
        if self.applied_results.contains(&result_id) {
            return snapshot;
        }
        let Some(position) = self.calls.iter().position(|(id, _)| *id == result_id) else {
            return snapshot;
        };
        self.applied_results.push(result_id);
        let (_, (name, input)) = self.calls.remove(position);
        self.apply_result(name, &input, structured_result(message))
            .or(snapshot)
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        self.tasks.clear();
        self.calls.clear();
        self.applied_results.clear();
    }

    fn replace_legacy_todos(&mut self, value: Option<&JsValue>) -> JsValue {
        self.tasks.clear();
        if let Some(todos) = value.and_then(JsValue::as_array) {
            for (index, todo) in todos.iter().enumerate() {
                let Some(mut item) = to_task_item(todo) else {
                    continue;
                };
                let id = item
                    .get("id")
                    .and_then(JsValue::as_str)
                    .map_or_else(|| format!("legacy:{index}"), str::to_owned);
                item.insert("id", text(&id));
                self.set_task(id, item);
            }
        }
        self.snapshot()
    }

    fn apply_result(
        &mut self,
        name: TaskToolName,
        input: &JsObject,
        result: Option<&JsObject>,
    ) -> Option<JsValue> {
        if result.and_then(|result| result.get("success")) == Some(&JsValue::Bool(false)) {
            return None;
        }
        match name {
            TaskToolName::TaskCreate => self.apply_create(input, result),
            TaskToolName::TaskUpdate => self.apply_update(input, result),
            TaskToolName::TaskList => self.apply_list(result),
            TaskToolName::TodoWrite => None,
        }
    }

    fn apply_create(&mut self, input: &JsObject, result: Option<&JsObject>) -> Option<JsValue> {
        let result_task = record(result.and_then(|result| result.get("task")));
        let id = string(result_task.and_then(|task| task.get("id")))
            .or_else(|| string(result.and_then(|result| result.get("taskId"))))?;
        let task_text = string(result_task.and_then(|task| task.get("subject")))
            .or_else(|| string(input.get("subject")))?;
        let active_form = string(input.get("activeForm"));
        let mut task = JsObject::new();
        task.insert("id", text(&id));
        task.insert("text", text(&task_text));
        task.insert("status", text("pending"));
        task.insert("completed", JsValue::Bool(false));
        if let Some(active_form) = active_form {
            task.insert("activeForm", text(&active_form));
        }
        self.set_task(id, task);
        Some(self.snapshot())
    }

    fn apply_update(&mut self, input: &JsObject, result: Option<&JsObject>) -> Option<JsValue> {
        let id = string(input.get("taskId"))
            .or_else(|| string(result.and_then(|result| result.get("taskId"))))?;
        let current = self.task(&id)?.clone();
        // `input.status ?? statusChange?.to`: a null input status falls through,
        // and only a missing value keeps the current status.
        let status_value = match input.get("status") {
            Some(value) if !matches!(value, JsValue::Undefined | JsValue::Null) => Some(value),
            _ => record(result.and_then(|result| result.get("statusChange")))
                .and_then(|change| change.get("to")),
        }
        .filter(|value| !matches!(value, JsValue::Undefined));
        let status = match status_value {
            None => retained_task_status(&current),
            Some(value) => task_status(Some(value)),
        };
        if status == "deleted" {
            self.tasks.retain(|(existing, _)| *existing != id);
            return Some(self.snapshot());
        }
        let mut task = current;
        if let Some(task_text) = string(input.get("subject")) {
            task.insert("text", text(&task_text));
        }
        if let Some(active_form) = string(input.get("activeForm")) {
            task.insert("activeForm", text(&active_form));
        }
        task.insert("status", text(status));
        task.insert("completed", JsValue::Bool(status == "completed"));
        self.set_task(id, task);
        Some(self.snapshot())
    }

    fn apply_list(&mut self, result: Option<&JsObject>) -> Option<JsValue> {
        let tasks = result?.get("tasks")?.as_array()?;
        self.tasks.clear();
        for task in tasks {
            if let Some(item) = to_task_item(task)
                && let Some(id) = item.get("id").and_then(JsValue::as_str).map(str::to_owned)
            {
                self.set_task(id, item);
            }
        }
        Some(self.snapshot())
    }

    fn snapshot(&self) -> JsValue {
        let mut item = JsObject::new();
        item.insert("type", text("todo"));
        item.insert(
            "items",
            JsValue::Array(
                self.tasks
                    .iter()
                    .map(|(_, task)| JsValue::Object(task.clone()))
                    .collect(),
            ),
        );
        JsValue::Object(item)
    }
}

/// `retainedTaskStatus(task)`.
fn retained_task_status(task: &JsObject) -> &'static str {
    match task.get("status").and_then(JsValue::as_str) {
        Some("completed") => "completed",
        Some("in_progress") => "in_progress",
        Some("pending") => "pending",
        _ if spocky_contracts::js::truthy(task.get("completed")) => "completed",
        _ => "pending",
    }
}
