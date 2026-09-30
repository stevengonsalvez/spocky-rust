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
    versions: Vec<u64>,
    capabilities: Vec<String>,
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
    events: Vec<String>,
    before: Vec<String>,
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
        input: Value,
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
        event: Value,
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
            ..
        } => {
            require_nonempty(connection_id, "connectionId")?;
            require_nonempty(acceptance_id, "acceptanceId")
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
        | PluginProcessMessage::ProviderClosed { connection_id, .. }
        | PluginProcessMessage::ProviderEvent { connection_id, .. } => {
            require_nonempty(connection_id, "connectionId")
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
