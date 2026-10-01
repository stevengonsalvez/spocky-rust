use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{PluginCatalogEntry, PluginId};

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PluginSourceIdentityWire {
    Directory {
        path: String,
    },
    Git {
        remote: String,
        #[serde(rename = "pluginPath")]
        plugin_path: String,
    },
    Npm {
        #[serde(rename = "packageName")]
        package_name: String,
        #[serde(rename = "pluginPath")]
        plugin_path: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallationWire {
    pub identity: PluginSourceIdentityWire,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginNpmInstallation {
    pub package_name: String,
    pub requested_spec: String,
    pub version: String,
    pub integrity: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginRuntimeStatus {
    Running,
    Disabled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginLegacySource {
    Directory,
    Git,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginListItem {
    pub id: PluginId,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<String>,
    pub path: String,
    pub enabled: bool,
    pub status: PluginRuntimeStatus,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub source: Option<PluginLegacySource>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub npm: Option<PluginNpmInstallation>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub installation: Option<PluginInstallationWire>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub remote: Option<String>,
    #[serde(
        rename = "ref",
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_ref: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub commit: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginLogStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginLogEntry {
    pub sequence: u64,
    pub timestamp: String,
    pub stream: PluginLogStream,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PluginUpdateSelection {
    Git { r#ref: String },
    Npm { version: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PluginUpdateTarget {
    Git {
        commit: String,
    },
    Npm {
        version: String,
        resolved: String,
        integrity: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateExpected {
    pub identity: PluginSourceIdentityWire,
    pub installation_root: String,
    pub revision: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginUpdateProposal {
    pub id: PluginId,
    pub expected: PluginUpdateExpected,
    pub target: PluginUpdateTarget,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum PluginDaemonRequest {
    #[serde(rename = "plugin.catalog.get.request")]
    CatalogGet {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "plugin.list.request")]
    List {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "plugin.logs.get.request")]
    LogsGet {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
    },
    #[serde(rename = "plugin.directory.install.request")]
    DirectoryInstall {
        #[serde(rename = "requestId")]
        request_id: String,
        path: String,
        #[serde(
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<PluginId>,
    },
    #[serde(rename = "plugin.directory.inspect.request")]
    DirectoryInspect {
        #[serde(rename = "requestId")]
        request_id: String,
        path: String,
    },
    #[serde(rename = "plugin.source.install.request")]
    SourceInstall {
        #[serde(rename = "requestId")]
        request_id: String,
        source: String,
        #[serde(
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<PluginId>,
        #[serde(
            rename = "ref",
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        source_ref: Option<String>,
        #[serde(
            rename = "pluginPath",
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        plugin_path: Option<String>,
    },
    #[serde(rename = "plugin.source.status.request")]
    SourceStatus {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(
            rename = "pluginId",
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        plugin_id: Option<PluginId>,
    },
    #[serde(rename = "plugin.source.update.preview.request")]
    SourceUpdatePreview {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(
            rename = "pluginId",
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        plugin_id: Option<PluginId>,
        #[serde(
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        target: Option<PluginUpdateSelection>,
    },
    #[serde(rename = "plugin.source.update.apply.request")]
    SourceUpdateApply {
        #[serde(rename = "requestId")]
        request_id: String,
        proposals: Vec<PluginUpdateProposal>,
    },
    #[serde(rename = "plugin.source.update.request")]
    SourceUpdate {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(
            rename = "pluginId",
            default,
            deserialize_with = "optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        plugin_id: Option<PluginId>,
    },
    #[serde(rename = "plugin.reload.request")]
    Reload {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
    },
    #[serde(rename = "plugin.enable.request")]
    Enable {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
    },
    #[serde(rename = "plugin.disable.request")]
    Disable {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
    },
    #[serde(rename = "plugin.remove.request")]
    Remove {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
    },
    #[serde(rename = "plugin.rpc.invoke.request")]
    RpcInvoke {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
        method: String,
        input: Value,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSourceStatusItem {
    pub id: PluginId,
    pub source: PluginLegacySource,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub npm: Option<PluginNpmInstallation>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub installation: Option<PluginInstallationWire>,
    pub path: String,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub remote: Option<String>,
    #[serde(
        rename = "ref",
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_ref: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_commit: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub latest_commit: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub commits_behind: Option<u64>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub update_available: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSourceUpdateItem {
    pub id: PluginId,
    pub previous_commit: String,
    pub current_commit: String,
    pub commits: u64,
    pub updated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginUpdatePreviewOutcome {
    Update,
    Current,
    InstalledNewer,
    Local,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginUpdatePreview {
    pub id: PluginId,
    pub outcome: PluginUpdatePreviewOutcome,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current: Option<PluginInstallationWire>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<PluginUpdateTarget>,
    pub links: Vec<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub proposal: Option<PluginUpdateProposal>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginUpdateResultOutcome {
    Updated,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginUpdateResult {
    pub id: PluginId,
    pub outcome: PluginUpdateResultOutcome,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub plugin: Option<PluginListItem>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub error: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum PluginDaemonResponse {
    #[serde(rename = "plugin.catalog.get.response")]
    CatalogGet { payload: CatalogPayload },
    #[serde(rename = "plugin.list.response")]
    List { payload: ListPayload },
    #[serde(rename = "plugin.logs.get.response")]
    LogsGet { payload: LogsPayload },
    #[serde(rename = "plugin.directory.install.response")]
    DirectoryInstall { payload: PluginPayload },
    #[serde(rename = "plugin.directory.inspect.response")]
    DirectoryInspect { payload: InspectPayload },
    #[serde(rename = "plugin.source.install.response")]
    SourceInstall { payload: PluginPayload },
    #[serde(rename = "plugin.source.status.response")]
    SourceStatus { payload: SourceStatusPayload },
    #[serde(rename = "plugin.source.update.preview.response")]
    SourceUpdatePreview { payload: UpdatePreviewPayload },
    #[serde(rename = "plugin.source.update.apply.response")]
    SourceUpdateApply { payload: UpdateApplyPayload },
    #[serde(rename = "plugin.source.update.response")]
    SourceUpdate { payload: SourceUpdatePayload },
    #[serde(rename = "plugin.reload.response")]
    Reload { payload: PluginPayload },
    #[serde(rename = "plugin.enable.response")]
    Enable { payload: PluginPayload },
    #[serde(rename = "plugin.disable.response")]
    Disable { payload: PluginPayload },
    #[serde(rename = "plugin.remove.response")]
    Remove { payload: RequestPayload },
    #[serde(rename = "plugin.rpc.invoke.response")]
    RpcInvoke { payload: RpcInvokePayload },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPayload {
    pub request_id: String,
    pub plugins: Vec<PluginCatalogEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPayload {
    pub request_id: String,
    pub plugins: Vec<PluginListItem>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsPayload {
    pub request_id: String,
    pub plugin_id: PluginId,
    pub entries: Vec<PluginLogEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPayload {
    pub request_id: String,
    pub plugin: PluginListItem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectPayload {
    pub request_id: String,
    pub id: PluginId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatusPayload {
    pub request_id: String,
    pub plugins: Vec<PluginSourceStatusItem>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePreviewPayload {
    pub request_id: String,
    pub plugins: Vec<PluginUpdatePreview>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApplyPayload {
    pub request_id: String,
    pub plugins: Vec<PluginUpdateResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceUpdatePayload {
    pub request_id: String,
    pub plugins: Vec<PluginSourceUpdateItem>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPayload {
    pub request_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcInvokePayload {
    pub request_id: String,
    pub output: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginRpcError {
    pub r#type: PluginRpcErrorType,
    pub payload: PluginRpcErrorPayload,
}

impl PluginRpcError {
    #[must_use]
    pub fn handler(
        request_id: impl Into<String>,
        request_type: impl Into<String>,
        error: &str,
    ) -> Self {
        Self {
            r#type: PluginRpcErrorType::RpcError,
            payload: PluginRpcErrorPayload {
                request_id: request_id.into(),
                request_type: Some(request_type.into()),
                error: format!("Request failed: {error}"),
                code: Some("handler_error".to_owned()),
            },
        }
    }

    #[must_use]
    pub fn access_denied(request_id: impl Into<String>, request_type: impl Into<String>) -> Self {
        let request_type = request_type.into();
        Self {
            r#type: PluginRpcErrorType::RpcError,
            payload: PluginRpcErrorPayload {
                request_id: request_id.into(),
                error: format!("Session is not authorized for {request_type}"),
                request_type: Some(request_type),
                code: Some("access_denied".to_owned()),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PluginRpcErrorType {
    #[serde(rename = "rpc_error")]
    RpcError,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRpcErrorPayload {
    pub request_id: String,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub request_type: Option<String>,
    pub error: String,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginNotification {
    pub r#type: PluginNotificationType,
    pub payload: PluginNotificationPayload,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PluginNotificationType {
    #[serde(rename = "status")]
    Status,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status")]
pub enum PluginNotificationPayload {
    #[serde(rename = "plugin_catalog_changed")]
    CatalogChanged {
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
    },
    #[serde(rename = "plugin_settings_changed")]
    SettingsChanged {
        #[serde(rename = "pluginId")]
        plugin_id: PluginId,
        #[serde(rename = "settingsId")]
        settings_id: String,
    },
}
