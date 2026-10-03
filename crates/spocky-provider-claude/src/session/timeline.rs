//! Content blocks to timeline items: text, reasoning, tool calls and their
//! results, partial stream events, and the tool-use cache they share.

use spocky_contracts::js::strict_equals;
use spocky_contracts::js_value::{JsObject, JsValue, parse};
use spocky_contracts::text::js_trim;
use spocky_session::agent_sdk::AgentError;

use super::{ClaudeSession, ToolUseEntry, text};
use crate::partial_json::parse_partial_json_object;
use crate::project_dir::normalize_path;
use crate::provider_image::render_provider_image_output;
use crate::tool_call_mapper::{MapperParams, map_canceled, map_completed, map_failed, map_running};
use crate::transcript::{
    INTERRUPT_TOOL_USE_PLACEHOLDER, coerce_tool_result_content_to_string, is_transcript_noise_text,
    split_tool_result_images,
};

/// The options of `mapBlocksToTimeline`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BlockOptions {
    /// `textMessageType: "user_message"`.
    pub user_text: bool,
    pub suppress_assistant_text: bool,
    pub suppress_reasoning: bool,
}

/// `isClaudeContentChunk(value)`: an object with a string `type`.
pub(crate) fn is_content_chunk(value: &JsValue) -> bool {
    value.is_object() && value.get("type").is_some_and(JsValue::is_string)
}

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

fn item(kind: &str, body: &str) -> JsValue {
    let mut object = JsObject::new();
    object.insert("type", text(kind));
    object.insert("text", text(body));
    JsValue::Object(object)
}

/// `Object.keys(value)` for an object or array.
fn own_keys(value: &JsValue) -> Vec<String> {
    match value {
        JsValue::Object(object) => object.iter().map(|(key, _)| key.to_owned()).collect(),
        JsValue::Array(items) => (0..items.len()).map(|index| index.to_string()).collect(),
        _ => Vec::new(),
    }
}

/// `value[key]` for an object or array.
fn own_get<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    match value {
        JsValue::Object(object) => object.get(key),
        JsValue::Array(items) => key.parse::<usize>().ok().and_then(|index| items.get(index)),
        _ => None,
    }
}

/// `firstStringField(input, primary, secondary)`.
fn first_string_field(input: &JsValue, primary: &str, secondary: &str) -> JsValue {
    str_of(input, primary)
        .or_else(|| str_of(input, secondary))
        .map_or(JsValue::Undefined, text)
}

fn file_change(path: &str, kind: &str) -> (String, String) {
    (path.to_owned(), kind.to_owned())
}

impl ClaudeSession {
    pub(crate) fn cache_get(&self, id: &str) -> Option<ToolUseEntry> {
        self.tool_use_cache
            .borrow()
            .iter()
            .find(|(entry_id, _)| entry_id == id)
            .map(|(_, entry)| entry.clone())
    }

    /// `toolUseCache.set(id, entry)`: an existing key keeps its position.
    pub(crate) fn cache_set(&self, id: &str, entry: ToolUseEntry) {
        let mut cache = self.tool_use_cache.borrow_mut();
        match cache.iter_mut().find(|(entry_id, _)| entry_id == id) {
            Some(slot) => slot.1 = entry,
            None => cache.push((id.to_owned(), entry)),
        }
    }

    pub(crate) fn cache_delete(&self, id: &str) {
        self.tool_use_cache
            .borrow_mut()
            .retain(|(entry_id, _)| entry_id != id);
    }

    /// `enqueueTimeline(item)`.
    pub(crate) fn enqueue_timeline(&self, timeline_item: JsValue) {
        let mut event = JsObject::new();
        event.insert("type", text("timeline"));
        event.insert("item", timeline_item);
        event.insert("provider", text("claude"));
        self.push_event(JsValue::Object(event));
    }

    /// `pushToolCall(item, target?)`.
    fn push_tool_call(&self, tool_call: Option<JsValue>, target: Option<&mut Vec<JsValue>>) {
        let Some(tool_call) = tool_call else {
            return;
        };
        match target {
            Some(target) => target.push(tool_call),
            None => self.enqueue_timeline(tool_call),
        }
    }

    /// `flushPendingToolCalls()`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper.
    pub(crate) fn flush_pending_tool_calls(&self) -> Result<(), AgentError> {
        let entries: Vec<(String, ToolUseEntry)> = self.tool_use_cache.borrow().clone();
        for (id, entry) in &entries {
            if entry.started {
                let input = entry.input.clone().unwrap_or(JsValue::Null);
                let canceled = map_canceled(&MapperParams {
                    call_id: Some(id),
                    name: &entry.name,
                    input: Some(&input),
                    output: Some(&JsValue::Null),
                    metadata: None,
                })?;
                self.push_tool_call(canceled, None);
            }
        }
        self.tool_use_cache.borrow_mut().clear();
        self.state.borrow_mut().sidechain_tracker.clear();
        // The task protocol's routing table is session-scoped, so it is
        // deliberately not reset here: the turn ended, the session did not.
        let observations = self
            .task_protocol_source
            .borrow_mut()
            .cancel_running_foreground_tasks();
        for event in crate::subagents::observation::fold_subagent_observations(&observations) {
            self.push_event(super::options::provider_subagent(event));
        }
        Ok(())
    }

    /// `mapBlocksToTimeline(content, options)`; `content` is a string or an
    /// array of blocks.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper.
    pub(crate) fn map_blocks_to_timeline(
        &self,
        content: &JsValue,
        options: BlockOptions,
    ) -> Result<Vec<JsValue>, AgentError> {
        let kind = if options.user_text {
            "user_message"
        } else {
            "assistant_message"
        };
        let suppress_text = !options.user_text && options.suppress_assistant_text;
        if let JsValue::String(content) = content {
            if content.is_empty()
                || content == INTERRUPT_TOOL_USE_PLACEHOLDER
                || is_transcript_noise_text(content)
                || suppress_text
            {
                return Ok(Vec::new());
            }
            return Ok(vec![item(kind, content)]);
        }
        let Some(blocks) = content.as_array() else {
            return Err(AgentError {
                name: "TypeError".to_owned(),
                message: "content is not iterable".to_owned(),
            });
        };
        let mut items = Vec::new();
        let mut user_text_parts: Vec<String> = Vec::new();
        for block in blocks {
            if !is_content_chunk(block) {
                continue;
            }
            self.map_block_to_timeline(
                block,
                &mut items,
                &mut user_text_parts,
                options,
                suppress_text,
            )?;
        }
        if options.user_text && !user_text_parts.is_empty() {
            items.insert(0, item("user_message", &user_text_parts.join("\n\n")));
        }
        Ok(items)
    }

    fn map_block_to_timeline(
        &self,
        block: &JsValue,
        items: &mut Vec<JsValue>,
        user_text_parts: &mut Vec<String>,
        options: BlockOptions,
        suppress_text: bool,
    ) -> Result<(), AgentError> {
        match str_of(block, "type").unwrap_or_default() {
            "text" | "text_delta" => {
                let body = str_of(block, "text").unwrap_or_default();
                if body.is_empty()
                    || body == INTERRUPT_TOOL_USE_PLACEHOLDER
                    || is_transcript_noise_text(body)
                {
                    return Ok(());
                }
                if options.user_text {
                    let trimmed = js_trim(body);
                    if !trimmed.is_empty() {
                        user_text_parts.push(trimmed.to_owned());
                    }
                } else if !suppress_text {
                    items.push(item("assistant_message", body));
                }
            }
            "thinking" | "thinking_delta" => {
                if let Some(thinking) = str_of(block, "thinking")
                    && !thinking.is_empty()
                    && !options.suppress_reasoning
                {
                    items.push(item("reasoning", thinking));
                }
            }
            "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                self.handle_tool_use_start(block, items)?;
            }
            "tool_result"
            | "mcp_tool_result"
            | "web_fetch_tool_result"
            | "web_search_tool_result"
            | "code_execution_tool_result"
            | "bash_code_execution_tool_result"
            | "text_editor_code_execution_tool_result" => {
                self.handle_tool_result(block, items)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_tool_use_start(
        &self,
        block: &JsValue,
        items: &mut Vec<JsValue>,
    ) -> Result<(), AgentError> {
        let Some((id, mut entry)) = self.upsert_tool_use_entry(block) else {
            return Ok(());
        };
        if entry.started {
            return Ok(());
        }
        entry.started = true;
        self.cache_set(&id, entry.clone());
        let input = entry
            .input
            .clone()
            .or_else(|| normalize_tool_input(block.get("input")).cloned())
            .unwrap_or(JsValue::Null);
        let running = map_running(&MapperParams {
            call_id: Some(&id),
            name: &entry.name,
            input: Some(&input),
            output: Some(&JsValue::Null),
            metadata: None,
        })?;
        self.push_tool_call(running, Some(items));
        Ok(())
    }

    fn handle_tool_result(
        &self,
        block: &JsValue,
        items: &mut Vec<JsValue>,
    ) -> Result<(), AgentError> {
        let tool_use_id = str_of(block, "tool_use_id");
        let entry =
            tool_use_id.and_then(|id| self.cache_get(id).map(|entry| (id.to_owned(), entry)));
        let block_tool_name = str_of(block, "tool_name");
        let tool_name = entry
            .as_ref()
            .map(|(_, entry)| entry.name.as_str())
            .or(block_tool_name)
            .unwrap_or("tool")
            .to_owned();
        let call_id: Option<String> = match tool_use_id {
            Some(id) if !id.is_empty() => Some(id.to_owned()),
            _ => entry.as_ref().map(|(id, _)| id.clone()),
        };
        // Image blocks leave the result so base64 never reaches the tool
        // output; each renders as an assistant markdown image after the call.
        let (images, content) = split_tool_result_images(block.get("content"));
        let output = Self::build_tool_output(
            content.as_ref(),
            block,
            entry.as_ref().map(|(_, entry)| entry),
        );
        let input = entry
            .as_ref()
            .and_then(|(_, entry)| entry.input.clone())
            .unwrap_or(JsValue::Null);
        let output_value = output.unwrap_or(JsValue::Null);
        let params = MapperParams {
            call_id: call_id.as_deref(),
            name: &tool_name,
            input: Some(&input),
            output: Some(&output_value),
            metadata: None,
        };
        let tool_call = if spocky_contracts::js::truthy(block.get("is_error")) {
            let mut error = spocky_contracts::js::spread(Some(block));
            error.insert("content", content.clone().unwrap_or(JsValue::Undefined));
            map_failed(&params, Some(&JsValue::Object(error)))?
        } else {
            map_completed(&params)?
        };
        self.push_tool_call(tool_call, Some(items));
        for image in &images {
            if let Some(rendered) = render_provider_image_output(image) {
                items.push(rendered);
            }
        }
        if let Some(id) = tool_use_id {
            self.cache_delete(id);
        }
        Ok(())
    }

    /// `buildToolOutput(content, block, entry)`.
    fn build_tool_output(
        content: Option<&JsValue>,
        block: &JsValue,
        entry: Option<&ToolUseEntry>,
    ) -> Option<JsValue> {
        if spocky_contracts::js::truthy(block.get("is_error")) {
            return None;
        }
        let server = entry
            .map(|entry| entry.server.as_str())
            .or_else(|| str_of(block, "server"))
            .unwrap_or("tool");
        let tool = entry
            .map(|entry| entry.name.as_str())
            .or_else(|| str_of(block, "tool_name"))
            .unwrap_or("tool");
        let coerced = coerce_tool_result_content_to_string(content);
        let input = entry.and_then(|entry| entry.input.as_ref());
        if let Some(structured) = Self::build_structured_tool_result(server, tool, &coerced, input)
        {
            return Some(structured);
        }
        let mut result = JsObject::new();
        if !coerced.is_empty() {
            // A JSON string is parsed; anything else stays unchanged.
            result.insert(
                "output",
                parse(&coerced).unwrap_or_else(|_| JsValue::String(coerced.clone())),
            );
        }
        if let Some(files) = entry.and_then(|entry| entry.files.as_ref())
            && !files.is_empty()
        {
            result.insert("files", files_value(files));
        }
        (!result.is_empty()).then_some(JsValue::Object(result))
    }

    fn is_command_execution_tool(server: &str, tool: &str, input: Option<&JsValue>) -> bool {
        let named = |value: &str| {
            value.contains("bash") || value.contains("shell") || value.contains("command")
        };
        if named(server) || named(tool) {
            return true;
        }
        input.is_some_and(|input| {
            input
                .get("command")
                .is_some_and(|command| command.is_string() || matches!(command, JsValue::Array(_)))
        })
    }

    fn is_file_write_tool(tool: &str) -> bool {
        tool.contains("write") || tool == "write_file" || tool == "create_file"
    }

    fn is_file_edit_tool(tool: &str) -> bool {
        tool.contains("edit")
            || tool.contains("patch")
            || tool == "apply_patch"
            || tool == "apply_diff"
    }

    fn is_file_read_tool(tool: &str) -> bool {
        tool.contains("read") || tool == "read_file" || tool == "view_file"
    }

    /// `buildStructuredToolResult(server, tool, output, input)`.
    fn build_structured_tool_result(
        server: &str,
        tool: &str,
        output: &str,
        input: Option<&JsValue>,
    ) -> Option<JsValue> {
        let server = server.to_lowercase();
        let tool = tool.to_lowercase();
        let mut result = JsObject::new();
        if Self::is_command_execution_tool(&server, &tool, input) {
            let empty = JsValue::Object(JsObject::new());
            let command = extract_command_text(input.unwrap_or(&empty))
                .unwrap_or_else(|| "command".to_owned());
            result.insert("type", text("command"));
            result.insert("command", JsValue::String(command));
            result.insert("output", text(output));
            result.insert(
                "cwd",
                input
                    .and_then(|input| str_of(input, "cwd"))
                    .map_or(JsValue::Undefined, text),
            );
            return Some(JsValue::Object(result));
        }
        let input = input?;
        let file_path = str_of(input, "file_path")?;
        if Self::is_file_write_tool(&tool) {
            result.insert("type", text("file_write"));
            result.insert("filePath", text(file_path));
            result.insert("oldContent", text(""));
            result.insert(
                "newContent",
                text(str_of(input, "content").unwrap_or(output)),
            );
            return Some(JsValue::Object(result));
        }
        if Self::is_file_edit_tool(&tool) {
            result.insert("type", text("file_edit"));
            result.insert("filePath", text(file_path));
            result.insert("diff", first_string_field(input, "patch", "diff"));
            result.insert(
                "oldContent",
                first_string_field(input, "old_str", "old_string"),
            );
            result.insert(
                "newContent",
                first_string_field(input, "new_str", "new_string"),
            );
            return Some(JsValue::Object(result));
        }
        if Self::is_file_read_tool(&tool) {
            result.insert("type", text("file_read"));
            result.insert("filePath", text(file_path));
            result.insert("content", text(output));
            return Some(JsValue::Object(result));
        }
        None
    }

    /// `updatePartialEventToolState(event)`: `true` when the event was an
    /// `input_json_delta` the session consumed.
    fn update_partial_event_tool_state(&self, event: &JsValue) -> Result<bool, AgentError> {
        let index = event.get("index").and_then(JsValue::as_f64);
        match str_of(event, "type").unwrap_or_default() {
            "content_block_start" => {
                let block = event
                    .get("content_block")
                    .filter(|block| is_content_chunk(block));
                if let (Some(block), Some(index)) = (block, index)
                    && str_of(block, "type") == Some("tool_use")
                    && let Some(id) = str_of(block, "id")
                {
                    let mut state = self.state.borrow_mut();
                    state
                        .tool_use_index_to_id
                        .insert(index_key(index), id.to_owned());
                    state.tool_use_input_buffers.remove(id);
                }
                Ok(false)
            }
            "content_block_delta" => {
                let delta = event.get("delta").filter(|delta| is_content_chunk(delta));
                if let Some(delta) = delta
                    && str_of(delta, "type") == Some("input_json_delta")
                {
                    self.handle_tool_input_delta(index, str_of(delta, "partial_json"))?;
                    return Ok(true);
                }
                Ok(false)
            }
            "content_block_stop" => {
                if let Some(index) = index {
                    let mut state = self.state.borrow_mut();
                    if let Some(tool_id) = state.tool_use_index_to_id.remove(&index_key(index)) {
                        state.tool_use_input_buffers.remove(&tool_id);
                    }
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// `mapPartialEvent(event, options)`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper.
    pub(crate) fn map_partial_event(
        &self,
        event: &JsValue,
        options: BlockOptions,
    ) -> Result<Vec<JsValue>, AgentError> {
        if self.update_partial_event_tool_state(event)? {
            return Ok(Vec::new());
        }
        let key = match str_of(event, "type").unwrap_or_default() {
            "content_block_start" => "content_block",
            "content_block_delta" => "delta",
            _ => return Ok(Vec::new()),
        };
        match event.get(key).filter(|block| is_content_chunk(block)) {
            Some(block) => self.map_blocks_to_timeline(
                &JsValue::Array(vec![block.clone()]),
                BlockOptions {
                    user_text: false,
                    ..options
                },
            ),
            None => Ok(Vec::new()),
        }
    }

    /// `upsertToolUseEntry(block)`: the id and the cached entry.
    fn upsert_tool_use_entry(&self, block: &JsValue) -> Option<(String, ToolUseEntry)> {
        let id = str_of(block, "id").filter(|id| !id.is_empty())?.to_owned();
        let mut entry = self
            .cache_get(&id)
            .unwrap_or_else(|| default_tool_use_entry(block));
        if let Some(name) = str_of(block, "name").filter(|name| !name.is_empty()) {
            name.clone_into(&mut entry.name);
        }
        if let Some(server) = str_of(block, "server").filter(|server| !server.is_empty()) {
            server.clone_into(&mut entry.server);
        } else if entry.server.is_empty() {
            entry.server.clone_from(&entry.name);
        }
        if matches!(
            str_of(block, "type"),
            Some("tool_use" | "mcp_tool_use" | "server_tool_use")
        ) && let Some(input) = normalize_tool_input(block.get("input"))
        {
            self.apply_tool_input(&mut entry, input);
        }
        self.cache_set(&id, entry.clone());
        Some((id, entry))
    }

    /// `handleToolInputDelta(index, partialJson)`.
    fn handle_tool_input_delta(
        &self,
        index: Option<f64>,
        partial_json: Option<&str>,
    ) -> Result<(), AgentError> {
        let (Some(index), Some(partial_json)) = (index, partial_json) else {
            return Ok(());
        };
        let tool_id = {
            let state = self.state.borrow();
            state.tool_use_index_to_id.get(&index_key(index)).cloned()
        };
        let Some(tool_id) = tool_id else {
            return Ok(());
        };
        let buffer = {
            let mut state = self.state.borrow_mut();
            let buffer = state
                .tool_use_input_buffers
                .entry(tool_id.clone())
                .or_default();
            buffer.push_str(partial_json);
            buffer.clone()
        };
        let entry = self.cache_get(&tool_id);
        let parsed = parse_partial_json_object(&buffer);
        let (Some(mut entry), Some(parsed)) = (entry, parsed) else {
            return Ok(());
        };
        let normalized = JsValue::Object(parsed.value);
        if !parsed.complete && own_keys(&normalized).is_empty() {
            return Ok(());
        }
        if are_tool_inputs_equal(entry.input.as_ref(), &normalized) {
            return Ok(());
        }
        self.apply_tool_input(&mut entry, &normalized);
        self.cache_set(&tool_id, entry.clone());
        let running = map_running(&MapperParams {
            call_id: Some(&tool_id),
            name: &entry.name,
            input: Some(&normalized),
            output: Some(&JsValue::Null),
            metadata: None,
        })?;
        self.push_tool_call(running, None);
        Ok(())
    }

    /// `applyToolInput(entry, input)`.
    fn apply_tool_input(&self, entry: &mut ToolUseEntry, input: &JsValue) {
        entry.input = Some(input.clone());
        if is_command_tool(&entry.name, input) {
            entry.classification = "command";
            if let Some(command) = extract_command_text(input) {
                entry.command_text = Some(command);
            }
        } else if let Some(files) = self.extract_file_changes(input)
            && !files.is_empty()
        {
            entry.classification = "file_change";
            entry.files = Some(files);
        }
    }

    /// `extractFileChanges(input)`.
    fn extract_file_changes(&self, input: &JsValue) -> Option<Vec<(String, String)>> {
        if let Some(path) = str_of(input, "file_path").filter(|path| !path.is_empty()) {
            let relative = self.relativize_path(path);
            if !relative.is_empty() {
                return Some(vec![file_change(&relative, &detect_file_kind(path))]);
            }
        }
        if let Some(patch) = str_of(input, "patch").filter(|patch| !patch.is_empty()) {
            let files = parse_patch_file_list(patch);
            if !files.is_empty() {
                return Some(
                    files
                        .iter()
                        .map(|(path, kind)| {
                            let relative = self.relativize_path(path);
                            file_change(&relative, kind)
                        })
                        .collect(),
                );
            }
        }
        if let Some(JsValue::Array(values)) = input.get("files") {
            let files: Vec<(String, String)> = values
                .iter()
                .filter_map(JsValue::as_str)
                .filter(|path| !path.is_empty())
                .map(|path| {
                    let relative = self.relativize_path(path);
                    file_change(&relative, &detect_file_kind(path))
                })
                .collect();
            if !files.is_empty() {
                return Some(files);
            }
        }
        None
    }

    /// `relativizePath(target)` for a non-empty target.
    fn relativize_path(&self, target: &str) -> String {
        let cwd = self.config_str("cwd").filter(|cwd| !cwd.is_empty());
        match cwd {
            Some(cwd) if target.starts_with(&cwd) => {
                let relative = posix_relative(&cwd, target);
                if relative.is_empty() {
                    posix_basename(target)
                } else {
                    relative
                }
            }
            _ => target.to_owned(),
        }
    }
}

/// Map keys of `toolUseIndexToId` (a JS number key).
fn index_key(index: f64) -> String {
    spocky_contracts::js::js_string(Some(&JsValue::Number(index)))
}

fn default_tool_use_entry(block: &JsValue) -> ToolUseEntry {
    let name = str_of(block, "name").filter(|name| !name.is_empty());
    let server = str_of(block, "server")
        .filter(|server| !server.is_empty())
        .or(name)
        .unwrap_or("tool");
    ToolUseEntry {
        name: name.unwrap_or("tool").to_owned(),
        server: server.to_owned(),
        classification: "generic",
        started: false,
        command_text: None,
        files: None,
        input: None,
    }
}

/// `normalizeToolInput(input)`: an object or array.
fn normalize_tool_input(input: Option<&JsValue>) -> Option<&JsValue> {
    input.filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))
}

/// `areToolInputsEqual(left, right)`: same keys, values strictly equal.
fn are_tool_inputs_equal(left: Option<&JsValue>, right: &JsValue) -> bool {
    let Some(left) = left else {
        return false;
    };
    let left_keys = own_keys(left);
    let right_keys = own_keys(right);
    if left_keys.len() != right_keys.len() {
        return false;
    }
    right_keys
        .iter()
        .all(|key| strict_equals(own_get(left, key), own_get(right, key)))
}

/// `isCommandTool(name, input)`.
fn is_command_tool(name: &str, input: &JsValue) -> bool {
    let name = name.to_lowercase();
    if name.contains("bash")
        || name.contains("shell")
        || name.contains("terminal")
        || name.contains("command")
    {
        return true;
    }
    input
        .get("command")
        .is_some_and(|command| command.is_string() || matches!(command, JsValue::Array(_)))
}

/// `extractCommandText(input)`.
fn extract_command_text(input: &JsValue) -> Option<String> {
    match input.get("command") {
        Some(JsValue::String(command)) if !command.is_empty() => return Some(command.clone()),
        Some(JsValue::Array(tokens)) => {
            let tokens: Vec<&str> = tokens.iter().filter_map(JsValue::as_str).collect();
            if !tokens.is_empty() {
                return Some(tokens.join(" "));
            }
        }
        _ => {}
    }
    str_of(input, "description")
        .filter(|description| !description.is_empty())
        .map(str::to_owned)
}

/// `fs.existsSync(path) ? "update" : "add"`.
fn detect_file_kind(path: &str) -> String {
    if std::path::Path::new(path).exists() {
        "update"
    } else {
        "add"
    }
    .to_owned()
}

/// `parsePatchFileList(patch)`: unique `(path, kind)` per patch header.
fn parse_patch_file_list(patch: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for line in split_lines(patch) {
        let trimmed = js_trim(line);
        let header = [
            ("*** Add File:", "add"),
            ("*** Delete File:", "delete"),
            ("*** Update File:", "update"),
        ]
        .into_iter()
        .find(|(prefix, _)| trimmed.starts_with(prefix));
        let Some((prefix, kind)) = header else {
            continue;
        };
        let file_path = js_trim(&trimmed.replacen(prefix, "", 1)).to_owned();
        let key = format!("{kind}:{file_path}");
        if !file_path.is_empty() && !seen.contains(&key) {
            seen.push(key);
            files.push((file_path, kind.to_owned()));
        }
    }
    files
}

/// `text.split(/\r?\n/)`.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// `path.posix.relative(from, to)`.
fn posix_relative(from: &str, to: &str) -> String {
    let resolve = |path: &str| {
        let absolute = if path.starts_with('/') {
            path.to_owned()
        } else {
            let cwd = std::env::current_dir()
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{cwd}/{path}")
        };
        normalize_path(&absolute)
    };
    let from = resolve(from);
    let to = resolve(to);
    let from_parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|part| !part.is_empty()).collect();
    let common = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut segments: Vec<&str> = vec![".."; from_parts.len() - common];
    segments.extend(&to_parts[common..]);
    segments.join("/")
}

/// `path.basename(target)`.
fn posix_basename(target: &str) -> String {
    target
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `{ path, kind }` objects, as `toolUseCache` files serialize.
fn files_value(files: &[(String, String)]) -> JsValue {
    JsValue::Array(
        files
            .iter()
            .map(|(path, kind)| {
                let mut object = JsObject::new();
                object.insert("path", text(path));
                object.insert("kind", text(kind));
                JsValue::Object(object)
            })
            .collect(),
    )
}
