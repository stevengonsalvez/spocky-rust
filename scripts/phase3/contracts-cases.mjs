// Golden cases for spocky-contracts, captured by contracts-capture.mjs.
//
// Each case: { id, direction, source, input | raw }.
//   direction  "inbound" (client to daemon) or "outbound" (daemon to client)
//   source     where the frame shape comes from at Paseo 5de45e2
//   input      a JS value passed through JSON.stringify
//   raw        exact JSON text, for key orders a JS literal cannot express
//
// Inbound inputs model what clients send; the fixture records what the
// daemon's zod parse produces. Outbound inputs model the daemon's exact
// JSON.stringify text in construction order; the fixture records what the
// client's zod-aot validator and plain zod make of it.

const CLI_CAPABILITIES = {
  hello_rejection: true,
  owned_subscriptions: true,
  all_providers: true,
  selective_agent_timeline: true,
  reasoning_merge_enum: true,
  custom_mode_icons: true,
  terminal_reflowable_snapshot: true,
  provider_subagents: true,
  projected_subagent_timeline: true,
  project_updates: true,
  compact_provider_snapshots: true,
  provider_snapshot_references: true,
  timeline_replacement_invalidation: true,
  timeline_notifications: true,
  plugin_timeline_items: true,
  workspace_setup_blocked: true,
  explicit_event_subscriptions: true,
};

const HELLO_SOURCE =
  "packages/client/src/daemon-client.ts:6133-6144; capabilities packages/client/src/connection/index.ts:127-145";

const SERVER_FEATURES = {
  usageSources: true,
  ownedSubscriptions: true,
  agentRequestReceipts: true,
  workspaceRequestReceipts: true,
  creationLifecycle: true,
  hubAgentRpc: true,
  directorySync: true,
  workspaceLabels: true,
  workspaceSetupRun: true,
  providersSnapshot: true,
  providersSnapshotCwd: true,
  checkoutForgeSetAutoMerge: true,
  checkoutGithubSetAutoMerge: true,
  githubCheckDetails: true,
  forgeCheckDetails: true,
  forgeSearch: true,
  daemonStatusRpc: true,
  daemonConfigReload: true,
  relayConfig: true,
  pushTokenRevocation: true,
  plugins: true,
  pluginManagement: true,
  pluginGitManagement: true,
  pluginSourceInstallation: true,
  pluginSourceUpdates: true,
  pluginLogs: true,
  pluginThemes: true,
  pluginSettings: true,
  pluginTimelineItems: true,
  skillManagement: true,
  "terminal-restore-modes": true,
  "terminal-input-mode-replay": true,
  "terminal-size-ownership": true,
  workspaceTerminals: true,
  rewind: true,
  agentTimelinePromptIndex: true,
  agentHistorySearch: true,
  checkoutRefresh: true,
  workspaceMultiplicity: true,
  projectRemove: true,
  projectAdd: true,
  projectList: true,
  worktreeRestore: true,
  workspaceRecovery: true,
  workspaceFileEditing: true,
  providerUsageList: true,
  agentDetach: true,
  agentThinkingUpdate: true,
  daemonDiagnostics: true,
  daemonSelfUpdate: true,
  agentForkContext: true,
  agentForkContextCursor: true,
  providerSubagents: true,
  projectedSubagentTimeline: true,
  providerSubagentNesting: true,
  workspacePinning: true,
  workspaceMarkUnread: true,
  hubRelationship: true,
  projectGithubClone: true,
  workspaceGithubRepositorySearch: true,
  projectCreateDirectory: true,
  commitsList: true,
  commitBaseClassification: true,
  providerRemoval: true,
  importSessionWorkspaceTarget: true,
  importSessionSearch: true,
  forgeProviders: true,
  selectiveAgentTimeline: true,
  explicitEventSubscriptions: true,
  canonicalSubmittedPrompts: true,
  stableProjectIdentity: true,
  workspaceScriptManagement: true,
  projectCustomIcon: true,
  fsEntryOps: true,
  fsEntryDuplicate: true,
  checkoutDiscardChanges: true,
  agentProfiles: true,
  agentConfigApply: true,
};

const PERMISSIONS = [
  "daemon.read",
  "daemon.manage",
  "tunnel.manage",
  "access.manage",
  "workspace.read",
  "workspace.write",
  "workspace.manage",
  "automation.manage",
  "hub.execute",
];

const SERVER_INFO_SOURCE =
  "packages/server/src/server/websocket-server.ts:1769-1946 buildServerInfoStatusPayload, createServerInfoMessage";

function serverInfo(overrides = {}) {
  return {
    type: "session",
    message: {
      type: "status",
      payload: {
        status: "server_info",
        protocolVersion: 1,
        serverId: "srv_golden",
        hostname: "golden-host",
        version: "0.10.0",
        permissions: PERMISSIONS,
        desktopManaged: false,
        ...overrides.beforeFeatures,
        features: { ...SERVER_FEATURES, ...overrides.features },
      },
    },
  };
}

export const CASES = [
  // Application ping and pong.
  {
    id: "ws.ping",
    direction: "inbound",
    source: "packages/client/src/daemon-client.ts:2244-2251",
    input: { type: "ping" },
  },
  {
    id: "ws.ping.extra_key_stripped",
    direction: "inbound",
    source: "WSPingMessageSchema strips unknown keys",
    input: { type: "ping", at: 1 },
  },
  {
    id: "ws.pong",
    direction: "outbound",
    source: "packages/server/src/server/websocket-server.ts:2319",
    input: { type: "pong" },
  },

  // Hello as the pinned CLI sends it.
  {
    id: "ws.hello.cli",
    direction: "inbound",
    source: HELLO_SOURCE,
    input: {
      type: "hello",
      clientId: "cid_00000000000000000000000000000000",
      clientType: "cli",
      protocolVersion: 1,
      capabilities: CLI_CAPABILITIES,
      appVersion: "0.10.0",
    },
  },
  {
    id: "ws.hello.cli_local_credential",
    direction: "inbound",
    source: HELLO_SOURCE,
    input: {
      type: "hello",
      clientId: "cid_00000000000000000000000000000000",
      clientType: "cli",
      protocolVersion: 1,
      auth: { kind: "localCredential", token: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" },
      capabilities: CLI_CAPABILITIES,
      appVersion: "0.10.0",
    },
  },
  {
    id: "ws.hello.password_and_known_caps_reordered",
    direction: "inbound",
    source: "WSHelloMessageSchema shape order and passthrough",
    input: {
      appVersion: "1",
      capabilities: { zeta: 1, timeline_notifications: false, voice: true, pushNotifications: true },
      auth: { password: "p", kind: "password", extra: true },
      protocolVersion: 2,
      clientType: "browser",
      clientId: "c",
      type: "hello",
    },
  },
  {
    id: "ws.hello.browser_host_filtered",
    direction: "inbound",
    source: "browser-automation/capabilities.ts BrowserAutomationHostCapabilitySchema",
    input: {
      type: "hello",
      clientId: "c",
      clientType: "browser",
      protocolVersion: 1,
      capabilities: {
        browser_host: { extra: "x", supportedCommands: ["nope", "click", "list_tabs", "click"] },
      },
    },
  },
  {
    id: "ws.hello.browser_host_no_known_command",
    direction: "inbound",
    source: "BrowserAutomationHostCapabilitySchema rejects an empty filtered list",
    input: {
      type: "hello",
      clientId: "c",
      clientType: "browser",
      protocolVersion: 1,
      capabilities: { browser_host: { supportedCommands: ["nope"], hostKind: "h" } },
    },
  },
  {
    id: "ws.hello.minimal",
    direction: "inbound",
    source: "WSHelloMessageSchema required keys only",
    input: { type: "hello", clientId: "c", clientType: "mcp", protocolVersion: 0 },
  },
  {
    id: "ws.hello.reject_empty_client_id",
    direction: "inbound",
    source: "WSHelloMessageSchema clientId min(1)",
    input: { type: "hello", clientId: "", clientType: "cli", protocolVersion: 1 },
  },
  {
    id: "ws.hello.reject_fractional_protocol",
    direction: "inbound",
    source: "WSHelloMessageSchema protocolVersion int",
    input: { type: "hello", clientId: "c", clientType: "cli", protocolVersion: 1.5 },
  },
  {
    id: "ws.hello.reject_null_app_version",
    direction: "inbound",
    source: "WSHelloMessageSchema appVersion optional, not nullable",
    input: { type: "hello", clientId: "c", clientType: "cli", protocolVersion: 1, appVersion: null },
  },
  {
    id: "ws.hello.reject_unknown_client_type",
    direction: "inbound",
    source: "WSHelloMessageSchema clientType enum",
    input: { type: "hello", clientId: "c", clientType: "tv", protocolVersion: 1 },
  },
  {
    id: "ws.hello.reject_non_boolean_capability",
    direction: "inbound",
    source: "WSHelloMessageSchema capability flags are booleans",
    input: {
      type: "hello",
      clientId: "c",
      clientType: "cli",
      protocolVersion: 1,
      capabilities: { hello_rejection: "yes" },
    },
  },
  {
    id: "ws.recording_state",
    direction: "inbound",
    source: "WSRecordingStateMessageSchema",
    input: { type: "recording_state", isRecording: true },
  },
  {
    id: "ws.reject_unknown_type",
    direction: "inbound",
    source: "WSInboundMessageSchema discriminated union",
    input: { type: "nope" },
  },

  // Rejection.
  {
    id: "ws.hello_rejected.password_required",
    direction: "outbound",
    source: "packages/server/src/server/websocket-server.ts:1706",
    input: { type: "hello.rejected", reason: "password_required", accepts: ["password"] },
  },
  {
    id: "ws.hello_rejected.incompatible_protocol",
    direction: "outbound",
    source: "packages/server/src/server/websocket-server.ts:1706",
    input: { type: "hello.rejected", reason: "incompatible_protocol", accepts: ["password"] },
  },

  // server_info.
  {
    id: "ws.server_info.default",
    direction: "outbound",
    source: SERVER_INFO_SOURCE,
    input: serverInfo(),
  },
  {
    id: "ws.server_info.desktop_managed_ungated",
    direction: "outbound",
    source: `${SERVER_INFO_SOURCE}; workspaceLabels, daemonStatusRpc, relayConfig gated off`,
    input: (() => {
      const frame = serverInfo({ features: { daemonSelfUpdate: false } });
      frame.message.payload.desktopManaged = true;
      const features = frame.message.payload.features;
      delete features.workspaceLabels;
      delete features.daemonStatusRpc;
      delete features.relayConfig;
      return frame;
    })(),
  },
  {
    id: "ws.server_info.voice_capabilities",
    direction: "outbound",
    source: `${SERVER_INFO_SOURCE}; capabilities websocket-server.ts:378-403`,
    input: serverInfo({
      beforeFeatures: {
        capabilities: {
          voice: {
            dictation: { enabled: true, reason: "" },
            voice: { enabled: false, reason: "Voice is disabled" },
          },
        },
      },
    }),
  },
];
