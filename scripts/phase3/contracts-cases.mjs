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

function session(message) {
  return { type: "session", message };
}

const UUID = "123e4567-e89b-42d3-a456-426614174000";
const WKS = "wks_0123456789abcdef";

CASES.push(
  // Session ping.
  {
    id: "session.ping",
    direction: "inbound",
    source: "PingMessageSchema",
    input: session({ type: "ping", requestId: "r1", clientSentAt: 1730000000000 }),
  },
  {
    id: "session.ping.reject_fractional_client_sent_at",
    direction: "inbound",
    source: "PingMessageSchema clientSentAt int",
    input: session({ type: "ping", requestId: "r1", clientSentAt: 1.5 }),
  },
  {
    id: "session.reject_unknown_type",
    direction: "inbound",
    source: "SessionInboundMessageSchema discriminated union",
    input: session({ type: "no_such_request", requestId: "r1" }),
  },

  // Pinned CLI G1 requests (client literal before its zod parse).
  {
    id: "session.workspace_create.cli_directory",
    direction: "inbound",
    source: "packages/cli/src/commands/workspace/create.ts:46-50; client/src/creation/index.ts:116-168",
    input: session({
      type: "workspace.create.request",
      source: { kind: "directory", path: "/tmp/project" },
      requestId: "8b0c3c8e-1a2b-4c3d-8e9f-000000000001",
      idempotencyKey: "8b0c3c8e-1a2b-4c3d-8e9f-000000000002",
      subscribe: true,
    }),
  },
  {
    id: "session.agent_create.cli_run",
    direction: "inbound",
    source: "packages/cli/src/commands/agent/run.ts:695-707; client daemon-client.ts:6841-6883",
    input: session({
      type: "agent.create.request",
      provider: "codex",
      cwd: "/tmp/project",
      modeId: "full-access",
      config: { provider: "codex", cwd: "/tmp/project", modeId: "full-access" },
      workspaceId: WKS,
      initialPrompt: "Say hello",
      requestId: "8b0c3c8e-1a2b-4c3d-8e9f-000000000003",
      idempotencyKey: "8b0c3c8e-1a2b-4c3d-8e9f-000000000004",
      subscribe: true,
    }),
  },
  {
    id: "session.wait_for_finish.cli",
    direction: "inbound",
    source: "packages/cli/src/commands/agent/run.ts:712",
    input: session({ type: "wait_for_finish_request", requestId: "r", agentId: UUID }),
  },
  {
    id: "session.fetch_agent.cli",
    direction: "inbound",
    source: "packages/cli/src/commands/agent/logs.ts:103, inspect.ts:233",
    input: session({ type: "fetch_agent_request", requestId: "r", agentId: "123e" }),
  },
  {
    id: "session.fetch_agent_timeline.cli_logs",
    direction: "inbound",
    source: "packages/cli/src/utils/timeline.ts:16-21",
    input: session({
      type: "fetch_agent_timeline_request",
      requestId: "r",
      agentId: UUID,
      direction: "tail",
      limit: 0,
      projection: "projected",
    }),
  },
  {
    id: "session.fetch_agents.cli_ls_all",
    direction: "inbound",
    source: "packages/cli/src/commands/agent/ls.ts:143-157",
    input: session({
      type: "fetch_agents_request",
      requestId: "r",
      scope: "active",
      filter: { includeArchived: true },
    }),
  },
  {
    id: "session.fetch_agents.cli_ls_ready",
    direction: "inbound",
    source: "readiness probe paseo ls --json",
    input: session({ type: "fetch_agents_request", requestId: "r", scope: "active" }),
  },
  {
    id: "session.fetch_workspaces.cli_query",
    direction: "inbound",
    source: "packages/cli/src/commands/agent/run.ts:509-517",
    input: session({
      type: "fetch_workspaces_request",
      requestId: "r",
      filter: { query: WKS },
      page: { limit: 200 },
    }),
  },

  // Edge semantics.
  {
    id: "session.fetch_agents.full_filter",
    direction: "inbound",
    source: "FetchAgentsRequestMessageSchema, AgentDirectoryFilterSchema",
    raw: '{"type":"session","message":{"sync":{"afterSeq":0,"generation":"g"},"subscribe":{},"page":{"cursor":"c","limit":1},"sort":[{"direction":"desc","key":"updated_at"}],"filter":{"thinkingOptionId":null,"statuses":["idle","closed"],"labels":{"b":"1","10":"2","2":"3"},"projectKeys":["p"],"requiresAttention":false,"x":1},"scope":"active","requestId":"r","type":"fetch_agents_request"}}',
  },
  {
    id: "session.fetch_agents.reject_page_limit_201",
    direction: "inbound",
    source: "page.limit max(200)",
    input: session({ type: "fetch_agents_request", requestId: "r", page: { limit: 201 } }),
  },
  {
    id: "session.fetch_agents.reject_empty_cursor",
    direction: "inbound",
    source: "page.cursor min(1)",
    input: session({ type: "fetch_agents_request", requestId: "r", page: { limit: 1, cursor: "" } }),
  },
  {
    id: "session.fetch_agents.reject_scope_all",
    direction: "inbound",
    source: "scope enum active",
    input: session({ type: "fetch_agents_request", requestId: "r", scope: "all" }),
  },
  {
    id: "session.fetch_agents.reject_null_filter",
    direction: "inbound",
    source: "filter optional, not nullable",
    input: session({ type: "fetch_agents_request", requestId: "r", filter: null }),
  },
  {
    id: "session.fetch_workspaces.full",
    direction: "inbound",
    source: "FetchWorkspacesRequestMessageSchema",
    input: session({
      type: "fetch_workspaces_request",
      requestId: "r",
      sort: [{ key: "activity_at", direction: "asc" }],
      filter: { idPrefix: "w", projectId: "p" },
      subscribe: { subscriptionId: "s" },
      sync: {},
    }),
  },
  {
    id: "session.fetch_agent_timeline.cursor",
    direction: "inbound",
    source: "FetchAgentTimelineRequestMessageSchema",
    input: session({
      type: "fetch_agent_timeline_request",
      mergeWindow: true,
      cursor: { seq: 4, epoch: "e" },
      direction: "before",
      requestId: "r",
      agentId: "a",
    }),
  },
  {
    id: "session.fetch_agent_timeline.reject_negative_limit",
    direction: "inbound",
    source: "limit nonnegative",
    input: session({ type: "fetch_agent_timeline_request", requestId: "r", agentId: "a", limit: -1 }),
  },
  {
    id: "session.wait_for_finish.timeout",
    direction: "inbound",
    source: "WaitForFinishRequestSchema timeoutMs positive int",
    input: session({ type: "wait_for_finish_request", timeoutMs: 1000, agentId: "a", requestId: "r" }),
  },
  {
    id: "session.wait_for_finish.reject_zero_timeout",
    direction: "inbound",
    source: "WaitForFinishRequestSchema timeoutMs positive",
    input: session({ type: "wait_for_finish_request", requestId: "r", agentId: "a", timeoutMs: 0 }),
  },
  {
    id: "session.send_agent_message.lenient_attachments",
    direction: "inbound",
    source: "SendAgentMessageRequestSchema, AgentAttachmentsSchema normalize",
    input: session({
      type: "send_agent_message_request",
      requestId: "r",
      agentId: "a",
      text: "hi",
      messageId: "m",
      activeTurnBehavior: "steer",
      images: [{ mimeType: "image/png", data: "AA==", x: 1 }],
      attachments: [
        { type: "text", mimeType: "text/plain", text: "t", contextKind: "other", title: null },
        { type: "text", mimeType: "text/plain", contextKind: "chat_history", text: "h" },
        { type: "github_pr", mimeType: "application/github-pr", number: 0, title: "bad", url: "u" },
        { type: "forge_issue", mimeType: "application/paseo-forge-issue", number: 3, title: "i", url: "u" },
        { type: "uploaded_file", id: "f", fileName: "a.txt", mimeType: "text/plain", size: 0, path: "/a" },
        "junk",
      ],
    }),
  },
  {
    id: "session.send_agent_message.attachments_null_becomes_empty",
    direction: "inbound",
    source: "AgentAttachmentsSchema transform on null",
    input: session({ type: "send_agent_message_request", requestId: "r", agentId: "a", text: "", attachments: null }),
  },
  {
    id: "session.send_agent_message.attachments_object_becomes_empty",
    direction: "inbound",
    source: "AgentAttachmentsSchema transform on non-array",
    input: session({ type: "send_agent_message_request", requestId: "r", agentId: "a", text: "", attachments: { a: 1 } }),
  },
  {
    id: "session.send_agent_message.reject_unknown_behavior",
    direction: "inbound",
    source: "ActiveTurnBehaviorSchema",
    input: session({ type: "send_agent_message_request", requestId: "r", agentId: "a", text: "", activeTurnBehavior: "queue" }),
  },
  {
    id: "session.send_agent_message.review_attachment",
    direction: "inbound",
    source: "ReviewAttachmentSchema",
    input: session({
      type: "send_agent_message_request",
      requestId: "r",
      agentId: "a",
      text: "",
      attachments: [
        {
          comments: [
            {
              context: {
                lines: [{ content: "x", type: "add", newLineNumber: 2, oldLineNumber: null }],
                targetLine: { type: "context", content: "y", oldLineNumber: 1, newLineNumber: 1 },
                hunkHeader: "@@",
              },
              body: "b",
              lineNumber: 1,
              side: "new",
              filePath: "f",
            },
          ],
          baseRef: null,
          mode: "base",
          cwd: "/c",
          mimeType: "application/paseo-review",
          type: "review",
        },
        {
          type: "forge_change_request",
          mimeType: "application/paseo-forge-change-request",
          number: 7,
          title: "t",
          url: "u",
          headRefName: null,
          projectPath: "/p",
          forge: "gitlab",
        },
        {
          type: "text",
          mimeType: "text/plain",
          text: "t",
          externalResource: { url: "u", title: "t", identifier: "i", id: "1", resourceType: "r", providerLabel: "L", provider: "p" },
        },
      ],
    }),
  },
  {
    id: "session.agent_create.full",
    direction: "inbound",
    source: "AgentCreateRequestSchema extend order",
    raw: `{"type":"session","message":{"subscribe":false,"agentId":"${UUID}","requestId":"r","labels":{"z":"1","3":"2"},"autoArchive":true,"worktree":{"mode":"branch-off","newBranch":"n","base":"main"},"git":{"githubPrNumber":2,"checkoutSource":{"number":1,"kind":"change_request"},"action":"checkout","refName":"r","baseBranch":"b"},"attachments":[],"images":[],"outputSchema":{"type":"object","1":2.50},"clientMessageId":"c","initialPrompt":"p","worktreeName":"w","callerAgentId":"ca","workspaceId":"any","env":{"B":"1","A":"2"},"config":{"mcpServers":{"s":{"args":["a"],"command":"c","type":"stdio"},"h":{"headers":{"k":"v"},"url":"u","type":"http"}},"systemPrompt":"s","toolPolicy":{"preapproved":[{"tool":" t ","server":" s ","kind":"mcp"}]},"providerOptions":{"n":{"deep":[1.0,null,true,{"2":0,"1":0}]}},"title":"  Title  ","featureValues":{"f":false},"thinkingOptionId":"t","model":"m","modeId":"x","cwd":"/c","provider":"codex","extra":1},"idempotencyKey":"k","type":"agent.create.request"}}`,
  },
  {
    id: "session.agent_create.title_null",
    direction: "inbound",
    source: "AgentSessionConfigSchema title optional nullable",
    input: session({ type: "agent.create.request", requestId: "r", config: { provider: "codex", cwd: "/c", title: null } }),
  },
  {
    id: "session.agent_create.reject_blank_title",
    direction: "inbound",
    source: "AgentSessionConfigSchema title trim min(1)",
    input: session({ type: "agent.create.request", requestId: "r", config: { provider: "codex", cwd: "/c", title: "   " } }),
  },
  {
    id: "session.agent_create.reject_long_title",
    direction: "inbound",
    source: "AgentSessionConfigSchema title max(200)",
    input: session({ type: "agent.create.request", requestId: "r", config: { provider: "codex", cwd: "/c", title: "x".repeat(201) } }),
  },
  {
    id: "session.agent_create.title_200_after_trim",
    direction: "inbound",
    source: "AgentSessionConfigSchema title trims before max(200)",
    input: session({ type: "agent.create.request", requestId: "r", config: { provider: "codex", cwd: "/c", title: ` ${"y".repeat(200)} ` } }),
  },
  {
    id: "session.agent_create.reject_bad_uuid",
    direction: "inbound",
    source: "AgentCreateRequestSchema agentId z.uuid()",
    input: session({ type: "agent.create.request", requestId: "r", agentId: "123e4567-e89b-02d3-a456-426614174000", config: { provider: "codex", cwd: "/c" } }),
  },
  {
    id: "session.agent_create.reject_invalid_strict_attachment",
    direction: "inbound",
    source: "AgentCreateRequestSchema attachments strict array",
    input: session({ type: "agent.create.request", requestId: "r", config: { provider: "codex", cwd: "/c" }, attachments: ["junk"] }),
  },
  {
    id: "session.agent_create.reject_tool_policy_extra_key",
    direction: "inbound",
    source: "ToolPolicySchema strict",
    input: session({ type: "agent.create.request", requestId: "r", config: { provider: "codex", cwd: "/c", toolPolicy: { preapproved: [], extra: 1 } } }),
  },
  {
    id: "session.agent_create.reject_long_idempotency_key",
    direction: "inbound",
    source: "idempotencyKey max(512)",
    input: session({ type: "agent.create.request", requestId: "r", idempotencyKey: "k".repeat(513), config: { provider: "codex", cwd: "/c" } }),
  },
  {
    id: "session.agent_create.reject_non_string_env",
    direction: "inbound",
    source: "env record(string, string)",
    input: session({ type: "agent.create.request", requestId: "r", env: { A: 1 }, config: { provider: "codex", cwd: "/c" } }),
  },
  {
    id: "session.agent_create.reject_null_labels",
    direction: "inbound",
    source: "labels default({}) applies to undefined only",
    input: session({ type: "agent.create.request", requestId: "r", labels: null, config: { provider: "codex", cwd: "/c" } }),
  },
  {
    id: "session.create_agent.legacy_lenient",
    direction: "inbound",
    source: "CreateAgentRequestMessageSchema",
    input: session({
      type: "create_agent_request",
      requestId: "r",
      config: { provider: "codex", cwd: "/c" },
      attachments: ["junk"],
      worktree: { mode: "checkout-pr", prNumber: 4 },
      agentId: "dropped",
      subscribe: true,
    }),
  },
  {
    id: "session.workspace_create.with_agent",
    direction: "inbound",
    source: "WorkspaceCreateRequestSchema, WorkspaceInitialAgentSchema",
    input: session({
      type: "workspace.create.request",
      requestId: "r",
      workspaceId: WKS,
      title: "T",
      firstAgentContext: { attachments: 5, prompt: "p" },
      agent: {
        agentId: UUID,
        labels: { k: "v" },
        config: { provider: "codex", cwd: "/c" },
        initialPrompt: "go",
        git: { baseBranch: "dropped" },
        requestId: "dropped",
      },
      source: { projectId: "p", path: "/c", kind: "directory" },
    }),
  },
  {
    id: "session.workspace_create.worktree_source",
    direction: "inbound",
    source: "WorkspaceCreateRequestSchema worktree source",
    input: session({
      type: "workspace.create.request",
      requestId: "r",
      source: { kind: "worktree", worktreeSlug: "s", githubPrNumber: 3, branchName: "b", action: "branch-off", cwd: "/c" },
    }),
  },
  {
    id: "session.workspace_create.reject_uppercase_workspace_id",
    direction: "inbound",
    source: "workspaceId regex ^wks_[a-f0-9]{16}$",
    input: session({ type: "workspace.create.request", requestId: "r", workspaceId: "wks_0123456789ABCDEF", source: { kind: "directory", path: "/c" } }),
  },
  {
    id: "session.workspace_create.reject_missing_source",
    direction: "inbound",
    source: "WorkspaceCreateRequestSchema source required",
    input: session({ type: "workspace.create.request", requestId: "r" }),
  },
  {
    id: "session.creation_subscribe",
    direction: "inbound",
    source: "CreationSubscribeRequestSchema",
    input: session({ type: "creation.subscribe.request", subscribe: true, idempotencyKey: "k", kind: "agent", requestId: "r" }),
  },
  {
    id: "session.creation_subscribe.reject_empty_key",
    direction: "inbound",
    source: "CreationSubscribeRequestSchema idempotencyKey min(1)",
    input: session({ type: "creation.subscribe.request", requestId: "r", kind: "workspace", idempotencyKey: "" }),
  },
  {
    id: "session.timeline_set_subscription",
    direction: "inbound",
    source: "SetAgentTimelineSubscriptionRequestMessageSchema",
    input: session({ type: "agent.timeline.set_subscription.request", requestId: "r", agentIds: ["b", "a"] }),
  },
  {
    id: "session.events_set_subscription",
    direction: "inbound",
    source: "SessionEventsSetSubscriptionRequestSchema",
    input: session({ type: "session.events.set_subscription.request", notifications: false, events: ["status.server_info", "activity_log"], requestId: "r" }),
  },
  {
    id: "session.events_set_subscription.reject_unknown_event",
    direction: "inbound",
    source: "SessionEventSubscriptionSchema enum",
    input: session({ type: "session.events.set_subscription.request", requestId: "r", events: ["agent_update"] }),
  },
  {
    id: "session.subscription_release",
    direction: "inbound",
    source: "SubscriptionReleaseRequestSchema",
    input: session({ type: "subscription.release.request", subscriptionId: "s", requestId: "r" }),
  },
);
