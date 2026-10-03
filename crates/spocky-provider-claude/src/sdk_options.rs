//! The Claude Agent SDK 0.3.246 `query()` option handling Paseo relies on:
//! `sD` (env, thinking, init config) and `ProcessTransport.initialize`
//! (the spawn command and argv), over the options object Paseo builds.
//!
//! Options are a [`JsObject`] with the baseline's keys and order. Function
//! members (`canUseTool`, `stderr`, `spawnClaudeCodeProcess`, hook
//! callbacks) are held as `undefined` slots, which is how
//! `JSON.stringify(options)` writes them; [`SdkCallbacks`] says which are
//! present.
//!
//! Not reproduced: the SDK sets `process.env.CLAUDE_AGENT_SDK_VERSION` on
//! the daemon itself (Rust 2024 forbids an unsynchronized `set_var` under
//! `forbid(unsafe_code)`), and `canUseTool` shadowing warnings it writes to
//! the daemon's stderr with `process.emitWarning`.

use spocky_contracts::js::{js_string, spread, truthy};
use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_session::agent_sdk::AgentError;

/// The SDK version the pinned Paseo build bundles.
pub const SDK_VERSION: &str = "0.3.246";

/// Which function members the options carry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SdkCallbacks {
    pub can_use_tool: bool,
    pub stderr: bool,
    pub spawn_override: bool,
}

/// What `query(options)` spawns and sends first.
#[derive(Debug, Clone, PartialEq)]
pub struct SdkLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: JsObject,
    /// The `initialize` control request body, without `hooks`.
    pub init: JsObject,
    /// `(event, matcher, timeout, callback count)` per hook matcher, in
    /// option order.
    pub hook_matchers: Vec<(String, JsValue, JsValue, usize)>,
}

fn get<'a>(options: &'a JsObject, key: &str) -> Option<&'a JsValue> {
    options
        .get(key)
        .filter(|value| !matches!(value, JsValue::Undefined))
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `pushFlag(args, key, value)` (`BT`): `--key=value` when the value looks
/// like a flag, else `--key value`.
fn push_flag(args: &mut Vec<String>, key: &str, value: &JsValue) {
    let value = js_string(Some(value));
    if value.chars().count() > 1 && value.starts_with('-') {
        args.push(format!("--{key}={value}"));
    } else {
        args.push(format!("--{key}"));
        args.push(value);
    }
}

fn array_len(value: Option<&JsValue>) -> usize {
    value
        .and_then(JsValue::as_array)
        .map_or(0, <[JsValue]>::len)
}

/// `isEnvTruthy(value)` (`Ie`).
fn env_truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => false,
        Some(JsValue::Bool(flag)) => *flag,
        Some(other) if !truthy(Some(other)) => false,
        Some(other) => {
            let normalized = js_string(Some(other)).to_lowercase();
            matches!(
                spocky_contracts::text::js_trim(&normalized),
                "1" | "true" | "yes" | "on"
            )
        }
    }
}

/// `{ ...extraArgs, settings }` merged with the sandbox option (`u2`).
fn merge_extra_args(extra: &JsObject, sandbox: Option<&JsValue>) -> Result<JsObject, AgentError> {
    let mut merged = extra.clone();
    let Some(sandbox) = sandbox.filter(|sandbox| truthy(Some(sandbox))) else {
        return Ok(merged);
    };
    let sandbox = match sandbox.as_object() {
        Some(record)
            if record.get("enabled") == Some(&JsValue::Bool(true))
                && record
                    .get("failIfUnavailable")
                    .is_none_or(|value| matches!(value, JsValue::Undefined)) =>
        {
            let mut patched = record.clone();
            patched.insert("failIfUnavailable", JsValue::Bool(true));
            JsValue::Object(patched)
        }
        _ => sandbox.clone(),
    };
    let settings = merged
        .get("settings")
        .filter(|settings| truthy(Some(settings)))
        .cloned();
    if let Some(settings) = &settings {
        let raw = js_string(Some(settings));
        let trimmed = spocky_contracts::text::js_trim(&raw);
        if !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
            return Err(AgentError::new(
                "Cannot use both a settings file path and the sandbox option. Include the sandbox configuration in your settings file instead.",
            ));
        }
    }
    let mut combined = JsObject::new();
    combined.insert("sandbox", sandbox.clone());
    if let Some(settings) = settings
        && let Ok(parsed) = spocky_contracts::js_value::parse(&js_string(Some(&settings)))
    {
        combined = spread(Some(&parsed));
        combined.insert("sandbox", sandbox);
    }
    merged.insert(
        "settings",
        JsValue::String(stringify(&JsValue::Object(combined))),
    );
    Ok(merged)
}

/// `isNativeBinary(path)` (`eMe`).
fn is_native_binary(path: &str) -> bool {
    ![".js", ".mjs", ".tsx", ".ts", ".jsx"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

/// The thinking config `sD` derives from `thinking` or `maxThinkingTokens`.
fn thinking_config(options: &JsObject) -> Option<JsObject> {
    if let Some(thinking) = get(options, "thinking").filter(|value| truthy(Some(value))) {
        let mut config = JsObject::new();
        match thinking.get("type").and_then(JsValue::as_str) {
            Some("adaptive") => {
                config.insert("type", text("adaptive"));
                config.insert(
                    "display",
                    thinking
                        .get("display")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
            }
            Some("enabled") => {
                config.insert("type", text("enabled"));
                config.insert(
                    "budgetTokens",
                    thinking
                        .get("budgetTokens")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
                config.insert(
                    "display",
                    thinking
                        .get("display")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
            }
            Some("disabled") => config.insert("type", text("disabled")),
            _ => return None,
        }
        return Some(config);
    }
    let tokens = get(options, "maxThinkingTokens")?;
    let mut config = JsObject::new();
    if tokens.as_f64() == Some(0.0) {
        config.insert("type", text("disabled"));
    } else {
        config.insert("type", text("enabled"));
        config.insert("budgetTokens", tokens.clone());
    }
    Some(config)
}

/// `sD` env handling: the options env (or `process.env`) with the SDK's
/// entries, then `ProcessTransport.initialize`'s `NODE_OPTIONS` and
/// `DEBUG` handling.
fn sdk_env(options: &JsObject, process_env: &JsObject) -> JsObject {
    let mut env = match get(options, "env").filter(|env| truthy(Some(env))) {
        Some(env) => spread(Some(env)),
        None => process_env.clone(),
    };
    if !truthy(env.get("CLAUDE_CODE_ENTRYPOINT")) {
        env.insert("CLAUDE_CODE_ENTRYPOINT", text("sdk-ts"));
    }
    if !truthy(env.get("CLAUDE_AGENT_SDK_VERSION")) {
        env.insert("CLAUDE_AGENT_SDK_VERSION", text(SDK_VERSION));
    }
    if truthy(get(options, "enableFileCheckpointing")) {
        env.insert("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", text("true"));
    }
    if let Some(format) = get(options, "toolConfig")
        .and_then(|config| config.get("askUserQuestion"))
        .and_then(|question| question.get("previewFormat"))
        .filter(|format| truthy(Some(format)))
    {
        env.insert("CLAUDE_CODE_QUESTION_PREVIEW_FORMAT", format.clone());
    }
    if !truthy(env.get("CLAUDE_CODE_ENTRYPOINT")) {
        env.insert("CLAUDE_CODE_ENTRYPOINT", text("sdk-ts"));
    }
    let debug = env_truthy(env.get("DEBUG_CLAUDE_AGENT_SDK"));
    if debug {
        env.insert("DEBUG", text("1"));
    }
    let mut cleaned = JsObject::new();
    for (key, value) in env.iter() {
        if key == "NODE_OPTIONS" || (key == "DEBUG" && !debug) {
            continue;
        }
        cleaned.insert(key, value.clone());
    }
    cleaned
}

/// The `skills` option's effect on `allowedTools`.
fn allowed_tools(options: &JsObject) -> Vec<JsValue> {
    let mut allowed: Vec<JsValue> = get(options, "allowedTools")
        .and_then(JsValue::as_array)
        .map(<[JsValue]>::to_vec)
        .unwrap_or_default();
    match get(options, "skills") {
        Some(JsValue::String(all)) if all == "all" => {
            if !allowed.iter().any(|tool| tool.as_str() == Some("Skill")) {
                allowed.push(text("Skill"));
            }
        }
        Some(JsValue::Array(skills)) => {
            let existing = allowed.clone();
            for skill in skills {
                let rule = format!("Skill({})", js_string(Some(skill)));
                if !existing.iter().any(|tool| tool.as_str() == Some(&rule)) {
                    allowed.push(JsValue::String(rule));
                }
            }
        }
        _ => {}
    }
    allowed
}

fn join(values: &[JsValue]) -> String {
    js_string(Some(&JsValue::Array(values.to_vec())))
}

/// The argv `ProcessTransport.initialize` builds after the executable.
#[allow(clippy::too_many_lines)] // One baseline function.
fn build_args(
    options: &JsObject,
    callbacks: SdkCallbacks,
    permission_mode: Option<&JsValue>,
) -> Result<Vec<String>, AgentError> {
    let mut args: Vec<String> = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    if let Some(config) = thinking_config(options) {
        match config.get("type").and_then(JsValue::as_str) {
            Some("enabled") => match config.get("budgetTokens") {
                None | Some(JsValue::Undefined) => {
                    args.extend(["--thinking".to_owned(), "adaptive".to_owned()]);
                }
                Some(budget) => {
                    args.extend(["--max-thinking-tokens".to_owned(), js_string(Some(budget))]);
                }
            },
            Some("disabled") => args.extend(["--thinking".to_owned(), "disabled".to_owned()]),
            Some("adaptive") => args.extend(["--thinking".to_owned(), "adaptive".to_owned()]),
            _ => {}
        }
        if config.get("type").and_then(JsValue::as_str) != Some("disabled")
            && let Some(display) = config.get("display").filter(|value| truthy(Some(value)))
        {
            args.extend(["--thinking-display".to_owned(), js_string(Some(display))]);
        }
    }
    let flag = |args: &mut Vec<String>, name: &str, key: &str| {
        if let Some(value) = get(options, key).filter(|value| truthy(Some(value))) {
            args.extend([name.to_owned(), js_string(Some(value))]);
        }
    };
    flag(&mut args, "--effort", "effort");
    flag(&mut args, "--max-turns", "maxTurns");
    if let Some(budget) = get(options, "maxBudgetUsd") {
        args.extend(["--max-budget-usd".to_owned(), js_string(Some(budget))]);
    }
    if let Some(budget) = get(options, "taskBudget").filter(|value| truthy(Some(value))) {
        args.extend(["--task-budget".to_owned(), js_string(budget.get("total"))]);
    }
    flag(&mut args, "--model", "model");
    flag(&mut args, "--agent", "agent");
    if array_len(get(options, "betas")) > 0 {
        args.extend([
            "--betas".to_owned(),
            join(
                get(options, "betas")
                    .and_then(JsValue::as_array)
                    .unwrap_or_default(),
            ),
        ]);
    }
    let json_schema = get(options, "outputFormat")
        .filter(|format| format.get("type").and_then(JsValue::as_str) == Some("json_schema"))
        .and_then(|format| format.get("schema"))
        .filter(|schema| !matches!(schema, JsValue::Undefined));
    if let Some(schema) = json_schema.filter(|schema| truthy(Some(schema))) {
        args.extend(["--json-schema".to_owned(), stringify(schema)]);
    }
    if let Some(file) = get(options, "debugFile").filter(|value| truthy(Some(value))) {
        args.extend(["--debug-file".to_owned(), js_string(Some(file))]);
    } else if truthy(get(options, "debug")) {
        args.push("--debug".to_owned());
    }
    let prompt_tool = get(options, "permissionPromptToolName").filter(|value| truthy(Some(value)));
    if callbacks.can_use_tool {
        if prompt_tool.is_some() {
            return Err(AgentError::new(
                "canUseTool callback cannot be used with permissionPromptToolName. Please use one or the other.",
            ));
        }
        args.extend(["--permission-prompt-tool".to_owned(), "stdio".to_owned()]);
    } else if let Some(tool) = prompt_tool {
        args.extend(["--permission-prompt-tool".to_owned(), js_string(Some(tool))]);
    }
    if truthy(get(options, "continue")) {
        args.push("--continue".to_owned());
    }
    if let Some(resume) = get(options, "resume").filter(|value| truthy(Some(value))) {
        args.push(format!("--resume={}", js_string(Some(resume))));
    }
    let allowed = allowed_tools(options);
    if !allowed.is_empty() {
        args.extend(["--allowedTools".to_owned(), join(&allowed)]);
    }
    if let Some(disallowed) = get(options, "disallowedTools")
        .and_then(JsValue::as_array)
        .filter(|tools| !tools.is_empty())
    {
        args.extend(["--disallowedTools".to_owned(), join(disallowed)]);
    }
    match get(options, "tools") {
        None => {}
        Some(JsValue::Array(tools)) if tools.is_empty() => {
            args.extend(["--tools".to_owned(), String::new()]);
        }
        Some(JsValue::Array(tools)) => args.extend(["--tools".to_owned(), join(tools)]),
        Some(_) => args.extend(["--tools".to_owned(), "default".to_owned()]),
    }
    if let Some(servers) = get(options, "mcpServers").filter(|value| truthy(Some(value))) {
        let mut external = JsObject::new();
        for (name, config) in servers.as_object().into_iter().flat_map(JsObject::iter) {
            if config.get("type").and_then(JsValue::as_str) == Some("sdk")
                && truthy(config.get("instance"))
            {
                continue;
            }
            external.insert(name, config.clone());
        }
        if !external.is_empty() {
            let mut wrapper = JsObject::new();
            wrapper.insert("mcpServers", JsValue::Object(external));
            args.extend([
                "--mcp-config".to_owned(),
                stringify(&JsValue::Object(wrapper)),
            ]);
        }
    }
    if let Some(sources) = get(options, "settingSources") {
        args.push(format!("--setting-sources={}", js_string(Some(sources))));
    }
    if truthy(get(options, "strictMcpConfig")) {
        args.push("--strict-mcp-config".to_owned());
    }
    if let Some(mode) = permission_mode.filter(|mode| truthy(Some(mode))) {
        args.extend(["--permission-mode".to_owned(), js_string(Some(mode))]);
    }
    if truthy(get(options, "allowDangerouslySkipPermissions")) {
        args.push("--allow-dangerously-skip-permissions".to_owned());
    }
    if let Some(fallback) = get(options, "fallbackModel").filter(|value| truthy(Some(value))) {
        if truthy(get(options, "model")) && get(options, "model") == Some(fallback) {
            return Err(AgentError::new(
                "Fallback model cannot be the same as the main model. Please specify a different model for fallbackModel option.",
            ));
        }
        args.extend(["--fallback-model".to_owned(), js_string(Some(fallback))]);
    }
    if truthy(get(options, "includeHookEvents")) {
        args.push("--include-hook-events".to_owned());
    }
    if truthy(get(options, "includePartialMessages")) {
        args.push("--include-partial-messages".to_owned());
    }
    for directory in get(options, "additionalDirectories")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        args.extend(["--add-dir".to_owned(), js_string(Some(directory))]);
    }
    for plugin in get(options, "plugins")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        if plugin.get("type").and_then(JsValue::as_str) == Some("local") {
            let flag = if truthy(plugin.get("skipMcpDiscovery")) {
                "--plugin-dir-no-mcp"
            } else {
                "--plugin-dir"
            };
            args.extend([flag.to_owned(), js_string(plugin.get("path"))]);
        } else {
            return Err(AgentError::new(format!(
                "Unsupported plugin type: {}",
                js_string(plugin.get("type"))
            )));
        }
    }
    if truthy(get(options, "forkSession")) {
        args.push("--fork-session".to_owned());
    }
    if let Some(at) = get(options, "resumeSessionAt").filter(|value| truthy(Some(value))) {
        args.push(format!("--resume-session-at={}", js_string(Some(at))));
    }
    if let Some(drops) = get(options, "resumeDropsTurn") {
        args.push(format!("--resume-drops-turn={}", js_string(Some(drops))));
    }
    if let Some(id) = get(options, "sessionId").filter(|value| truthy(Some(value))) {
        args.push(format!("--session-id={}", js_string(Some(id))));
    }
    if get(options, "persistSession") == Some(&JsValue::Bool(false)) {
        args.push("--no-session-persistence".to_owned());
    }
    if let Some(managed) = get(options, "managedSettings").filter(|value| truthy(Some(value))) {
        args.extend(["--managed-settings".to_owned(), stringify(managed)]);
    }
    let mut extra =
        get(options, "extraArgs").map_or_else(JsObject::new, |extra| spread(Some(extra)));
    if let Some(workload) = get(options, "workload").filter(|value| truthy(Some(value))) {
        extra.insert("workload", workload.clone());
    }
    // `typeof settings === "object" ? JSON.stringify(settings) : settings`,
    // then set when truthy; `null` is an object and becomes "null".
    if let Some(settings) = get(options, "settings") {
        let settings =
            if settings.is_object() || settings.as_array().is_some() || settings.is_null() {
                JsValue::String(stringify(settings))
            } else {
                settings.clone()
            };
        if truthy(Some(&settings)) {
            extra.insert("settings", settings);
        }
    }
    for (key, value) in merge_extra_args(&extra, get(options, "sandbox"))?.iter() {
        match value {
            JsValue::Null => args.push(format!("--{key}")),
            JsValue::Undefined => push_flag(&mut args, key, &JsValue::Undefined),
            other => push_flag(&mut args, key, other),
        }
    }
    Ok(args)
}

/// The `initialize` request's config, from `sD`'s `initConfig`.
fn init_config(options: &JsObject) -> JsObject {
    let system_prompt = options.get("systemPrompt");
    let (prompt, append, exclude) = match system_prompt {
        None | Some(JsValue::Undefined) => (Some(text("")), None, None),
        Some(JsValue::String(prompt)) => (Some(text(prompt)), None, None),
        Some(JsValue::Array(parts)) => (Some(JsValue::Array(parts.clone())), None, None),
        Some(other) if other.get("type").and_then(JsValue::as_str) == Some("preset") => (
            None,
            other.get("append").cloned(),
            other.get("excludeDynamicSections").cloned(),
        ),
        Some(_) => (None, None, None),
    };
    let member = |key: &str| options.get(key).cloned().unwrap_or(JsValue::Undefined);
    let mut request = JsObject::new();
    request.insert("subtype", text("initialize"));
    request.insert("hooks", JsValue::Undefined);
    request.insert("sdkMcpServers", JsValue::Undefined);
    let json_schema = get(options, "outputFormat")
        .filter(|format| format.get("type").and_then(JsValue::as_str) == Some("json_schema"))
        .and_then(|format| format.get("schema").cloned());
    request.insert("jsonSchema", json_schema.unwrap_or(JsValue::Undefined));
    request.insert(
        "systemPrompt",
        match &prompt {
            Some(JsValue::String(prompt)) => JsValue::Array(vec![JsValue::String(prompt.clone())]),
            Some(other) => other.clone(),
            None => JsValue::Undefined,
        },
    );
    request.insert("appendSystemPrompt", append.unwrap_or(JsValue::Undefined));
    request.insert("planModeInstructions", member("planModeInstructions"));
    request.insert(
        "appendSubagentSystemPrompt",
        member("appendSubagentSystemPrompt"),
    );
    request.insert("toolAliases", member("toolAliases"));
    request.insert(
        "excludeDynamicSections",
        exclude.unwrap_or(JsValue::Undefined),
    );
    request.insert("agents", member("agents"));
    request.insert("title", member("title"));
    let skills = member("skills");
    request.insert(
        "skills",
        if skills.as_array().is_some() {
            skills
        } else {
            JsValue::Undefined
        },
    );
    request.insert(
        "webSearchIsolationExemptMcpServers",
        member("webSearchIsolationExemptMcpServers"),
    );
    request.insert("promptSuggestions", member("promptSuggestions"));
    request.insert("agentProgressSummaries", member("agentProgressSummaries"));
    request.insert("forwardSubagentText", member("forwardSubagentText"));
    request.insert("supportedDialogKinds", member("supportedDialogKinds"));
    request.insert("perTaskStopAffordance", member("perTaskStopAffordance"));
    request
}

/// The hook matchers `Query.initialize` registers, with how many callbacks
/// each matcher holds.
fn hook_matchers(options: &JsObject) -> Vec<(String, JsValue, JsValue, usize)> {
    let mut matchers = Vec::new();
    for (event, entries) in get(options, "hooks")
        .and_then(JsValue::as_object)
        .into_iter()
        .flat_map(JsObject::iter)
    {
        for entry in entries.as_array().unwrap_or_default() {
            matchers.push((
                event.to_owned(),
                entry.get("matcher").cloned().unwrap_or(JsValue::Undefined),
                entry.get("timeout").cloned().unwrap_or(JsValue::Undefined),
                array_len(entry.get("hooks")),
            ));
        }
    }
    matchers
}

/// `query({ options })` up to the spawn: the command, argv, cwd, and env
/// `spawnClaudeCodeProcess` receives, and the `initialize` config.
///
/// # Errors
///
/// The SDK's option validation errors.
pub fn prepare_launch(
    options: &JsObject,
    callbacks: SdkCallbacks,
    process_env: &JsObject,
) -> Result<SdkLaunch, AgentError> {
    // `permissionMode ?? (resolvePermissionModeInCli ? undefined : "default")`.
    let permission_mode = match get(options, "permissionMode").filter(|mode| !mode.is_null()) {
        Some(mode) => mode.clone(),
        None if truthy(get(options, "resolvePermissionModeInCli")) => JsValue::Undefined,
        None => text("default"),
    };
    let env = sdk_env(options, process_env);
    let args = build_args(options, callbacks, Some(&permission_mode))?;
    let executable_path = get(options, "pathToClaudeCodeExecutable")
        .map(|path| js_string(Some(path)))
        .unwrap_or_default();
    let executable_args: Vec<String> = get(options, "executableArgs")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .map(|arg| js_string(Some(arg)))
        .collect();
    let (command, args) = if is_native_binary(&executable_path) {
        (executable_path, [executable_args, args].concat())
    } else {
        let executable = get(options, "executable")
            .map_or_else(|| "node".to_owned(), |value| js_string(Some(value)));
        (
            executable,
            [executable_args, vec![executable_path], args].concat(),
        )
    };
    Ok(SdkLaunch {
        command,
        args,
        cwd: get(options, "cwd").map(|cwd| js_string(Some(cwd))),
        env,
        init: init_config(options),
        hook_matchers: hook_matchers(options),
    })
}
