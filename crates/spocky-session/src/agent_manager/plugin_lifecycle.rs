//! The plugin lifecycle seam from pinned Paseo
//! `plugins/lifecycle/index.ts` and `plugins/runtime.ts`: the `before`
//! requests the agent manager sends through `pluginLifecycle`
//! (`agent.create`, `agent.session_open`) and the events it emits.
//!
//! [`NoPluginLifecycle`] is the runtime with no plugin loaded, as the daemon
//! runs it until the plugin host exists: `before` only validates the request,
//! `emit` goes nowhere.

use std::sync::OnceLock;

use spocky_contracts::zod::{Outcome, Schema, UnknownKeys, Verdict, verdict};
use spocky_store::js_value::{JsObject, JsValue, stringify_pretty};

use super::ManagedAgentSnapshot;
use crate::agent_labels::PARENT_AGENT_ID_LABEL;
use crate::agent_sdk::{AgentError, AgentResult, BoxFuture};

/// `PluginLifecycle`: `before(name, request)` resolves the request after
/// every plugin's transform, `emit(name, event)` tells the plugins.
pub trait PluginLifecycle: Send + Sync {
    /// `before(name, request)` for `agent.create` (`{ config, env }`) and
    /// `agent.session_open` (`{ agentId, workspaceId, provider, cwd, reason,
    /// purpose, env }`).
    fn before(&self, name: &str, request: JsValue) -> BoxFuture<'_, AgentResult<JsValue>>;

    /// `emit(name, event)`. It runs while the manager state is locked, so it
    /// must not call back into the manager.
    fn emit(&self, name: &str, event: JsValue);
}

/// The plugin runtime with no plugin loaded.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPluginLifecycle;

impl PluginLifecycle for NoPluginLifecycle {
    fn before(&self, name: &str, request: JsValue) -> BoxFuture<'_, AgentResult<JsValue>> {
        let name = name.to_owned();
        Box::pin(async move {
            match name.as_str() {
                "agent.create" => {
                    let config = request.get("config").cloned().unwrap_or(JsValue::Undefined);
                    let env = request.get("env").and_then(JsValue::as_object);
                    let config = before_agent_create(&config, env)?;
                    let mut parsed = JsObject::new();
                    parsed.insert("config", config);
                    if let Some(env) = env {
                        parsed.insert("env", JsValue::Object(env.clone()));
                    }
                    Ok(JsValue::Object(parsed))
                }
                "agent.session_open" => before_session_open(&request),
                other => Err(AgentError::new(format!("Unknown before hook: {other}"))),
            }
        })
    }

    fn emit(&self, _name: &str, _event: JsValue) {}
}

/// `describeHookAgent(agent)`: `{ id, workspaceId, parentAgentId, provider,
/// cwd, title }`, the agent as a plugin sees it.
pub(super) fn describe_hook_agent(
    id: &str,
    workspace_id: Option<&str>,
    labels: &JsValue,
    provider: &str,
    cwd: &str,
    title: Option<&str>,
) -> JsValue {
    let text = |value: &str| JsValue::String(value.to_owned());
    let mut agent = JsObject::new();
    agent.insert("id", text(id));
    agent.insert("workspaceId", workspace_id.map_or(JsValue::Null, text));
    // `agent.labels[PARENT_AGENT_ID_LABEL] ?? null`.
    agent.insert(
        "parentAgentId",
        labels
            .get(PARENT_AGENT_ID_LABEL)
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or(JsValue::Null),
    );
    agent.insert("provider", text(provider));
    agent.insert("cwd", text(cwd));
    agent.insert("title", title.map_or(JsValue::Null, text));
    JsValue::Object(agent)
}

/// `describeHookAgent({ ...agent, title: agent.config.title })`.
pub(super) fn describe_hook_agent_of(agent: &ManagedAgentSnapshot) -> JsValue {
    describe_hook_agent(
        &agent.id,
        agent.workspace_id.as_deref(),
        &agent.labels,
        &agent.provider,
        &agent.cwd,
        agent.config.get("title").and_then(JsValue::as_str),
    )
}

/// `publishAgentStream(lifecycle, agent, event, timeline)`: the turn and
/// permission events plugins hear about; `timeline` runs only for a turn end.
pub(super) fn publish_agent_stream(
    lifecycle: &dyn PluginLifecycle,
    agent: &JsValue,
    event: &JsValue,
    timeline: impl FnOnce() -> Vec<JsValue>,
) {
    let field = |key: &str| event.get(key).cloned().unwrap_or(JsValue::Undefined);
    // `event.turnId ?? null`.
    let turn_id = || match event.get("turnId") {
        None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Null,
        Some(turn) => turn.clone(),
    };
    let object = |entries: Vec<(&str, JsValue)>| {
        let mut out = JsObject::new();
        for (key, value) in entries {
            out.insert(key, value);
        }
        JsValue::Object(out)
    };
    let kind = |kind: &str, extra: Vec<(&str, JsValue)>| {
        let mut entries = vec![("kind", JsValue::String(kind.to_owned()))];
        entries.extend(extra);
        object(entries)
    };
    let ended = |outcome: JsValue| {
        lifecycle.emit(
            "agent.turn_ended",
            object(vec![
                ("agent", agent.clone()),
                ("turnId", turn_id()),
                ("timeline", JsValue::Array(timeline())),
                ("outcome", outcome),
            ]),
        );
    };
    match event.get("type").and_then(JsValue::as_str) {
        Some("turn_started") => lifecycle.emit(
            "agent.turn_started",
            object(vec![("agent", agent.clone()), ("turnId", turn_id())]),
        ),
        Some("turn_completed") => ended(kind("completed", Vec::new())),
        Some("turn_failed") => ended(kind(
            "failed",
            vec![(
                "error",
                object(vec![("message", field("error")), ("code", field("code"))]),
            )],
        )),
        Some("turn_canceled") => ended(kind("canceled", vec![("reason", field("reason"))])),
        Some("permission_requested") => lifecycle.emit(
            "agent.permission_requested",
            object(vec![
                ("agent", agent.clone()),
                ("request", field("request")),
            ]),
        ),
        Some("permission_resolved") => lifecycle.emit(
            "agent.permission_resolved",
            object(vec![
                ("agent", agent.clone()),
                ("requestId", field("requestId")),
                ("resolution", field("resolution")),
            ]),
        ),
        _ => {}
    }
}

fn session_open_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let string = || Schema::String(Vec::new());
        Schema::Object(
            vec![
                ("agentId", string()),
                ("workspaceId", Schema::Nullable(Box::new(string()))),
                ("provider", string()),
                ("cwd", string()),
                (
                    "reason",
                    Schema::Enum(&["create", "resume", "refresh", "import"]),
                ),
                ("purpose", Schema::Enum(&["interactive", "history"])),
                (
                    "env",
                    Schema::Record(Box::new(string()), Box::new(string())),
                ),
            ],
            UnknownKeys::Strict,
        )
    })
}

/// `validateBeforeRequest("agent.session_open", request)`: the request
/// parsed by a strict object, so a bad field is the `ZodError` of its issues.
fn before_session_open(request: &JsValue) -> AgentResult<JsValue> {
    match verdict(session_open_schema(), request) {
        Verdict::Valid(parsed) => Ok(parsed),
        Verdict::Invalid(issues) => Err(AgentError::named(
            "ZodError",
            stringify_pretty(&JsValue::Array(issues)),
        )),
        Verdict::TooDeep => Err(AgentError::named(
            "RangeError",
            "Maximum call stack size exceeded",
        )),
        Verdict::Unmodeled | Verdict::Throws(_) => {
            unreachable!("the session-open schema has no unmodeled or throwing parts")
        }
    }
}

/// `pluginLifecycle.before("agent.create", { config, env })` with no plugin
/// loaded: the request is only parsed by
/// `CreateAgentRequestMessageSchema.pick({ config, env }).strict()`, so the
/// config keeps its schema keys, in schema order, and loses any other key.
/// `env` (a record of strings) parses to itself.
///
/// A failing request reports zod's issue list from the contracts schema:
/// the request is checked as a session `create_agent_request` holding only
/// the `config` and `env` (plus its required `type` and `requestId`), whose
/// discriminated option reports the same issues as the picked schema, under
/// the session envelope's `message` key, which each issue path drops.
pub(super) fn before_agent_create(
    config: &JsValue,
    env: Option<&JsObject>,
) -> Result<JsValue, AgentError> {
    let parse_error = |message: String| AgentError::named("ZodError", message);
    let mut request = JsObject::new();
    request.insert("type", JsValue::String("create_agent_request".to_owned()));
    request.insert("config", config.clone());
    if let Some(env) = env {
        request.insert("env", JsValue::Object(env.clone()));
    }
    request.insert("requestId", JsValue::String("agent.create".to_owned()));
    let mut envelope = JsObject::new();
    envelope.insert("type", JsValue::String("session".to_owned()));
    envelope.insert("message", JsValue::Object(request));
    match spocky_contracts::zod_schemas::check_inbound(&JsValue::Object(envelope)) {
        Outcome::Invalid(issues) => return Err(parse_error(without_envelope_path(&issues))),
        Outcome::TooDeep => {
            return Err(AgentError::named(
                "RangeError".to_owned(),
                "Maximum call stack size exceeded".to_owned(),
            ));
        }
        Outcome::Valid | Outcome::Unmodeled => {}
    }
    let parsed =
        <spocky_contracts::agent_config::AgentSessionConfig as serde::Deserialize>::deserialize(
            spocky_contracts::json::JsValueDeserializer(config),
        )
        .map_err(|error| parse_error(error.to_string()))?;
    let text = serde_json::to_string(&parsed).map_err(|error| parse_error(error.to_string()))?;
    spocky_store::js_value::parse(&text).map_err(|error| parse_error(error.to_string()))
}

/// Drops the session envelope's leading `message` from each issue path; a
/// nested union's issues keep their own relative paths.
fn without_envelope_path(issues: &str) -> String {
    let Ok(parsed) = spocky_store::js_value::parse(issues) else {
        return issues.to_owned();
    };
    let Some(list) = parsed.as_array() else {
        return issues.to_owned();
    };
    let issues = list
        .iter()
        .map(|issue| {
            let (JsValue::Object(object), Some(path)) =
                (issue, issue.get("path").and_then(JsValue::as_array))
            else {
                return issue.clone();
            };
            let skip = usize::from(path.first().and_then(JsValue::as_str) == Some("message"));
            let mut object = object.clone();
            object.insert("path", JsValue::Array(path[skip..].to_vec()));
            JsValue::Object(object)
        })
        .collect();
    spocky_store::js_value::stringify_pretty(&JsValue::Array(issues))
}
