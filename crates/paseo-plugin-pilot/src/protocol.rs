use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "scope", deny_unknown_fields)]
pub enum ProviderCatalogOptions {
    #[serde(rename = "global")]
    Global {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },
    #[serde(rename = "workspace")]
    Workspace {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConnectRequest {
    pub versions: Vec<u64>,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ProviderInput {
    #[serde(rename = "catalog")]
    Catalog {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    #[serde(rename = "sessions")]
    Sessions {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    #[serde(rename = "session.open")]
    SessionOpen {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        config: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persistence: Option<Value>,
        history: String,
    },
    #[serde(rename = "session.prompt")]
    SessionPrompt {
        #[serde(rename = "sessionId")]
        session_id: String,
        prompt: Value,
    },
    #[serde(rename = "session.interrupt")]
    SessionInterrupt {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    #[serde(rename = "session.usage_reference")]
    SessionUsageReference {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    #[serde(rename = "session.permission")]
    SessionPermission {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "permissionId")]
        permission_id: String,
        response: Value,
    },
    #[serde(rename = "session.configure")]
    SessionConfigure {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        changes: Value,
    },
    #[serde(rename = "session.revert")]
    SessionRevert {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        token: Value,
        scope: String,
    },
    #[serde(rename = "session.archive")]
    SessionArchive {
        #[serde(rename = "requestId")]
        request_id: String,
        persistence: Value,
    },
    #[serde(rename = "session.unarchive")]
    SessionUnarchive {
        #[serde(rename = "requestId")]
        request_id: String,
        persistence: Value,
    },
    #[serde(rename = "session.close")]
    SessionClose {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum ProviderEvent {
    #[serde(rename = "catalog")]
    Catalog {
        #[serde(rename = "requestId")]
        request_id: String,
        catalog: Value,
    },
    #[serde(rename = "sessions")]
    Sessions {
        #[serde(rename = "requestId")]
        request_id: String,
        sessions: Vec<Value>,
    },
    #[serde(rename = "request.completed")]
    RequestCompleted {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "usage_reference")]
    UsageReference {
        #[serde(rename = "requestId")]
        request_id: String,
        reference: Option<Value>,
    },
    #[serde(rename = "request.failed")]
    RequestFailed {
        #[serde(rename = "requestId")]
        request_id: String,
        error: Value,
    },
    #[serde(rename = "session.opened")]
    SessionOpened {
        #[serde(default, rename = "requestId", skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(
            default,
            rename = "parentSessionId",
            skip_serializing_if = "Option::is_none"
        )]
        parent_session_id: Option<String>,
        #[serde(
            default,
            rename = "toolCallId",
            skip_serializing_if = "Option::is_none"
        )]
        tool_call_id: Option<String>,
        capabilities: Vec<String>,
        restoration: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persistence: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        cwd: String,
    },
    #[serde(rename = "session.ready")]
    SessionReady {
        #[serde(default, rename = "requestId", skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    #[serde(rename = "session.closed")]
    SessionClosed {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<Value>,
    },
    #[serde(rename = "session.runtime_failed")]
    SessionRuntimeFailed {
        #[serde(rename = "sessionId")]
        session_id: String,
        error: Value,
    },
    #[serde(rename = "session.persistence")]
    SessionPersistence {
        #[serde(rename = "sessionId")]
        session_id: String,
        persistence: Value,
    },
    #[serde(rename = "session.prompt_result")]
    SessionPromptResult {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "clientMessageId")]
        client_message_id: String,
        result: Value,
    },
    #[serde(rename = "session.turn")]
    SessionTurn {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<Value>,
    },
    #[serde(rename = "session.usage")]
    SessionUsage {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        usage: Value,
    },
    #[serde(rename = "session.config")]
    SessionConfig {
        #[serde(rename = "sessionId")]
        session_id: String,
        config: Value,
    },
    #[serde(rename = "session.commands")]
    SessionCommands {
        #[serde(rename = "sessionId")]
        session_id: String,
        commands: Vec<Value>,
    },
    #[serde(rename = "session.permission")]
    SessionPermission {
        #[serde(rename = "sessionId")]
        session_id: String,
        request: Value,
    },
    #[serde(rename = "session.permission_resolved")]
    SessionPermissionResolved {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "permissionId")]
        permission_id: String,
    },
    #[serde(rename = "session.notice")]
    SessionNotice {
        #[serde(rename = "sessionId")]
        session_id: String,
        notice: Value,
    },
    #[serde(rename = "timeline.item")]
    TimelineItem {
        #[serde(rename = "sessionId")]
        session_id: String,
        item: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HookKind {
    Event,
    Before,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessHooks {
    pub events: Vec<String>,
    pub before: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessProviderMetadata {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, rename = "iconPath", skip_serializing_if = "Option::is_none")]
    icon_path: Option<String>,
    #[serde(
        default,
        rename = "hasCatalogCacheKey",
        skip_serializing_if = "Option::is_none"
    )]
    has_catalog_cache_key: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessUsageSourceMetadata {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    discover: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum PluginProcessRequest {
    #[serde(rename = "initialize")]
    Initialize {
        #[serde(rename = "pluginId")]
        plugin_id: String,
        bundle: String,
        #[serde(rename = "appVersion")]
        app_version: String,
        #[serde(rename = "pluginDirectory")]
        plugin_directory: String,
        #[serde(
            default,
            rename = "settingsDirectory",
            skip_serializing_if = "Option::is_none"
        )]
        settings_directory: Option<String>,
    },
    #[serde(rename = "provider.catalog_key")]
    ProviderCatalogKey {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "providerId")]
        provider_id: String,
        options: ProviderCatalogOptions,
    },
    #[serde(rename = "hook")]
    Hook {
        #[serde(rename = "requestId")]
        request_id: String,
        kind: HookKind,
        name: String,
        input: Value,
    },
    #[serde(rename = "hook.cancel")]
    HookCancel {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "usage.identify")]
    UsageIdentify {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sourceId")]
        source_id: String,
        input: Value,
    },
    #[serde(rename = "usage.fetch")]
    UsageFetch {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sourceId")]
        source_id: String,
        input: Value,
    },
    #[serde(rename = "usage.discover")]
    UsageDiscover {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "sourceId")]
        source_id: String,
    },
    #[serde(rename = "invoke")]
    Invoke {
        #[serde(rename = "requestId")]
        request_id: String,
        method: String,
        input: Value,
    },
    #[serde(rename = "provider.connect")]
    ProviderConnect {
        #[serde(rename = "providerId")]
        provider_id: String,
        #[serde(rename = "connectionId")]
        connection_id: String,
        request: ProviderConnectRequest,
    },
    #[serde(rename = "provider.send")]
    ProviderSend {
        #[serde(rename = "connectionId")]
        connection_id: String,
        #[serde(rename = "acceptanceId")]
        acceptance_id: String,
        input: ProviderInput,
    },
    #[serde(rename = "provider.close")]
    ProviderClose {
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
    #[serde(rename = "shutdown")]
    Shutdown {},
    #[serde(rename = "paseo_frame")]
    PaseoFrame {
        data: Value,
        #[serde(rename = "isBinary")]
        is_binary: bool,
    },
    #[serde(rename = "paseo_close")]
    PaseoClose {},
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum PluginProcessMessage {
    #[serde(rename = "settings.changed")]
    SettingsChanged {
        #[serde(rename = "settingsId")]
        settings_id: String,
    },
    #[serde(rename = "hooks.changed")]
    HooksChanged { hooks: ProcessHooks },
    #[serde(rename = "ready")]
    Ready {
        methods: Vec<String>,
        providers: Vec<ProcessProviderMetadata>,
        #[serde(
            default,
            rename = "usageSources",
            skip_serializing_if = "Option::is_none"
        )]
        usage_sources: Option<Vec<ProcessUsageSourceMetadata>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hooks: Option<ProcessHooks>,
    },
    #[serde(rename = "result")]
    Result {
        #[serde(rename = "requestId")]
        request_id: String,
        output: Value,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "requestId")]
        request_id: String,
        error: String,
    },
    #[serde(rename = "fatal")]
    Fatal { error: String },
    #[serde(rename = "provider.connected")]
    ProviderConnected {
        #[serde(rename = "connectionId")]
        connection_id: String,
        version: u64,
        capabilities: Vec<String>,
    },
    #[serde(rename = "provider.connect_failed")]
    ProviderConnectFailed {
        #[serde(rename = "connectionId")]
        connection_id: String,
        error: String,
    },
    #[serde(rename = "provider.accepted")]
    ProviderAccepted {
        #[serde(rename = "connectionId")]
        connection_id: String,
        #[serde(rename = "acceptanceId")]
        acceptance_id: String,
    },
    #[serde(rename = "provider.rejected")]
    ProviderRejected {
        #[serde(rename = "connectionId")]
        connection_id: String,
        #[serde(rename = "acceptanceId")]
        acceptance_id: String,
        error: String,
    },
    #[serde(rename = "provider.event")]
    ProviderEvent {
        #[serde(rename = "connectionId")]
        connection_id: String,
        event: ProviderEvent,
    },
    #[serde(rename = "provider.closed")]
    ProviderClosed {
        #[serde(rename = "connectionId")]
        connection_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "paseo_frame")]
    PaseoFrame {
        data: Value,
        #[serde(rename = "isBinary")]
        is_binary: bool,
    },
    #[serde(rename = "paseo_close")]
    PaseoClose {},
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeProtocolStep {
    Send(PluginProcessRequest),
    Receive(PluginProcessMessage),
}

pub fn decode_process_request(encoded: &str) -> Result<PluginProcessRequest, String> {
    let request: PluginProcessRequest =
        serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    validate_request(&request)?;
    Ok(request)
}

pub fn decode_process_message(encoded: &str) -> Result<PluginProcessMessage, String> {
    let message: PluginProcessMessage =
        serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    validate_message(&message)?;
    Ok(message)
}

fn require_nonempty(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

fn require_optional_nonempty(value: Option<&String>, field: &str) -> Result<(), String> {
    value.map_or(Ok(()), |value| require_nonempty(value, field))
}

impl ProviderInput {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Catalog { request_id, .. } => require_nonempty(request_id, "requestId"),
            Self::Sessions {
                request_id, limit, ..
            } => {
                require_nonempty(request_id, "requestId")?;
                if limit == &Some(0) {
                    return Err("limit must be positive".into());
                }
                Ok(())
            }
            Self::SessionOpen {
                request_id,
                session_id,
                config,
                persistence,
                history,
            } => {
                require_nonempty(request_id, "requestId")?;
                require_nonempty(session_id, "sessionId")?;
                if !matches!(history.as_str(), "replay" | "skip") {
                    return Err("history must be replay or skip".into());
                }
                validate_session_config(config)?;
                if let Some(persistence) = persistence {
                    validate_persistence(persistence)?;
                }
                Ok(())
            }
            Self::SessionPrompt { session_id, prompt } => {
                require_nonempty(session_id, "sessionId")?;
                validate_prompt(prompt)
            }
            Self::SessionInterrupt {
                request_id,
                session_id,
            }
            | Self::SessionUsageReference {
                request_id,
                session_id,
            }
            | Self::SessionClose {
                request_id,
                session_id,
            } => {
                require_nonempty(request_id, "requestId")?;
                require_nonempty(session_id, "sessionId")
            }
            Self::SessionConfigure {
                request_id,
                session_id,
                changes,
            } => {
                require_nonempty(request_id, "requestId")?;
                require_nonempty(session_id, "sessionId")?;
                validate_config_changes(changes)
            }
            Self::SessionPermission {
                session_id,
                permission_id,
                response,
            } => {
                require_nonempty(session_id, "sessionId")?;
                require_nonempty(permission_id, "permissionId")?;
                validate_permission_response(response)
            }
            Self::SessionRevert {
                request_id,
                session_id,
                scope,
                ..
            } => {
                require_nonempty(request_id, "requestId")?;
                require_nonempty(session_id, "sessionId")?;
                if !matches!(scope.as_str(), "conversation" | "files" | "both") {
                    return Err("scope must be conversation, files, or both".into());
                }
                Ok(())
            }
            Self::SessionArchive {
                request_id,
                persistence,
            }
            | Self::SessionUnarchive {
                request_id,
                persistence,
            } => {
                require_nonempty(request_id, "requestId")?;
                validate_persistence(persistence)
            }
        }
    }
}

fn value_object<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}

fn strict_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    label: &str,
) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown {label} field: {key}"));
    }
    Ok(())
}

fn required<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<&'a Value, String> {
    object
        .get(key)
        .ok_or_else(|| format!("{label}.{key} is required"))
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<(), String> {
    if required(object, key, label)?.is_string() {
        Ok(())
    } else {
        Err(format!("{label}.{key} must be a string"))
    }
}

fn optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<(), String> {
    if object.get(key).is_none_or(Value::is_string) {
        Ok(())
    } else {
        Err(format!("{label}.{key} must be a string"))
    }
}

fn validate_string_map(value: &Value, label: &str) -> Result<(), String> {
    let object = value_object(value, label)?;
    if object.values().all(Value::is_string) {
        Ok(())
    } else {
        Err(format!("{label} values must be strings"))
    }
}

fn validate_persistence(value: &Value) -> Result<(), String> {
    let object = value_object(value, "persistence")?;
    if required(object, "version", "persistence")?
        .as_u64()
        .is_none()
    {
        return Err("persistence.version must be a nonnegative integer".into());
    }
    required(object, "data", "persistence")?;
    Ok(())
}

fn validate_mcp_server(value: &Value) -> Result<(), String> {
    let object = value_object(value, "mcp server")?;
    let kind = required(object, "type", "mcp server")?
        .as_str()
        .ok_or_else(|| "mcp server.type must be a string".to_owned())?;
    match kind {
        "stdio" => {
            strict_keys(
                object,
                &["type", "command", "args", "env", "alwaysLoad"],
                "mcp server",
            )?;
            required_string(object, "command", "mcp server")?;
            if let Some(args) = object.get("args")
                && !args
                    .as_array()
                    .is_some_and(|values| values.iter().all(Value::is_string))
            {
                return Err("mcp server.args must be strings".into());
            }
            if let Some(env) = object.get("env") {
                validate_string_map(env, "mcp server.env")?;
            }
        }
        "http" | "sse" => {
            strict_keys(
                object,
                &["type", "url", "headers", "alwaysLoad"],
                "mcp server",
            )?;
            required_string(object, "url", "mcp server")?;
            if let Some(headers) = object.get("headers") {
                validate_string_map(headers, "mcp server.headers")?;
            }
        }
        _ => return Err("mcp server.type must be stdio, http, or sse".into()),
    }
    if object
        .get("alwaysLoad")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("mcp server.alwaysLoad must be a boolean".into());
    }
    Ok(())
}

fn validate_session_config(value: &Value) -> Result<(), String> {
    let object = value_object(value, "config")?;
    strict_keys(
        object,
        &[
            "cwd",
            "env",
            "systemPrompt",
            "mcpServers",
            "toolPolicy",
            "model",
            "mode",
            "thinkingOption",
            "settings",
            "providerOptions",
            "title",
            "persist",
        ],
        "config",
    )?;
    required_string(object, "cwd", "config")?;
    validate_string_map(required(object, "env", "config")?, "config.env")?;
    let servers = value_object(
        required(object, "mcpServers", "config")?,
        "config.mcpServers",
    )?;
    for server in servers.values() {
        validate_mcp_server(server)?;
    }
    value_object(required(object, "settings", "config")?, "config.settings")?;
    if let Some(options) = object.get("providerOptions") {
        value_object(options, "config.providerOptions")?;
    }
    for key in ["systemPrompt", "model", "mode", "thinkingOption", "title"] {
        optional_string(object, key, "config")?;
    }
    if !required(object, "persist", "config")?.is_boolean() {
        return Err("config.persist must be a boolean".into());
    }
    if let Some(policy) = object.get("toolPolicy") {
        let policy = value_object(policy, "config.toolPolicy")?;
        strict_keys(policy, &["preapproved"], "tool policy")?;
        let entries = required(policy, "preapproved", "tool policy")?
            .as_array()
            .ok_or_else(|| "tool policy.preapproved must be an array".to_owned())?;
        for entry in entries {
            let entry = value_object(entry, "preapproved entry")?;
            strict_keys(entry, &["kind", "server", "tool"], "preapproved entry")?;
            if required(entry, "kind", "preapproved entry")?.as_str() != Some("mcp") {
                return Err("preapproved entry.kind must be mcp".into());
            }
            required_string(entry, "server", "preapproved entry")?;
            required_string(entry, "tool", "preapproved entry")?;
        }
    }
    Ok(())
}

fn validate_prompt_content(value: &Value) -> Result<(), String> {
    let object = value_object(value, "prompt content")?;
    let kind = required(object, "type", "prompt content")?
        .as_str()
        .ok_or_else(|| "prompt content.type must be a string".to_owned())?;
    match kind {
        "text" if object.get("mimeType").is_none() => {
            strict_keys(object, &["type", "text"], "text content")?;
            required_string(object, "text", "text content")
        }
        "image" => {
            strict_keys(object, &["type", "data", "mimeType"], "image content")?;
            required_string(object, "data", "image content")?;
            required_string(object, "mimeType", "image content")
        }
        "forge_change_request"
        | "forge_issue"
        | "github_pr"
        | "github_issue"
        | "text"
        | "review"
        | "uploaded_file" => Ok(()),
        _ => Err(format!("invalid prompt content type: {kind}")),
    }
}

fn validate_prompt(value: &Value) -> Result<(), String> {
    let object = value_object(value, "prompt")?;
    strict_keys(
        object,
        &[
            "clientMessageId",
            "delivery",
            "input",
            "outputSchema",
            "clearPendingPermissions",
        ],
        "prompt",
    )?;
    required_string(object, "clientMessageId", "prompt")?;
    if !matches!(
        required(object, "delivery", "prompt")?.as_str(),
        Some("auto" | "steer")
    ) {
        return Err("prompt.delivery must be auto or steer".into());
    }
    if object
        .get("clearPendingPermissions")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("prompt.clearPendingPermissions must be a boolean".into());
    }
    let input = value_object(required(object, "input", "prompt")?, "prompt.input")?;
    match required(input, "type", "prompt.input")?.as_str() {
        Some("message") => {
            strict_keys(input, &["type", "content"], "prompt.input")?;
            let content = required(input, "content", "prompt.input")?
                .as_array()
                .ok_or_else(|| "prompt.input.content must be an array".to_owned())?;
            for item in content {
                validate_prompt_content(item)?;
            }
            Ok(())
        }
        Some("command") => {
            strict_keys(input, &["type", "name", "arguments"], "prompt.input")?;
            required_string(input, "name", "prompt.input")?;
            required_string(input, "arguments", "prompt.input")
        }
        _ => Err("prompt.input.type must be message or command".into()),
    }
}

fn validate_permission_response(value: &Value) -> Result<(), String> {
    let object = value_object(value, "permission response")?;
    match required(object, "behavior", "permission response")?.as_str() {
        Some("allow") => {
            if let Some(input) = object.get("updatedInput") {
                value_object(input, "permission response.updatedInput")?;
            }
            if let Some(permissions) = object.get("updatedPermissions")
                && !permissions
                    .as_array()
                    .is_some_and(|items| items.iter().all(Value::is_object))
            {
                return Err("permission response.updatedPermissions must be objects".into());
            }
            Ok(())
        }
        Some("deny") => {
            if object
                .get("interrupt")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("permission response.interrupt must be a boolean".into());
            }
            Ok(())
        }
        _ => Err("permission response.behavior must be allow or deny".into()),
    }
}

fn validate_config_changes(value: &Value) -> Result<(), String> {
    let object = value_object(value, "changes")?;
    strict_keys(
        object,
        &["model", "mode", "thinkingOption", "settings"],
        "changes",
    )?;
    for key in ["model", "mode", "thinkingOption"] {
        if object
            .get(key)
            .is_some_and(|value| !(value.is_string() || value.is_null()))
        {
            return Err(format!("changes.{key} must be a string or null"));
        }
    }
    if let Some(settings) = object.get("settings") {
        value_object(settings, "changes.settings")?;
    }
    Ok(())
}

fn validate_catalog(value: &Value) -> Result<(), String> {
    let catalog = value_object(value, "catalog")?;
    for (field, item_label) in [("models", "model"), ("modes", "mode")] {
        let items = required(catalog, field, "catalog")?
            .as_array()
            .ok_or_else(|| format!("catalog.{field} must be an array"))?;
        for item in items {
            let item = value_object(item, item_label)?;
            let id = required(item, "id", item_label)?
                .as_str()
                .ok_or_else(|| format!("{item_label}.id must be a string"))?;
            require_nonempty(id, &format!("{item_label}.id"))?;
            required_string(item, "label", item_label)?;
            if item_label == "model" {
                if let Some(aliases) = item.get("aliases")
                    && !aliases
                        .as_array()
                        .is_some_and(|values| values.iter().all(Value::is_string))
                {
                    return Err("model.aliases must be strings".into());
                }
                if let Some(options) = item.get("thinkingOptions") {
                    validate_select_options(options, "model.thinkingOptions")?;
                }
            }
        }
    }
    if let Some(options) = catalog.get("thinkingOptions") {
        validate_select_options(options, "catalog.thinkingOptions")?;
    }
    Ok(())
}

fn validate_select_options(value: &Value, label: &str) -> Result<(), String> {
    let options = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))?;
    for option in options {
        let option = value_object(option, label)?;
        let id = required(option, "id", label)?
            .as_str()
            .ok_or_else(|| format!("{label}.id must be a string"))?;
        require_nonempty(id, &format!("{label}.id"))?;
        required_string(option, "label", label)?;
    }
    Ok(())
}

fn validate_tool_call_detail(value: &Value) -> Result<(), String> {
    let detail = value_object(value, "tool detail")?;
    let kind = required(detail, "type", "tool detail")?
        .as_str()
        .ok_or_else(|| "tool detail.type must be a string".to_owned())?;
    match kind {
        "shell" => required_string(detail, "command", "tool detail"),
        "read" | "edit" | "write" => required_string(detail, "filePath", "tool detail"),
        "search" => {
            required_string(detail, "query", "tool detail")?;
            if let Some(tool) = detail.get("toolName")
                && !matches!(
                    tool.as_str(),
                    Some("search" | "grep" | "glob" | "web_search")
                )
            {
                return Err("invalid search toolName".into());
            }
            if let Some(mode) = detail.get("mode")
                && !matches!(
                    mode.as_str(),
                    Some("content" | "files_with_matches" | "count")
                )
            {
                return Err("invalid search mode".into());
            }
            Ok(())
        }
        "fetch" => required_string(detail, "url", "tool detail"),
        "worktree_setup" => {
            required_string(detail, "worktreePath", "tool detail")?;
            required_string(detail, "branchName", "tool detail")?;
            required_string(detail, "log", "tool detail")
        }
        "sub_agent" => required_string(detail, "log", "tool detail"),
        "plain_text" | "unknown" => Ok(()),
        "plan" => required_string(detail, "text", "tool detail"),
        _ => Err(format!("invalid tool detail type: {kind}")),
    }
}

fn validate_timeline_item(value: &Value) -> Result<(), String> {
    let item = value_object(value, "timeline item")?;
    let id = required(item, "id", "timeline item")?
        .as_str()
        .ok_or_else(|| "timeline item.id must be a string".to_owned())?;
    require_nonempty(id, "timeline item.id")?;
    let kind = required(item, "type", "timeline item")?
        .as_str()
        .ok_or_else(|| "timeline item.type must be a string".to_owned())?;
    match kind {
        "user_message" | "assistant_message" | "reasoning" => {
            required_string(item, "text", "timeline item")
        }
        "tool_call" => {
            required_string(item, "callId", "timeline item")?;
            required_string(item, "name", "timeline item")?;
            validate_tool_call_detail(required(item, "detail", "timeline item")?)?;
            match required(item, "status", "timeline item")?.as_str() {
                Some("failed") => required(item, "error", "timeline item").map(drop),
                Some("running" | "completed" | "canceled") => {
                    if required(item, "error", "timeline item")?.is_null() {
                        Ok(())
                    } else {
                        Err("non-failed tool call error must be null".into())
                    }
                }
                _ => Err("invalid tool call status".into()),
            }
        }
        "todo" => {
            if required(item, "items", "timeline item")?.is_array() {
                Ok(())
            } else {
                Err("timeline todo items must be an array".into())
            }
        }
        "error" | "notification" => required_string(item, "message", "timeline item"),
        "compaction" => {
            if matches!(
                required(item, "status", "timeline item")?.as_str(),
                Some("loading" | "completed")
            ) {
                Ok(())
            } else {
                Err("invalid compaction status".into())
            }
        }
        "plugin" => {
            required_string(item, "pluginId", "timeline item")?;
            required_string(item, "kind", "timeline item")?;
            if required(item, "version", "timeline item")?.is_number() {
                required(item, "data", "timeline item")?;
                Ok(())
            } else {
                Err("plugin timeline version must be a number".into())
            }
        }
        _ => Err(format!("invalid timeline item type: {kind}")),
    }
}

impl ProviderEvent {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Catalog {
                request_id,
                catalog,
            } => {
                require_nonempty(request_id, "requestId")?;
                validate_catalog(catalog)
            }
            Self::Sessions { request_id, .. }
            | Self::RequestCompleted { request_id }
            | Self::UsageReference { request_id, .. }
            | Self::RequestFailed { request_id, .. } => require_nonempty(request_id, "requestId"),
            Self::SessionOpened {
                request_id,
                session_id,
                parent_session_id,
                tool_call_id,
                restoration,
                ..
            } => {
                require_optional_nonempty(request_id.as_ref(), "requestId")?;
                require_nonempty(session_id, "sessionId")?;
                require_optional_nonempty(parent_session_id.as_ref(), "parentSessionId")?;
                require_optional_nonempty(tool_call_id.as_ref(), "toolCallId")?;
                if !matches!(restoration.as_str(), "core" | "parent") {
                    return Err("restoration must be core or parent".into());
                }
                Ok(())
            }
            Self::SessionReady {
                request_id,
                session_id,
            } => {
                require_optional_nonempty(request_id.as_ref(), "requestId")?;
                require_nonempty(session_id, "sessionId")
            }
            Self::SessionClosed { session_id, .. }
            | Self::SessionRuntimeFailed { session_id, .. }
            | Self::SessionUsage { session_id, .. }
            | Self::SessionConfig { session_id, .. }
            | Self::SessionCommands { session_id, .. }
            | Self::SessionPermission { session_id, .. }
            | Self::SessionNotice { session_id, .. } => require_nonempty(session_id, "sessionId"),
            Self::SessionPersistence {
                session_id,
                persistence,
            } => {
                require_nonempty(session_id, "sessionId")?;
                validate_persistence(persistence)
            }
            Self::TimelineItem {
                session_id, item, ..
            } => {
                require_nonempty(session_id, "sessionId")?;
                validate_timeline_item(item)
            }
            Self::SessionPromptResult {
                session_id,
                client_message_id,
                ..
            } => {
                require_nonempty(session_id, "sessionId")?;
                require_nonempty(client_message_id, "clientMessageId")
            }
            Self::SessionTurn {
                session_id,
                turn_id,
                state,
                ..
            } => {
                require_nonempty(session_id, "sessionId")?;
                require_nonempty(turn_id, "turnId")?;
                if !matches!(
                    state.as_str(),
                    "started" | "completed" | "failed" | "canceled"
                ) {
                    return Err("invalid session turn state".into());
                }
                Ok(())
            }
            Self::SessionPermissionResolved {
                session_id,
                permission_id,
            } => {
                require_nonempty(session_id, "sessionId")?;
                require_nonempty(permission_id, "permissionId")
            }
        }
    }
}

fn validate_request(request: &PluginProcessRequest) -> Result<(), String> {
    match request {
        PluginProcessRequest::Initialize { plugin_id, .. } => {
            require_nonempty(plugin_id, "pluginId")
        }
        PluginProcessRequest::ProviderCatalogKey {
            request_id,
            provider_id,
            ..
        } => {
            require_nonempty(request_id, "requestId")?;
            require_nonempty(provider_id, "providerId")
        }
        PluginProcessRequest::Invoke {
            request_id, method, ..
        } => {
            require_nonempty(request_id, "requestId")?;
            require_nonempty(method, "method")
        }
        PluginProcessRequest::ProviderConnect {
            provider_id,
            connection_id,
            request,
        } => {
            require_nonempty(provider_id, "providerId")?;
            require_nonempty(connection_id, "connectionId")?;
            if request.versions.contains(&0) {
                return Err("versions must be positive".into());
            }
            Ok(())
        }
        PluginProcessRequest::ProviderSend {
            connection_id,
            acceptance_id,
            input,
        } => {
            require_nonempty(connection_id, "connectionId")?;
            require_nonempty(acceptance_id, "acceptanceId")?;
            input.validate()
        }
        PluginProcessRequest::ProviderClose { connection_id } => {
            require_nonempty(connection_id, "connectionId")
        }
        PluginProcessRequest::Hook { .. }
        | PluginProcessRequest::HookCancel { .. }
        | PluginProcessRequest::UsageIdentify { .. }
        | PluginProcessRequest::UsageFetch { .. }
        | PluginProcessRequest::UsageDiscover { .. }
        | PluginProcessRequest::Shutdown {}
        | PluginProcessRequest::PaseoFrame { .. }
        | PluginProcessRequest::PaseoClose {} => Ok(()),
    }
}

fn validate_message(message: &PluginProcessMessage) -> Result<(), String> {
    match message {
        PluginProcessMessage::Ready {
            providers,
            usage_sources,
            ..
        } => {
            for provider in providers {
                require_nonempty(&provider.id, "provider.id")?;
                require_nonempty(&provider.label, "provider.label")?;
            }
            for source in usage_sources.iter().flatten() {
                require_nonempty(&source.id, "usageSource.id")?;
                require_nonempty(&source.label, "usageSource.label")?;
            }
            Ok(())
        }
        PluginProcessMessage::Result { request_id, .. }
        | PluginProcessMessage::Error { request_id, .. } => {
            require_nonempty(request_id, "requestId")
        }
        PluginProcessMessage::ProviderConnected {
            connection_id,
            version,
            ..
        } => {
            require_nonempty(connection_id, "connectionId")?;
            if *version == 0 {
                return Err("version must be positive".into());
            }
            Ok(())
        }
        PluginProcessMessage::ProviderConnectFailed { connection_id, .. }
        | PluginProcessMessage::ProviderClosed { connection_id, .. } => {
            require_nonempty(connection_id, "connectionId")
        }
        PluginProcessMessage::ProviderEvent {
            connection_id,
            event,
        } => {
            require_nonempty(connection_id, "connectionId")?;
            event.validate()
        }
        PluginProcessMessage::ProviderAccepted {
            connection_id,
            acceptance_id,
        }
        | PluginProcessMessage::ProviderRejected {
            connection_id,
            acceptance_id,
            ..
        } => {
            require_nonempty(connection_id, "connectionId")?;
            require_nonempty(acceptance_id, "acceptanceId")
        }
        PluginProcessMessage::SettingsChanged { .. }
        | PluginProcessMessage::HooksChanged { .. }
        | PluginProcessMessage::Fatal { .. }
        | PluginProcessMessage::PaseoFrame { .. }
        | PluginProcessMessage::PaseoClose {} => Ok(()),
    }
}
