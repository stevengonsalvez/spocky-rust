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

// Review 1 edge cases: JavaScript number text, property order, and
// JSON.parse duplicate keys.
const HARD_FLOATS = [
  "4.658607306269797003e-22",
  "1.840086450705316562e-174",
  "8.3112634102003129e-134",
  "7.41273583631350841e262",
  "1.3936445979811658496e245",
  "7.5593921016370845944e95",
];

CASES.push(
  {
    id: "ws.hello.extras_number_text",
    direction: "inbound",
    source: "WSHelloMessageSchema capabilities passthrough keeps extra values; JSON.stringify number text",
    raw: `{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1,"capabilities":{"x":1e21,"neg":-0,"one":1.0,"tiny":1e-7,"big":123456789012345678901234567890,${HARD_FLOATS.map((f, i) => `"f${i}":${f}`).join(",")}}}`,
  },
  {
    id: "ws.hello.extras_integer_like_keys",
    direction: "inbound",
    source: "JS engines enumerate array-index keys first, ascending",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1,"capabilities":{"z":1,"10":0,"voice":true,"5":0,"01":2,"4294967295":3,"4294967294":4,"hello_rejection":false,"a":{"2":1,"1":2}}}',
  },
  {
    id: "ws.hello.browser_host_extras_integer_like_keys",
    direction: "inbound",
    source: "BrowserAutomationHostCapabilitySchema passthrough key order",
    raw: '{"type":"hello","clientId":"c","clientType":"browser","protocolVersion":1,"capabilities":{"browser_host":{"z":1,"7":-0,"supportedCommands":["click"]}}}',
  },
  {
    id: "ws.hello.duplicate_keys_last_value_first_position",
    direction: "inbound",
    source: "JSON.parse keeps the first position and the last value",
    raw: '{"type":"hello","clientId":"a","clientType":"cli","protocolVersion":1,"capabilities":{"q":1,"voice":false,"q":2,"voice":true},"clientId":"b"}',
  },
  {
    id: "ws.duplicate_type_last_wins",
    direction: "inbound",
    source: "JSON.parse duplicate discriminator",
    raw: '{"type":"hello","type":"ping"}',
  },
  {
    id: "ws.hello.duplicate_key_rejected_value_then_valid",
    direction: "inbound",
    source: "JSON.parse discards the first value before zod sees it",
    raw: '{"type":"hello","clientId":"","clientType":"cli","protocolVersion":1,"clientId":"ok"}',
  },
  {
    id: "ws.hello.protocol_version_1_0",
    direction: "inbound",
    source: "z.number().int() on 1.0",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1.0}',
  },
  {
    id: "ws.hello.protocol_version_negative_zero",
    direction: "inbound",
    source: "z.number().int() on -0; JSON.stringify(-0) is 0",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":-0}',
  },
  {
    id: "ws.hello.protocol_version_max_safe",
    direction: "inbound",
    source: "z.number().int() accepts Number.MAX_SAFE_INTEGER",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":9007199254740991}',
  },
  {
    id: "ws.hello.reject_protocol_version_2_pow_53",
    direction: "inbound",
    source: "z.number().int() rejects 2^53",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":9007199254740992}',
  },
  {
    id: "ws.hello.reject_protocol_version_exponent_overflow",
    direction: "inbound",
    source: "z.number().int() rejects 1e16",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1e16}',
  },
  {
    id: "session.agent_create.provider_options_hard_floats",
    direction: "inbound",
    source: "z.json() provider options keep JSON.parse doubles",
    raw: `{"type":"session","message":{"type":"agent.create.request","requestId":"r","config":{"provider":"codex","cwd":"/c","providerOptions":{"floats":[${HARD_FLOATS.join(",")},-0,5e-324,1.7976931348623157e308]}}}}`,
  },
  {
    id: "session.fetch_agents.duplicate_nested_keys",
    direction: "inbound",
    source: "JSON.parse duplicate keys inside a record and an object",
    raw: '{"type":"session","message":{"type":"fetch_agents_request","requestId":"r","filter":{"labels":{"b":"1","a":"2","b":"3"},"includeArchived":false,"includeArchived":true}}}',
  },
);

// JSON.parse semantics carried by js_value: lone surrogates, overflowing
// numbers, nesting deeper than serde_json's 128 levels, and syntax errors.
function nested(depth) {
  return `${"[".repeat(depth)}1${"]".repeat(depth)}`;
}

CASES.push(
  {
    id: "ws.hello.lone_surrogate_strings",
    direction: "inbound",
    source: "JSON.parse keeps lone surrogates; JSON.stringify writes them as \\u escapes",
    raw: '{"type":"hello","clientId":"c\\ud800","clientType":"cli","protocolVersion":1,"appVersion":"\\udfff\\ud83d\\ude00","capabilities":{"k\\udc00":"\\ud801"}}',
  },
  {
    id: "ws.hello.reject_overflowing_protocol_version",
    direction: "inbound",
    source: "JSON.parse turns 1e400 into Infinity; z.number().int() rejects it",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1e400}',
  },
  {
    id: "ws.hello.extras_overflowing_numbers",
    direction: "inbound",
    source: "passthrough keeps Infinity; JSON.stringify writes null",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1,"capabilities":{"big":1e400,"small":-1e400,"under":1e-400}}',
  },
  {
    id: "session.agent_create.deep_provider_options",
    direction: "inbound",
    source: "z.json() accepts nesting deeper than 128 levels",
    raw: `{"type":"session","message":{"type":"agent.create.request","requestId":"r","config":{"provider":"codex","cwd":"/c","providerOptions":{"deep":${nested(300)}}}}}`,
  },
  {
    id: "session.agent_create.reject_infinity_in_json_options",
    direction: "inbound",
    source: "z.json() rejects a non-finite number",
    raw: '{"type":"session","message":{"type":"agent.create.request","requestId":"r","config":{"provider":"codex","cwd":"/c","providerOptions":{"n":1e400}}}}',
  },
  {
    id: "ws.reject_trailing_comma",
    direction: "inbound",
    source: "JSON.parse SyntaxError",
    raw: '{"type":"ping",}',
  },
  {
    id: "ws.reject_lone_surrogate_escape_truncated",
    direction: "inbound",
    source: "JSON.parse SyntaxError on a short \\u escape",
    raw: '{"type":"hello","clientId":"\\ud8","clientType":"cli","protocolVersion":1}',
  },
);

// G1 outbound session frames in pinned daemon construction order. The Rust
// test builds each one from typed values and must write identical bytes.
const T0 = "2026-10-01T00:00:00.000Z";
const T1 = "2026-10-01T00:00:01.000Z";
const AGENT_ID = "123e4567-e89b-42d3-a456-426614174000";
const THREAD = "019a0000-0000-7000-8000-000000000001";
const CODEX_CAPABILITIES = {
  supportsStreaming: true,
  supportsSessionPersistence: true,
  supportsSessionListing: true,
  supportsDynamicModes: false,
  supportsMcpServers: true,
  supportsReasoningStream: true,
  supportsToolInvocations: true,
  supportsRewindConversation: true,
  supportsRewindFiles: false,
  supportsRewindBoth: false,
};
const CODEX_MODES = [
  {
    id: "auto",
    label: "Default Permissions",
    description: "Edit files and run commands with Codex's default approval flow.",
  },
  {
    id: "full-access",
    label: "Full Access",
    description: "Edit files, run commands, and access the network without additional prompts.",
  },
];
const PLAN_FEATURE = {
  type: "toggle",
  id: "plan_mode",
  label: "Plan",
  description: "Switch Codex into planning-only collaboration mode",
  tooltip: "Toggle plan mode",
  icon: "list-todo",
  value: false,
};

// Live snapshot from pinned toAgentPayload (agent-projections.ts), then the
// two assignments of enrichAgentPayload (session.ts:2033-2037) for an agent
// with no stored title or archive time.
function liveAgent(pinned, { idle = false } = {}) {
  const payload = pinned.toAgentPayload({
    id: AGENT_ID,
    provider: "codex",
    cwd: "/tmp/project",
    workspaceId: WKS,
    config: { provider: "codex", cwd: "/tmp/project", modeId: "full-access" },
    runtimeInfo: {
      provider: "codex",
      sessionId: THREAD,
      model: null,
      thinkingOptionId: null,
      modeId: "full-access",
      extra: undefined,
    },
    createdAt: new Date(T0),
    updatedAt: new Date(T1),
    lastUserMessageAt: new Date(T0),
    lifecycle: idle ? "idle" : "running",
    activeTurnId: idle ? null : "turn-1",
    activeTurnStartedAt: idle ? null : new Date(T0),
    capabilities: CODEX_CAPABILITIES,
    currentModeId: "full-access",
    availableModes: CODEX_MODES,
    features: [PLAN_FEATURE],
    pendingPermissions: new Map(),
    // codex-app-server-agent.ts describePersistence
    persistence: {
      provider: "codex",
      sessionId: THREAD,
      nativeHandle: THREAD,
      metadata: {
        provider: "codex",
        cwd: "/tmp/project",
        title: null,
        threadId: THREAD,
        modeId: "full-access",
        model: null,
        thinkingOptionId: null,
        providerOptions: undefined,
        toolPolicy: undefined,
        systemPrompt: undefined,
        mcpServers: undefined,
        asyncQuestions: [],
      },
    },
    labels: {},
    lastUsage: idle
      ? { inputTokens: 1200, cachedInputTokens: 0, outputTokens: 34, totalCostUsd: 0.0123 }
      : undefined,
    lastError: undefined,
    attention: idle
      ? { requiresAttention: true, attentionReason: "finished", attentionTimestamp: new Date(T1) }
      : { requiresAttention: false },
  });
  payload.title = null;
  payload.archivedAt = null;
  return payload;
}

function idleAgent(pinned) {
  return liveAgent(pinned, { idle: true });
}

// Stored snapshot from pinned buildStoredAgentPayload.
function storedAgent(pinned) {
  return pinned.buildStoredAgentPayload(
    {
      id: AGENT_ID,
      provider: "codex",
      cwd: "/tmp/project",
      workspaceId: WKS,
      createdAt: T0,
      updatedAt: T1,
      lastActivityAt: T1,
      lastUserMessageAt: T0,
      title: null,
      labels: {},
      lastStatus: "closed",
      lastModeId: "full-access",
      config: null,
      persistence: { provider: "codex", sessionId: THREAD, nativeHandle: THREAD },
      requiresAttention: false,
      attentionReason: null,
      attentionTimestamp: null,
    },
    ["codex"],
  );
}

// Placement from buildProjectPlacementForWorkspace (session.ts:2065-2088) with
// the checkout from pinned checkoutFromPersistedWorkspacePlacement.
function placement(pinned) {
  return {
    projectKey: "proj_golden",
    projectName: "project",
    workspaceName: "main",
    checkout: pinned.checkoutFromPersistedWorkspacePlacement({
      workspace: {
        kind: "local_checkout",
        cwd: "/tmp/project",
        branch: "main",
        worktreeRoot: "/tmp/project",
        isPaseoOwnedWorktree: false,
        mainRepoRoot: null,
      },
    }),
  };
}

function workspace(pinned) {
  return {
    id: WKS,
    projectId: "proj_golden",
    projectDisplayName: "project",
    projectCustomName: null,
    projectCustomIconRevision: null,
    projectRootPath: "/tmp/project",
    workspaceDirectory: "/tmp/project",
    projectKind: "git",
    workspaceKind: "local_checkout",
    name: "main",
    title: null,
    pinnedAt: null,
    archivingAt: null,
    status: "done",
    statusEnteredAt: null,
    activityAt: null,
    diffStat: null,
    scripts: [],
    project: placement(pinned),
  };
}

function creation(kind, revision, phase, extra = {}) {
  return {
    kind,
    idempotencyKey: "8b0c3c8e-1a2b-4c3d-8e9f-000000000002",
    revision,
    phase,
    error: null,
    workspaceId: kind === "workspace" ? WKS : WKS,
    agentId: kind === "workspace" ? null : AGENT_ID,
    ...extra,
  };
}

const SESSION_SOURCE = "packages/server/src/server/session.ts";

CASES.push(
  {
    id: "out.workspace_create.update_accepted",
    direction: "outbound",
    source: "creation/index.ts initialRecord; session.ts creationUpdate",
    build: (pinned) => session({ type: "workspace.create.update", payload: creation("workspace", 0, "accepted") }),
  },
  {
    id: "out.workspace_create.update_ready",
    direction: "outbound",
    source: "creation/index.ts publish workspace_ready; session.ts describeWorkspaceRecord",
    build: (pinned) => session({
      type: "workspace.create.update",
      payload: creation("workspace", 1, "workspace_ready", { workspace: workspace(pinned) }),
    }),
  },
  {
    id: "out.workspace_create.response",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleWorkspaceCreation success`,
    build: (pinned) => session({
      type: "workspace.create.response",
      payload: {
        requestId: "r",
        workspace: workspace(pinned),
        creation: creation("workspace", 2, "completed", { workspace: workspace(pinned) }),
        error: null,
        setupTerminalId: null,
      },
    }),
  },
  {
    id: "out.workspace_create.response_error",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleWorkspaceCreation catch`,
    build: (pinned) => session({
      type: "workspace.create.response",
      payload: {
        requestId: "r",
        workspace: null,
        error: "Directory not found",
        errorCode: "directory_not_found",
        setupTerminalId: null,
      },
    }),
  },
  {
    id: "out.agent_create.update_ready",
    direction: "outbound",
    source: "creation/index.ts publish agent_ready; agent-projections.ts toAgentPayload",
    build: (pinned) => session({
      type: "agent.create.update",
      payload: creation("agent", 1, "agent_ready", { agent: liveAgent(pinned) }),
    }),
  },
  {
    id: "out.agent_create.update_failed",
    direction: "outbound",
    source: "creation/index.ts publish failed",
    build: (pinned) => session({
      type: "agent.create.update",
      payload: {
        ...creation("agent", 1, "failed"),
        error: "Codex exited",
        failedStage: "agent",
        outcomeUnknown: false,
      },
    }),
  },
  {
    id: "out.agent_create.response",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleAgentCreation`,
    build: (pinned) => session({
      type: "agent.create.response",
      payload: {
        requestId: "r",
        agent: liveAgent(pinned),
        error: null,
        creation: creation("agent", 3, "completed", { agent: liveAgent(pinned) }),
      },
    }),
  },
  {
    id: "out.fetch_agent.response",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleFetchAgent`,
    build: (pinned) => session({
      type: "fetch_agent_response",
      payload: { requestId: "r", agent: idleAgent(pinned), project: placement(pinned), error: null },
    }),
  },
  {
    id: "out.fetch_agent.not_found",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleFetchAgent not found`,
    build: (pinned) => session({
      type: "fetch_agent_response",
      payload: { requestId: "r", agent: null, project: null, error: "Agent not found: x" },
    }),
  },
  {
    id: "out.fetch_agent.stored",
    direction: "outbound",
    source: "agent-projections.ts buildStoredAgentPayload",
    build: (pinned) => session({
      type: "fetch_agent_response",
      payload: {
        requestId: "r",
        agent: storedAgent(pinned),
        project: placement(pinned),
        error: null,
      },
    }),
  },
  {
    id: "out.fetch_agents.response",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleFetchAgents, listFetchAgentsEntries`,
    build: (pinned) => session({
      type: "fetch_agents_response",
      payload: {
        requestId: "r",
        entries: [{ agent: idleAgent(pinned), project: placement(pinned) }],
        pageInfo: { nextCursor: null, prevCursor: null, hasMore: false },
      },
    }),
  },
  {
    id: "out.fetch_workspaces.response_git_data",
    direction: "outbound",
    source: `${SESSION_SOURCE} describeWorkspaceRecordWithGitData; workspace-directory.ts`,
    build: (pinned) => session({
      type: "fetch_workspaces_response",
      payload: {
        requestId: "r",
        subscriptionId: "sub-1",
        entries: [
          {
            ...workspace(pinned),
            diffStat: { additions: 3, deletions: 1 },
            gitRuntime: {
              currentBranch: "main",
              remoteUrl: null,
              isPaseoOwnedWorktree: false,
              isDirty: true,
              aheadBehind: null,
              aheadOfOrigin: null,
              behindOfOrigin: null,
            },
            githubRuntime: { featuresEnabled: false, pullRequest: null, error: null },
            forge: "github",
          },
        ],
        emptyProjects: [],
        pageInfo: { nextCursor: null, prevCursor: null, hasMore: false },
      },
    }),
  },
  {
    id: "out.fetch_agent_timeline.response",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleFetchAgentTimelineRequest; agent-manager.ts recordSubmittedPrompt; timeline-projection.ts; codex/tool-call-mapper.ts`,
    build: (pinned) => session({
      type: "fetch_agent_timeline_response",
      payload: {
        requestId: "r",
        agentId: AGENT_ID,
        agent: idleAgent(pinned),
        direction: "tail",
        projection: "projected",
        epoch: "ep-1",
        reset: false,
        staleCursor: false,
        gap: false,
        window: { minSeq: 1, maxSeq: 6, nextSeq: 7 },
        startCursor: { epoch: "ep-1", seq: 1 },
        endCursor: { epoch: "ep-1", seq: 6 },
        hasOlder: false,
        hasNewer: false,
        entries: [
          {
            provider: "codex",
            item: { type: "user_message", text: "Say hello", clientMessageId: "cm-1", messageId: "cm-1" },
            timestamp: T0,
            seqStart: 1,
            seqEnd: 1,
            sourceSeqRanges: [{ startSeq: 1, endSeq: 1 }],
            turnId: "turn-1",
            collapsed: [],
          },
          {
            provider: "codex",
            item: { type: "reasoning", text: "Thinking" },
            timestamp: T0,
            seqStart: 2,
            seqEnd: 2,
            sourceSeqRanges: [{ startSeq: 2, endSeq: 2 }],
            turnId: "turn-1",
            collapsed: [],
          },
          {
            provider: "codex",
            item: {
              type: "tool_call",
              callId: "call-1",
              name: "shell",
              status: "completed",
              error: null,
              detail: { type: "shell", command: "ls", cwd: "/tmp/project", output: "a\n", exitCode: 0 },
            },
            timestamp: T0,
            seqStart: 3,
            seqEnd: 4,
            sourceSeqRanges: [{ startSeq: 3, endSeq: 4 }],
            turnId: "turn-1",
            collapsed: ["tool_lifecycle"],
          },
          {
            provider: "codex",
            item: { type: "assistant_message", text: "Hello!", messageId: "msg-1" },
            timestamp: T1,
            seqStart: 5,
            seqEnd: 6,
            sourceSeqRanges: [{ startSeq: 5, endSeq: 6 }],
            turnId: "turn-1",
            collapsed: ["assistant_merge"],
          },
        ],
        error: null,
      },
    }),
  },
  {
    id: "out.fetch_agent_timeline.error",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleFetchAgentTimelineRequest catch`,
    build: (pinned) => session({
      type: "fetch_agent_timeline_response",
      payload: {
        requestId: "r",
        agentId: "missing",
        agent: null,
        direction: "tail",
        projection: "projected",
        epoch: "",
        reset: false,
        staleCursor: false,
        gap: false,
        window: { minSeq: 0, maxSeq: 0, nextSeq: 0 },
        startCursor: null,
        endCursor: null,
        hasOlder: false,
        hasNewer: false,
        mergeWindow: true,
        entries: [],
        error: "Agent not found: missing",
      },
    }),
  },
  {
    id: "out.wait_for_finish.idle",
    direction: "outbound",
    source: `${SESSION_SOURCE} handleWaitForFinish`,
    build: (pinned) => session({
      type: "wait_for_finish_response",
      payload: { requestId: "r", status: "idle", final: idleAgent(pinned), error: null, lastMessage: "Hello!" },
    }),
  },
  {
    id: "out.session_pong",
    direction: "outbound",
    source: `${SESSION_SOURCE}:2586-2597`,
    build: (pinned) => session({
      type: "pong",
      payload: { requestId: "r", clientSentAt: 1, serverReceivedAt: 1790000000000, serverSentAt: 1790000000000 },
    }),
  },
  {
    id: "out.rpc_error",
    direction: "outbound",
    source: "owned-subscriptions/index.ts:221-224",
    build: (pinned) => session({
      type: "rpc_error",
      payload: { requestId: "r", requestType: "fetch_agent_request", error: "Invalid message", code: "invalid_message" },
    }),
  },
  {
    id: "out.status_error",
    direction: "outbound",
    source: "owned-subscriptions/index.ts:221-224 without requestId",
    build: (pinned) => session({ type: "status", payload: { status: "error", message: "Invalid message" } }),
  },
  {
    id: "out.subscription_responses",
    direction: "outbound",
    source: `${SESSION_SOURCE}:2273-2281`,
    build: (pinned) => session({
      type: "agent.timeline.set_subscription.response",
      payload: { agentIds: ["a", "b"], requestId: "r", subscriptionId: "sub-1" },
    }),
  },
  {
    id: "out.send_agent_message.response",
    direction: "outbound",
    source: `${SESSION_SOURCE}:8119-8127`,
    build: (pinned) => session({
      type: "send_agent_message_response",
      payload: { requestId: "r", agentId: AGENT_ID, accepted: true, error: null },
    }),
  },
);

// Review 2: String length is UTF-16 code units, so a lone surrogate counts
// once; zod 4 drops own __proto__ keys from z.record, passthrough extras, and
// z.json() at any depth without validating them, but z.unknown() keeps them.
function agentCreate(config, extra = "") {
  return `{"type":"session","message":{"type":"agent.create.request","requestId":"r"${extra},"config":{"provider":"codex","cwd":"/c"${config}}}}`;
}

CASES.push(
  {
    id: "session.agent_create.title_200_with_lone_surrogate",
    direction: "inbound",
    source: "title max(200) counts a lone surrogate as one code unit",
    raw: agentCreate(`,"title":"${"y".repeat(199)}\\ud800"`),
  },
  {
    id: "session.agent_create.reject_title_201_with_lone_surrogate",
    direction: "inbound",
    source: "title max(200)",
    raw: agentCreate(`,"title":"${"y".repeat(200)}\\ud800"`),
  },
  {
    id: "session.agent_create.reject_title_199_plus_astral",
    direction: "inbound",
    source: "title max(200) counts U+10FFFF as two code units",
    raw: agentCreate(`,"title":"${"y".repeat(199)}\\udbff\\udfff"`),
  },
  {
    id: "session.agent_create.idempotency_key_512_with_lone_surrogate",
    direction: "inbound",
    source: "idempotencyKey max(512)",
    raw: agentCreate("", `,"idempotencyKey":"${"k".repeat(511)}\\udfff"`),
  },
  {
    id: "session.creation_subscribe.lone_surrogate_key",
    direction: "inbound",
    source: "idempotencyKey min(1) with a lone surrogate",
    raw: '{"type":"session","message":{"type":"creation.subscribe.request","requestId":"r","kind":"agent","idempotencyKey":"\\ud800"}}',
  },
  {
    id: "session.agent_create.proto_keys",
    direction: "inbound",
    source: "z.record and z.json drop __proto__ unvalidated; z.unknown keeps it",
    raw: agentCreate(
      ',"featureValues":{"__proto__":{"x":1},"f":{"__proto__":7,"k":1}},"providerOptions":{"__proto__":"x","p":{"a":[{"__proto__":{"n":1e400},"b":2}],"__proto__":1}}',
      ',"labels":{"__proto__":5,"z":"1"},"env":{"__proto__":{},"A":"1"}',
    ),
  },
  {
    id: "ws.hello.proto_extras",
    direction: "inbound",
    source: "passthrough extras drop __proto__; z.unknown extra values keep nested __proto__",
    raw: '{"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1,"capabilities":{"__proto__":{"voice":true},"x":{"__proto__":1},"voice":false}}',
  },
);
