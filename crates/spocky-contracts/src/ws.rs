//! WebSocket-level frames on `/ws`: hello, application ping and pong,
//! `hello.rejected`, and the `server_info` status sent after an accepted hello.
//!
//! Sources at Paseo `5de45e2`: `packages/protocol/src/messages.ts`
//! (`WSHelloMessageSchema`, `WSHelloRejectedMessageSchema`,
//! `ServerInfoStatusPayloadSchema`) and `packages/server/src/server/
//! websocket-server.ts` (`buildServerInfoStatusPayload`, `rejectHello`).

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::field::optional;
use crate::number::Int;
use crate::text::NonEmptyString;

/// `WS_PROTOCOL_VERSION` in `websocket-server.ts`.
pub const WS_PROTOCOL_VERSION: i64 = 1;

/// `WSHelloMessageSchema.clientType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientType {
    Mobile,
    Browser,
    Cli,
    Mcp,
    Hub,
}

/// `WSHelloMessageSchema.auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HelloAuth {
    #[serde(rename = "password")]
    Password { password: String },
    #[serde(rename = "localCredential")]
    LocalCredential { token: String },
}

/// `BROWSER_AUTOMATION_COMMAND_NAMES` in `browser-automation/rpc-schemas.ts`.
pub const BROWSER_AUTOMATION_COMMAND_NAMES: [&str; 22] = [
    "list_tabs",
    "new_tab",
    "snapshot",
    "click",
    "fill",
    "wait",
    "type",
    "keypress",
    "navigate",
    "back",
    "forward",
    "reload",
    "screenshot",
    "upload",
    "select",
    "hover",
    "drag",
    "logs",
    "evaluate",
    "scroll",
    "resize",
    "close_tab",
];

/// `BrowserAutomationHostCapabilitySchema`: a passthrough object whose
/// `supportedCommands` keeps known names once each, in first-seen order, and
/// must keep at least one; `hostKind` defaults to `"browser host"`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BrowserHostCapability {
    #[serde(rename = "supportedCommands")]
    pub supported_commands: Vec<String>,
    #[serde(rename = "hostKind")]
    pub host_kind: NonEmptyString,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl<'de> Deserialize<'de> for BrowserHostCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "supportedCommands")]
            supported_commands: Vec<NonEmptyString>,
            #[serde(rename = "hostKind", default = "default_host_kind")]
            host_kind: NonEmptyString,
            #[serde(flatten)]
            extra: Map<String, Value>,
        }

        fn default_host_kind() -> NonEmptyString {
            NonEmptyString::new("browser host".to_owned()).unwrap_or_else(|| unreachable!())
        }

        let raw = Raw::deserialize(deserializer)?;
        let mut supported_commands: Vec<String> = Vec::new();
        for command in raw.supported_commands {
            let command = command.into_string();
            if BROWSER_AUTOMATION_COMMAND_NAMES.contains(&command.as_str())
                && !supported_commands.contains(&command)
            {
                supported_commands.push(command);
            }
        }
        if supported_commands.is_empty() {
            return Err(de::Error::custom(
                "supportedCommands must include at least one known browser automation command",
            ));
        }
        Ok(Self {
            supported_commands,
            host_kind: raw.host_kind,
            extra: raw.extra,
        })
    }
}

/// `WSHelloMessageSchema.capabilities`: known flags in shape order, then
/// unknown keys in input order (`.passthrough()`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HelloCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub voice: Option<bool>,
    #[serde(
        rename = "hello_rejection",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub hello_rejection: Option<bool>,
    #[serde(
        rename = "pushNotifications",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub push_notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub explicit_event_subscriptions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub all_providers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub reasoning_merge_enum: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub selective_agent_timeline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub custom_mode_icons: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub terminal_reflowable_snapshot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub provider_subagents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub project_updates: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub compact_provider_snapshots: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub provider_snapshot_references: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub timeline_replacement_invalidation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub timeline_notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub browser_host: Option<BrowserHostCapability>,
    /// Keys outside the shape, such as `owned_subscriptions`, kept as sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl HelloCapabilities {
    /// Reads a capability flag the way the daemon does: `capabilities[key] === true`.
    #[must_use]
    pub fn is_enabled(&self, key: &str) -> bool {
        let known = match key {
            "voice" => self.voice,
            "hello_rejection" => self.hello_rejection,
            "pushNotifications" => self.push_notifications,
            "explicit_event_subscriptions" => self.explicit_event_subscriptions,
            "all_providers" => self.all_providers,
            "reasoning_merge_enum" => self.reasoning_merge_enum,
            "selective_agent_timeline" => self.selective_agent_timeline,
            "custom_mode_icons" => self.custom_mode_icons,
            "terminal_reflowable_snapshot" => self.terminal_reflowable_snapshot,
            "provider_subagents" => self.provider_subagents,
            "project_updates" => self.project_updates,
            "compact_provider_snapshots" => self.compact_provider_snapshots,
            "provider_snapshot_references" => self.provider_snapshot_references,
            "timeline_replacement_invalidation" => self.timeline_replacement_invalidation,
            "timeline_notifications" => self.timeline_notifications,
            _ => return self.extra.get(key) == Some(&Value::Bool(true)),
        };
        known == Some(true)
    }
}

/// `WSHelloMessageSchema` without its `type` tag, in zod output order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(rename = "clientId")]
    pub client_id: NonEmptyString,
    #[serde(rename = "clientType")]
    pub client_type: ClientType,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: Int,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub auth: Option<HelloAuth>,
    #[serde(
        rename = "appVersion",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub capabilities: Option<HelloCapabilities>,
}

/// `WSHelloRejectedMessageSchema.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloRejectedReason {
    PasswordRequired,
    IncorrectPassword,
    IncompatibleProtocol,
}

/// `WSHelloRejectedMessageSchema.accepts` items: only `"password"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelloAcceptedAuth {
    Password,
}

/// `{ type: "hello.rejected", reason, accepts: ["password"] }` from `rejectHello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloRejected {
    pub reason: HelloRejectedReason,
    pub accepts: Vec<HelloAcceptedAuth>,
}

impl HelloRejected {
    /// The frame `rejectHello` sends.
    #[must_use]
    pub fn new(reason: HelloRejectedReason) -> Self {
        Self {
            reason,
            accepts: vec![HelloAcceptedAuth::Password],
        }
    }
}

/// `DAEMON_PERMISSIONS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonPermission {
    #[serde(rename = "daemon.read")]
    DaemonRead,
    #[serde(rename = "daemon.manage")]
    DaemonManage,
    #[serde(rename = "tunnel.manage")]
    TunnelManage,
    #[serde(rename = "access.manage")]
    AccessManage,
    #[serde(rename = "workspace.read")]
    WorkspaceRead,
    #[serde(rename = "workspace.write")]
    WorkspaceWrite,
    #[serde(rename = "workspace.manage")]
    WorkspaceManage,
    #[serde(rename = "automation.manage")]
    AutomationManage,
    #[serde(rename = "hub.execute")]
    HubExecute,
}

impl DaemonPermission {
    /// `DAEMON_PERMISSIONS` in declaration order.
    pub const ALL: [Self; 9] = [
        Self::DaemonRead,
        Self::DaemonManage,
        Self::TunnelManage,
        Self::AccessManage,
        Self::WorkspaceRead,
        Self::WorkspaceWrite,
        Self::WorkspaceManage,
        Self::AutomationManage,
        Self::HubExecute,
    ];
}

/// `ServerCapabilityStateSchema`, built as `{ enabled, reason }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCapabilityState {
    pub enabled: bool,
    pub reason: String,
}

/// `ServerVoiceCapabilitiesSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerVoiceCapabilities {
    pub dictation: ServerCapabilityState,
    pub voice: ServerCapabilityState,
}

/// `serverCapabilities` as the daemon builds it: `{ voice }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub voice: ServerVoiceCapabilities,
}

macro_rules! server_features {
    ($($(#[$attr:meta])* $field:ident: $ty:ty => $key:literal,)*) => {
        /// `server_info.features` in `buildServerInfoStatusPayload` construction
        /// order. `Option` fields are spread in only when their gate is on.
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct ServerFeatures {
            $($(#[$attr])* #[serde(rename = $key)] pub $field: $ty,)*
        }
    };
}

server_features! {
    usage_sources: bool => "usageSources",
    owned_subscriptions: bool => "ownedSubscriptions",
    agent_request_receipts: bool => "agentRequestReceipts",
    workspace_request_receipts: bool => "workspaceRequestReceipts",
    creation_lifecycle: bool => "creationLifecycle",
    hub_agent_rpc: bool => "hubAgentRpc",
    directory_sync: bool => "directorySync",
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    workspace_labels: Option<bool> => "workspaceLabels",
    workspace_setup_run: bool => "workspaceSetupRun",
    providers_snapshot: bool => "providersSnapshot",
    providers_snapshot_cwd: bool => "providersSnapshotCwd",
    checkout_forge_set_auto_merge: bool => "checkoutForgeSetAutoMerge",
    checkout_github_set_auto_merge: bool => "checkoutGithubSetAutoMerge",
    github_check_details: bool => "githubCheckDetails",
    forge_check_details: bool => "forgeCheckDetails",
    forge_search: bool => "forgeSearch",
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    daemon_status_rpc: Option<bool> => "daemonStatusRpc",
    daemon_config_reload: bool => "daemonConfigReload",
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    relay_config: Option<bool> => "relayConfig",
    push_token_revocation: bool => "pushTokenRevocation",
    plugins: bool => "plugins",
    plugin_management: bool => "pluginManagement",
    plugin_git_management: bool => "pluginGitManagement",
    plugin_source_installation: bool => "pluginSourceInstallation",
    plugin_source_updates: bool => "pluginSourceUpdates",
    plugin_logs: bool => "pluginLogs",
    plugin_themes: bool => "pluginThemes",
    plugin_settings: bool => "pluginSettings",
    plugin_timeline_items: bool => "pluginTimelineItems",
    skill_management: bool => "skillManagement",
    terminal_restore_modes: bool => "terminal-restore-modes",
    terminal_input_mode_replay: bool => "terminal-input-mode-replay",
    terminal_size_ownership: bool => "terminal-size-ownership",
    workspace_terminals: bool => "workspaceTerminals",
    rewind: bool => "rewind",
    agent_timeline_prompt_index: bool => "agentTimelinePromptIndex",
    agent_history_search: bool => "agentHistorySearch",
    checkout_refresh: bool => "checkoutRefresh",
    workspace_multiplicity: bool => "workspaceMultiplicity",
    project_remove: bool => "projectRemove",
    project_add: bool => "projectAdd",
    project_list: bool => "projectList",
    worktree_restore: bool => "worktreeRestore",
    workspace_recovery: bool => "workspaceRecovery",
    workspace_file_editing: bool => "workspaceFileEditing",
    provider_usage_list: bool => "providerUsageList",
    agent_detach: bool => "agentDetach",
    agent_thinking_update: bool => "agentThinkingUpdate",
    daemon_diagnostics: bool => "daemonDiagnostics",
    daemon_self_update: bool => "daemonSelfUpdate",
    agent_fork_context: bool => "agentForkContext",
    agent_fork_context_cursor: bool => "agentForkContextCursor",
    provider_subagents: bool => "providerSubagents",
    projected_subagent_timeline: bool => "projectedSubagentTimeline",
    provider_subagent_nesting: bool => "providerSubagentNesting",
    workspace_pinning: bool => "workspacePinning",
    workspace_mark_unread: bool => "workspaceMarkUnread",
    hub_relationship: bool => "hubRelationship",
    project_github_clone: bool => "projectGithubClone",
    workspace_github_repository_search: bool => "workspaceGithubRepositorySearch",
    project_create_directory: bool => "projectCreateDirectory",
    commits_list: bool => "commitsList",
    commit_base_classification: bool => "commitBaseClassification",
    provider_removal: bool => "providerRemoval",
    import_session_workspace_target: bool => "importSessionWorkspaceTarget",
    import_session_search: bool => "importSessionSearch",
    forge_providers: bool => "forgeProviders",
    selective_agent_timeline: bool => "selectiveAgentTimeline",
    explicit_event_subscriptions: bool => "explicitEventSubscriptions",
    canonical_submitted_prompts: bool => "canonicalSubmittedPrompts",
    stable_project_identity: bool => "stableProjectIdentity",
    workspace_script_management: bool => "workspaceScriptManagement",
    project_custom_icon: bool => "projectCustomIcon",
    fs_entry_ops: bool => "fsEntryOps",
    fs_entry_duplicate: bool => "fsEntryDuplicate",
    checkout_discard_changes: bool => "checkoutDiscardChanges",
    agent_profiles: bool => "agentProfiles",
    agent_config_apply: bool => "agentConfigApply",
}

/// Daemon inputs that vary the advertised `features`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ServerFeatureGates {
    /// `this.workspaceLabelService` is set.
    pub workspace_labels: bool,
    /// `this.advertiseDaemonStatusRpc`, on by default.
    pub daemon_status_rpc: bool,
    /// `this.advertiseRelayConfig`, on by default.
    pub relay_config: bool,
    /// `daemonRuntimeConfig.desktopManaged === true`.
    pub desktop_managed: bool,
}

impl ServerFeatures {
    /// The `features` object `buildServerInfoStatusPayload` builds.
    #[must_use]
    pub fn advertised(gates: ServerFeatureGates) -> Self {
        let when = |on: bool| on.then_some(true);
        Self {
            usage_sources: true,
            owned_subscriptions: true,
            agent_request_receipts: true,
            workspace_request_receipts: true,
            creation_lifecycle: true,
            hub_agent_rpc: true,
            directory_sync: true,
            workspace_labels: when(gates.workspace_labels),
            workspace_setup_run: true,
            providers_snapshot: true,
            providers_snapshot_cwd: true,
            checkout_forge_set_auto_merge: true,
            checkout_github_set_auto_merge: true,
            github_check_details: true,
            forge_check_details: true,
            forge_search: true,
            daemon_status_rpc: when(gates.daemon_status_rpc),
            daemon_config_reload: true,
            relay_config: when(gates.relay_config),
            push_token_revocation: true,
            plugins: true,
            plugin_management: true,
            plugin_git_management: true,
            plugin_source_installation: true,
            plugin_source_updates: true,
            plugin_logs: true,
            plugin_themes: true,
            plugin_settings: true,
            plugin_timeline_items: true,
            skill_management: true,
            terminal_restore_modes: true,
            terminal_input_mode_replay: true,
            terminal_size_ownership: true,
            workspace_terminals: true,
            rewind: true,
            agent_timeline_prompt_index: true,
            agent_history_search: true,
            checkout_refresh: true,
            workspace_multiplicity: true,
            project_remove: true,
            project_add: true,
            project_list: true,
            worktree_restore: true,
            workspace_recovery: true,
            workspace_file_editing: true,
            provider_usage_list: true,
            agent_detach: true,
            agent_thinking_update: true,
            daemon_diagnostics: true,
            daemon_self_update: !gates.desktop_managed,
            agent_fork_context: true,
            agent_fork_context_cursor: true,
            provider_subagents: true,
            projected_subagent_timeline: true,
            provider_subagent_nesting: true,
            workspace_pinning: true,
            workspace_mark_unread: true,
            hub_relationship: true,
            project_github_clone: true,
            workspace_github_repository_search: true,
            project_create_directory: true,
            commits_list: true,
            commit_base_classification: true,
            provider_removal: true,
            import_session_workspace_target: true,
            import_session_search: true,
            forge_providers: true,
            selective_agent_timeline: true,
            explicit_event_subscriptions: true,
            canonical_submitted_prompts: true,
            stable_project_identity: true,
            workspace_script_management: true,
            project_custom_icon: true,
            fs_entry_ops: true,
            fs_entry_duplicate: true,
            checkout_discard_changes: true,
            agent_profiles: true,
            agent_config_apply: true,
        }
    }
}

/// The `server_info` status payload without its `status` tag, in
/// `buildServerInfoStatusPayload` construction order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: Int,
    #[serde(rename = "serverId")]
    pub server_id: String,
    /// `os.hostname()`.
    pub hostname: String,
    pub version: String,
    pub permissions: Vec<DaemonPermission>,
    #[serde(rename = "desktopManaged")]
    pub desktop_managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub capabilities: Option<ServerCapabilities>,
    pub features: ServerFeatures,
}

/// Frames a client sends: `WSInboundMessageSchema` minus the session envelope,
/// which the session module adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsControlInbound {
    /// Application-level `{ "type": "ping" }`; there are no WebSocket ping frames.
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "hello")]
    Hello(Box<Hello>),
    #[serde(rename = "recording_state")]
    RecordingState {
        #[serde(rename = "isRecording")]
        is_recording: bool,
    },
}

/// Frames the daemon sends outside the session envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsControlOutbound {
    /// `{ "type": "pong" }`, the reply to an application ping.
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "hello.rejected")]
    HelloRejected(HelloRejected),
}

#[cfg(test)]
mod tests {
    use super::{
        HelloRejected, HelloRejectedReason, ServerFeatureGates, ServerFeatures, WsControlInbound,
        WsControlOutbound,
    };

    #[test]
    fn control_frames_match_daemon_literals() {
        let pong = serde_json::to_string(&WsControlOutbound::Pong).unwrap();
        assert_eq!(pong, r#"{"type":"pong"}"#);
        let rejected = WsControlOutbound::HelloRejected(HelloRejected::new(
            HelloRejectedReason::IncompatibleProtocol,
        ));
        assert_eq!(
            serde_json::to_string(&rejected).unwrap(),
            r#"{"type":"hello.rejected","reason":"incompatible_protocol","accepts":["password"]}"#
        );
    }

    #[test]
    fn gated_features_are_spread_in_place() {
        let all_off = ServerFeatures::advertised(ServerFeatureGates {
            workspace_labels: false,
            daemon_status_rpc: false,
            relay_config: false,
            desktop_managed: true,
        });
        let text = serde_json::to_string(&all_off).unwrap();
        assert!(!text.contains("workspaceLabels"));
        assert!(!text.contains("daemonStatusRpc"));
        assert!(!text.contains("relayConfig"));
        assert!(text.contains(r#""daemonSelfUpdate":false"#));
        assert!(text.starts_with(r#"{"usageSources":true,"ownedSubscriptions":true"#));
        assert!(text.ends_with(r#""agentProfiles":true,"agentConfigApply":true}"#));
    }

    #[test]
    fn hello_rejects_empty_client_id_and_unknown_client_type() {
        let parse = |text: &str| serde_json::from_str::<WsControlInbound>(text);
        assert!(
            parse(r#"{"type":"hello","clientId":"","clientType":"cli","protocolVersion":1}"#)
                .is_err()
        );
        assert!(
            parse(r#"{"type":"hello","clientId":"c","clientType":"tv","protocolVersion":1}"#)
                .is_err()
        );
        assert!(
            parse(r#"{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1.5}"#)
                .is_err()
        );
        assert!(
            parse(r#"{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1}"#)
                .is_ok()
        );
    }
}
