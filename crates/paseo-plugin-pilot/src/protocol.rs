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
                history,
                ..
            } => {
                require_nonempty(request_id, "requestId")?;
                require_nonempty(session_id, "sessionId")?;
                if !matches!(history.as_str(), "replay" | "skip") {
                    return Err("history must be replay or skip".into());
                }
                Ok(())
            }
            Self::SessionPrompt { session_id, .. } => require_nonempty(session_id, "sessionId"),
            Self::SessionInterrupt {
                request_id,
                session_id,
            }
            | Self::SessionUsageReference {
                request_id,
                session_id,
            }
            | Self::SessionConfigure {
                request_id,
                session_id,
                ..
            }
            | Self::SessionClose {
                request_id,
                session_id,
            } => {
                require_nonempty(request_id, "requestId")?;
                require_nonempty(session_id, "sessionId")
            }
            Self::SessionPermission {
                session_id,
                permission_id,
                ..
            } => {
                require_nonempty(session_id, "sessionId")?;
                require_nonempty(permission_id, "permissionId")
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
            Self::SessionArchive { request_id, .. } | Self::SessionUnarchive { request_id, .. } => {
                require_nonempty(request_id, "requestId")
            }
        }
    }
}

impl ProviderEvent {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Catalog { request_id, .. }
            | Self::Sessions { request_id, .. }
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
            | Self::SessionPersistence { session_id, .. }
            | Self::SessionUsage { session_id, .. }
            | Self::SessionConfig { session_id, .. }
            | Self::SessionCommands { session_id, .. }
            | Self::SessionPermission { session_id, .. }
            | Self::SessionNotice { session_id, .. }
            | Self::TimelineItem { session_id, .. } => require_nonempty(session_id, "sessionId"),
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
