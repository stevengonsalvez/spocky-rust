//! `buildOptions`, `ensureQuery`, and `query.ts`'s spawn override.

use std::rc::{Rc, Weak};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::js_trim;
use spocky_session::agent_sdk::AgentError;

use super::{
    ClaudeSession, assert_mode_can_run, assert_thinking_option_supported, is_thinking_effort, text,
};
use crate::launch::{external_process_env, resolve_spawn_command, spawn_env};
use crate::local::LocalBoxFuture;
use crate::model_manifest::{
    CLAUDE_DISABLED_THINKING_OPTION_ID, CLAUDE_ULTRACODE_THINKING_OPTION_ID,
};
use crate::process::{ChildProcess, SpawnFailure, SpawnRequest, terminate_with_tree_kill};
use crate::sdk_query::{
    CanUseTool, ClaudeOptions, ClaudeQuery, HookCallback, ProcessQuery, PromptInput, QueryInput,
    SpawnClaudeCodeProcess,
};
use crate::subagents::observation::fold_subagent_observations;
use crate::transcript::to_claude_sdk_mcp_config;

/// `CLAUDE_SETTING_SOURCES`.
const CLAUDE_SETTING_SOURCES: [&str; 3] = ["user", "project", "local"];
const MAX_RECENT_STDERR_CHARS: usize = 4000;

/// `composeSystemPromptParts(...parts)`.
fn compose_system_prompt_parts(parts: &[Option<&str>]) -> Option<String> {
    let prompt = parts
        .iter()
        .filter_map(|part| part.map(js_trim))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!prompt.is_empty()).then_some(prompt)
}

/// `applyClaudeToolPolicy(options, toolPolicy)`.
fn apply_tool_policy(options: &JsValue, tool_policy: Option<&JsValue>) -> JsObject {
    let mut applied = spocky_contracts::js::spread(Some(options));
    let Some(policy) = tool_policy.filter(|policy| spocky_contracts::js::truthy(Some(policy)))
    else {
        return applied;
    };
    let mut tools: Vec<String> = options
        .get("allowedTools")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsValue::as_str)
        .map(str::to_owned)
        .collect();
    for grant in policy
        .get("preapproved")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        tools.push(format!(
            "mcp__{}__{}",
            spocky_contracts::js::js_string(grant.get("server")),
            spocky_contracts::js::js_string(grant.get("tool"))
        ));
    }
    let mut unique: Vec<String> = Vec::new();
    for tool in tools {
        if !unique.contains(&tool) {
            unique.push(tool);
        }
    }
    applied.insert(
        "allowedTools",
        JsValue::Array(unique.into_iter().map(JsValue::String).collect()),
    );
    applied
}

impl ClaudeSession {
    /// `resolveThinkingConfig()`: `(thinking, effort, ultracode)`.
    pub(crate) fn resolve_thinking_config(
        &self,
    ) -> Result<(Option<JsValue>, Option<String>, bool), AgentError> {
        let option = self
            .config_str("thinkingOptionId")
            .filter(|option| !option.is_empty() && option != "default");
        assert_thinking_option_supported(self.config_str("model").as_deref(), option.as_deref())?;
        let thinking = |kind: &str| {
            let mut thinking = JsObject::new();
            thinking.insert("type", text(kind));
            Some(JsValue::Object(thinking))
        };
        Ok(match option.as_deref() {
            Some(CLAUDE_DISABLED_THINKING_OPTION_ID) => (thinking("disabled"), None, false),
            Some(CLAUDE_ULTRACODE_THINKING_OPTION_ID) => {
                (thinking("adaptive"), Some("xhigh".to_owned()), true)
            }
            Some(effort) if is_thinking_effort(Some(effort)) => {
                (thinking("adaptive"), Some(effort.to_owned()), false)
            }
            _ => (None, None, false),
        })
    }

    /// `buildSettingsOptions(providerOptions, { ultracode })`.
    fn build_settings(&self, provider_options: &JsObject, ultracode: bool) -> Option<JsValue> {
        let fast_mode = self.resolve_fast_mode_setting();
        if fast_mode.is_none() && !ultracode {
            return None;
        }
        let mut updates = JsObject::new();
        if let Some(fast_mode) = fast_mode {
            updates.insert("fastMode", JsValue::Bool(fast_mode));
        }
        if ultracode {
            updates.insert("ultracode", JsValue::Bool(true));
        }
        let settings = provider_options.get("settings");
        Some(match settings {
            Some(settings)
                if spocky_contracts::js::truthy(Some(settings)) && !settings.is_string() =>
            {
                let mut merged = spocky_contracts::js::spread(Some(settings));
                for (key, value) in updates.iter() {
                    merged.insert(key, value.clone());
                }
                JsValue::Object(merged)
            }
            Some(settings) if !matches!(settings, JsValue::Undefined | JsValue::Null) => {
                settings.clone()
            }
            _ => JsValue::Object(updates),
        })
    }

    /// `buildOptions()`.
    #[allow(clippy::too_many_lines)] // The baseline's option object.
    pub(crate) async fn build_options(self: &Rc<Self>) -> Result<ClaudeOptions, AgentError> {
        let (thinking, effort, ultracode) = self.resolve_thinking_config()?;
        let (system_prompt, daemon_prompt, provider_options, tool_policy) = {
            let state = self.state.borrow();
            let read = |key: &str| {
                state
                    .config
                    .get(key)
                    .and_then(JsValue::as_str)
                    .map(str::to_owned)
            };
            (
                read("systemPrompt"),
                read("daemonAppendSystemPrompt"),
                state
                    .config
                    .get("providerOptions")
                    .cloned()
                    .unwrap_or(JsValue::Object(JsObject::new())),
                state.config.get("toolPolicy").cloned(),
            )
        };
        let appended =
            compose_system_prompt_parts(&[system_prompt.as_deref(), daemon_prompt.as_deref()])
                .unwrap_or_default();
        let provider_options = apply_tool_policy(&provider_options, tool_policy.as_ref());
        let settings = self.build_settings(&provider_options, ultracode);
        let sdk_env = self.build_sdk_env();
        let current_mode = self.state.borrow().current_mode.clone();
        assert_mode_can_run(current_mode.as_deref().unwrap_or_default(), &sdk_env)?;
        let binary = (self.options.resolve_binary)().await?;
        let (cwd, pending_fresh, session_id) = {
            let state = self.state.borrow();
            (
                state
                    .config
                    .get("cwd")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
                state.pending_fresh_session_id.clone(),
                state.claude_session_id.clone(),
            )
        };
        let mut data = JsObject::new();
        data.insert("cwd", cwd);
        data.insert("includePartialMessages", JsValue::Bool(true));
        data.insert(
            "permissionMode",
            current_mode.map_or(JsValue::Undefined, JsValue::String),
        );
        data.insert("allowDangerouslySkipPermissions", JsValue::Bool(true));
        data.insert(
            "agents",
            self.options
                .defaults_agents
                .clone()
                .unwrap_or(JsValue::Undefined),
        );
        data.insert("canUseTool", JsValue::Undefined);
        data.insert("pathToClaudeCodeExecutable", JsValue::String(binary));
        let mut prompt = JsObject::new();
        prompt.insert("type", text("preset"));
        prompt.insert("preset", text("claude_code"));
        prompt.insert("append", JsValue::String(appended));
        data.insert("systemPrompt", JsValue::Object(prompt));
        data.insert(
            "settingSources",
            JsValue::Array(
                CLAUDE_SETTING_SOURCES
                    .iter()
                    .map(|source| text(source))
                    .collect(),
            ),
        );
        data.insert("stderr", JsValue::Undefined);
        data.insert("enableFileCheckpointing", JsValue::Bool(true));
        if let Some(fresh) = &pending_fresh {
            data.insert("sessionId", text(fresh));
        } else if let Some(id) = &session_id {
            data.insert("resume", text(id));
        }
        if let Some(thinking) = thinking {
            data.insert("thinking", thinking);
        }
        if let Some(effort) = effort {
            data.insert("effort", JsValue::String(effort));
        }
        for (key, value) in provider_options.iter() {
            data.insert(key, value.clone());
        }
        if let Some(settings) = settings {
            data.insert("settings", settings);
        }
        data.insert("forwardSubagentText", JsValue::Bool(true));
        let mut hooks_shape = JsObject::new();
        for event in ["PreToolUse", "PostToolUse", "SubagentStop"] {
            let mut entry = JsObject::new();
            entry.insert("hooks", JsValue::Array(vec![JsValue::Undefined]));
            hooks_shape.insert(event, JsValue::Array(vec![JsValue::Object(entry)]));
        }
        data.insert("hooks", JsValue::Object(hooks_shape));
        if let Some(persist) = self.options.persist_session {
            data.insert("persistSession", JsValue::Bool(persist));
        }
        data.insert("env", JsValue::Object(sdk_env));
        let mcp_servers = self.state.borrow().config.get("mcpServers").cloned();
        if let Some(servers) =
            mcp_servers.filter(|servers| spocky_contracts::js::truthy(Some(servers)))
        {
            let mut normalized = JsObject::new();
            for (name, config) in servers.as_object().into_iter().flat_map(JsObject::iter) {
                normalized.insert(name, to_claude_sdk_mcp_config(config));
            }
            data.insert("mcpServers", JsValue::Object(normalized));
        }
        if let Some(model) = self.config_str("model").filter(|model| !model.is_empty()) {
            data.insert("model", JsValue::String(model));
        }
        self.state.borrow_mut().last_options_model = data
            .get("model")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        if let (Some(id), None) = (&session_id, &pending_fresh) {
            data.insert("resume", text(id));
        }
        if let Some(disallowed) = self
            .options
            .runtime_settings
            .as_ref()
            .and_then(|settings| settings.disallowed_tools.as_ref())
            .filter(|tools| !tools.is_empty())
        {
            let mut tools: Vec<JsValue> = data
                .get("disallowedTools")
                .and_then(JsValue::as_array)
                .map(<[JsValue]>::to_vec)
                .unwrap_or_default();
            tools.extend(disallowed.iter().map(|tool| text(tool)));
            data.insert("disallowedTools", JsValue::Array(tools));
        }
        Ok(ClaudeOptions {
            data,
            can_use_tool: Some(self.can_use_tool_callback()),
            hooks: self.subagent_effort_hooks(),
            stderr: Some(self.stderr_callback()),
            spawn: None,
        })
    }

    fn can_use_tool_callback(self: &Rc<Self>) -> CanUseTool {
        let weak = Rc::downgrade(self);
        Rc::new(move |tool_name, input, options| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(session) = weak.upgrade() else {
                    return Err(AgentError::new("Claude session closed"));
                };
                session
                    .handle_permission_request(tool_name, input, options)
                    .await
            })
        })
    }

    /// `buildSubagentEffortHooks()`.
    fn subagent_effort_hooks(self: &Rc<Self>) -> Vec<(String, Vec<HookCallback>)> {
        let weak = Rc::downgrade(self);
        let observe: HookCallback = Rc::new(move |input, _tool_use_id| {
            if let Some(session) = weak.upgrade() {
                let observations = session
                    .task_protocol_source
                    .borrow_mut()
                    .observe_hook(&input);
                for event in fold_subagent_observations(&observations) {
                    session.notify_subscribers(provider_subagent(event));
                }
            }
            Box::pin(async { Ok(JsValue::Object(JsObject::new())) })
        });
        ["PreToolUse", "PostToolUse", "SubagentStop"]
            .iter()
            .map(|event| ((*event).to_owned(), vec![Rc::clone(&observe)]))
            .collect()
    }

    fn stderr_callback(self: &Rc<Self>) -> Rc<dyn Fn(String)> {
        let weak = Rc::downgrade(self);
        Rc::new(move |data| {
            if let Some(session) = weak.upgrade() {
                session.capture_stderr(&data);
            }
        })
    }

    /// `captureStderr(data)`.
    pub(crate) fn capture_stderr(&self, data: &str) {
        let trimmed = js_trim(data);
        if trimmed.is_empty() {
            return;
        }
        let mut state = self.state.borrow_mut();
        let combined = if state.recent_stderr.is_empty() {
            trimmed.to_owned()
        } else {
            format!("{}\n{trimmed}", state.recent_stderr)
        };
        let units: Vec<u16> = spocky_contracts::js_value::js_text_utf16(&combined).collect();
        state.recent_stderr = if units.len() > MAX_RECENT_STDERR_CHARS {
            spocky_contracts::js_value::js_text_from_utf16(
                &units[units.len() - MAX_RECENT_STDERR_CHARS..],
            )
        } else {
            combined
        };
    }

    /// The `spawnClaudeCodeProcess` override from `query.ts`.
    fn spawn_override(
        self: &Rc<Self>,
        stderr: Option<Rc<dyn Fn(String)>>,
    ) -> SpawnClaudeCodeProcess {
        let weak: Weak<Self> = Rc::downgrade(self);
        let settings = self.options.runtime_settings.clone();
        let launch_env = self.options.launch_env.clone();
        let process_env = Rc::clone(&self.options.process_env);
        Rc::new(
            move |request: SpawnRequest| -> Result<Rc<ChildProcess>, SpawnFailure> {
                let (command, args) =
                    resolve_spawn_command(&request.command, &request.args, settings.as_ref());
                let default_runtime = command == "node" || command == "bun";
                let spawn_request = if default_runtime {
                    // `buildSelfNodeCommand`: the daemon's own Node runs the SDK's
                    // script. A Rust daemon has none, so `node` is resolved from
                    // PATH (a documented divergence).
                    let provider = spawn_env(&request.env, settings.as_ref(), launch_env.as_ref());
                    let mut env = external_process_env(&process_env(), &[]);
                    env.insert("ELECTRON_RUN_AS_NODE", text("1"));
                    for (key, value) in provider.iter() {
                        env.insert(key, value.clone());
                    }
                    SpawnRequest {
                        command: "node".to_owned(),
                        args,
                        cwd: request.cwd.clone(),
                        env,
                    }
                } else {
                    SpawnRequest {
                        command,
                        args,
                        cwd: request.cwd.clone(),
                        env: spawn_env(&request.env, settings.as_ref(), launch_env.as_ref()),
                    }
                };
                let child = ChildProcess::spawn(&spawn_request, stderr.clone())?;
                if let Some(session) = weak.upgrade() {
                    session.on_child_process(&child);
                }
                Ok(child)
            },
        )
    }

    /// `onChildProcess(child)`: records the child and watches its exit.
    fn on_child_process(self: &Rc<Self>, child: &Rc<ChildProcess>) {
        self.state.borrow_mut().child_process = Some(Rc::clone(child));
        let weak = Rc::downgrade(self);
        let watched = Rc::clone(child);
        tokio::task::spawn_local(async move {
            let exit = watched.wait_exit().await;
            if let Some(session) = weak.upgrade() {
                session.handle_runtime_exit(&watched, &exit);
            }
        });
    }

    /// `ensureQuery()`.
    pub(crate) fn ensure_query(
        self: &Rc<Self>,
    ) -> LocalBoxFuture<'_, Result<Rc<dyn ClaudeQuery>, AgentError>> {
        Box::pin(async move {
            let restart = {
                let state = self.state.borrow();
                if let Some(query) = &state.query
                    && !state.query_restart_needed
                {
                    return Ok(Rc::clone(query));
                }
                state.query_restart_needed && state.query.is_some()
            };
            if restart {
                let (old_query, old_input, retired) = {
                    let mut state = self.state.borrow_mut();
                    let old_query = state.query.take();
                    let old_input = state.input.take();
                    state.query_pump_running = false;
                    state.query_restart_needed = false;
                    (old_query, old_input, state.child_process.take())
                };
                if retired.is_some() {
                    self.fail_running_runtime_tasks();
                }
                if let Some(input) = old_input {
                    input.end();
                }
                if let Some(query) = old_query {
                    query.close();
                    query.return_().await;
                }
                if let Some(child) = retired {
                    terminate_with_tree_kill(
                        &child,
                        Duration::from_millis(2000),
                        Duration::from_millis(2000),
                    )
                    .await;
                }
            }
            self.state.borrow_mut().persistence = None;
            let input = Rc::new(PromptInput::default());
            let mut options = self.build_options().await?;
            let stderr = options.stderr.clone();
            options.spawn = Some(self.spawn_override(stderr));
            self.state.borrow_mut().input = Some(Rc::clone(&input));
            let query_input = QueryInput {
                prompt: Rc::clone(&input),
                options,
            };
            let query: Rc<dyn ClaudeQuery> = match &self.options.query_factory {
                Some(factory) => factory(query_input)?,
                None => Rc::new(ProcessQuery::start(
                    query_input,
                    &(self.options.process_env)(),
                )?),
            };
            self.state.borrow_mut().query = Some(Rc::clone(&query));
            if let Some(fast_mode) = self.resolve_fast_mode_setting() {
                let mut settings = JsObject::new();
                settings.insert("fastMode", JsValue::Bool(fast_mode));
                query.apply_flag_settings(JsValue::Object(settings)).await?;
            }
            let current = self.state.borrow().query.clone();
            Ok(current.unwrap_or(query))
        })
    }
}

/// A `provider_subagent` stream event.
pub(crate) fn provider_subagent(event: JsValue) -> JsValue {
    let mut wrapped = JsObject::new();
    wrapped.insert("type", text("provider_subagent"));
    wrapped.insert("provider", text("claude"));
    wrapped.insert("event", event);
    JsValue::Object(wrapped)
}
