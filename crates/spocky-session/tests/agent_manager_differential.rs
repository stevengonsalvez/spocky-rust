//! Differential check of the agent manager against the pinned build's
//! `AgentManager`: the same scripted fake providers drive both, and the
//! outcomes, subscriber feeds (agent payloads and stream events), provider
//! calls, timelines, run and wait results, and stored records must match.
//!
//! Scenarios:
//! - `main`: `createAgent`, a completed `runAgent`, and `waitForAgentEvent`.
//! - `errors`: the `createAgent` rejections (working directory, unknown,
//!   unavailable and disabled providers, provider options, tool policy, MCP
//!   support, agent ids) and the unknown-agent errors.
//! - `turns`: a failed turn, a canceled turn, coalescing edges, an idle
//!   `waitForActive`, and aborted waits on a held turn.
//! - `permission`: a wait that finishes on `permission_requested`, then a
//!   wait that returns the pending permission at once.
//!
//! - `lifecycle`: a permission answered by `respondToPermission`, a turn
//!   canceled by `cancelAgentRun` (twice), a cancel before the turn has
//!   started, and `closeAgent` (twice).
//!
//! - `subagents`: provider sub-agent events during a turn, the four
//!   sub-agent queries, and `closeAgent` canceling a running child.
//!
//! - `hydration`: `hydrateTimelineFromProvider` from the provider's
//!   history: already primed (a no-op), forced with and without broadcast,
//!   and an unknown agent.
//!
//! - `resume`: `resumeAgentFromPersistence` priming from history, an
//!   archived agent resumed for history without its working directory,
//!   and the id, client and availability errors.
//!
//! - `titles`: `setTitle` (trimmed, blank, unknown agent) and
//!   `hasInFlightRun` idle, running and unknown.
//!
//! - `runstart`: `waitForAgentRunStart` on an unknown agent, with no
//!   pending run, through a slow start, a failed start, an abort mid-wait
//!   and before the wait, and after the run finished.
//!
//! - `outofband`: `tryRunOutOfBand` declined, a `/goal` command with a
//!   client message id whose handler emits a timeline item and a usage
//!   event, a failing handler, and an unknown agent.
//!
//! - `loading`: `ensureAgentLoaded` on a second manager over the first's
//!   storage: two concurrent loads sharing one resume (one asking for the
//!   timeline broadcast), a record without a persistence handle taking the
//!   create path, an unavailable provider, and a missing record.
//!
//! - `replace`: `replaceAgentRun` on an idle agent, on a running one (its run
//!   is interrupted, then the prompt streams), on one whose run finishes by
//!   itself while the replacement waits, with a run still starting, and with
//!   a cancellation that is never acknowledged (the replacement fails, and
//!   its mark is cleared when the held run later ends).
//!
//! - `archive`: `archiveAgent` on a parent whose children are archived
//!   with it (live and stored-only), detached (another workspace, an open
//!   tab) or left alone; `unarchiveSnapshot` with a workspace and label
//!   patch, twice and for an unknown agent; `detachAgent`; and, on a fresh
//!   home, `archiveSnapshot` of a closed agent then `unarchiveSnapshotByHandle`.
//!
//! - `shutdown`: `flush` does not wait for an in-flight `createAgent`
//!   (its `createSession` held 150 ms), `flushForShutdown` does, and a
//!   registration after `prepareForShutdown` is refused.
//!
//! A scripted `{"type":"__delay","ms":N}` entry pauses the fake's emission
//! and is never emitted; a leading `{"type":"__startDelay","ms":N}` holds
//! `startTurn` that long before it resolves, and a leading
//! `{"type":"__startFail","ms":N,"message":M}` makes it reject with `M`
//! after `N` ms.
//!
//! Normalized: wall-clock ISO timestamps (`<ISO>`) and random UUIDs such as
//! timeline epochs (`<UUID>`), nothing else. The fixed agent ids stay as
//! they are.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly). `wait_releases_its_subscription` runs
//! without them.

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use spocky_session::agent_loading::{EnsureAgentLoadedDeps, ensure_agent_loaded};
use spocky_session::agent_manager::{
    AgentManager, AgentManagerEvent, AgentManagerOptions, CreateAgentOptions, HydrateBroadcast,
    HydrateTimelineOptions, ProviderDefinition, ResumeAgentOptions, SubscribeOptions,
    TurnEventStream, UnarchiveUpdates, WaitForAgentOptions,
};
use spocky_session::agent_projection::{AgentAttention, to_agent_payload};
use spocky_session::agent_sdk::{
    AbortController, AbortReason, AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError,
    AgentEventStream, AgentLaunchContext, AgentPromptInput, AgentResult, AgentResumePurpose,
    AgentResumeSessionOptions, AgentRunOptions, AgentSession, AgentStreamEvent, BoxFuture,
    FetchCatalogOptions, ImportedTimelineEntry, OutOfBandHandler, ProviderRefreshContext,
    StreamCallback, Unsubscribe,
};
use spocky_session::agent_storage::AgentStorage;
use spocky_session::timeline::FetchDirection;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const AGENT_ID: &str = "00000000-0000-4000-8000-0000000000a1";
const OTHER_ID: &str = "00000000-0000-4000-8000-0000000000b2";
const UNKNOWN_ID: &str = "00000000-0000-4000-8000-0000000000ff";

/// The ids the scenarios choose; [`normalize`] keeps them.
const CHILD_SAME_ID: &str = "00000000-0000-4000-8000-0000000000e1";
const CHILD_OTHER_WORKSPACE_ID: &str = "00000000-0000-4000-8000-0000000000e2";
const CHILD_TAB_ID: &str = "00000000-0000-4000-8000-0000000000e3";
const CHILD_STORED_ID: &str = "00000000-0000-4000-8000-0000000000e4";
const FIXED_IDS: [&str; 7] = [
    AGENT_ID,
    OTHER_ID,
    UNKNOWN_ID,
    CHILD_SAME_ID,
    CHILD_OTHER_WORKSPACE_ID,
    CHILD_TAB_ID,
    CHILD_STORED_ID,
];

/// What the fake session emits after `startTurn` resolves when its script
/// has no turn left.
const TURN_EVENTS: &str = r#"[
  {"type":"thread_started","provider":"fake","sessionId":"sess-1"},
  {"type":"turn_started","provider":"fake","turnId":"turn-1"},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"user_message","text":"hello","clientMessageId":"client-1","messageId":"provider-msg-1"}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"reasoning","text":"thinking "}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"reasoning","text":"more"}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"tool_call","callId":"call-1","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"ls"}}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"tool_call","callId":"call-1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"ls","output":"a\nb","exitCode":0}}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"assistant_message","text":"Hel"}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"assistant_message","text":"lo!"}},
  {"type":"usage_updated","provider":"fake","turnId":"turn-1","usage":{"inputTokens":3,"outputTokens":5}},
  {"type":"turn_completed","provider":"fake","turnId":"turn-1","usage":{"inputTokens":3,"outputTokens":5,"totalCostUsd":0.25}}
]"#;

/// The `turns` scenario's scripted turns, in `startTurn` order.
const SCENARIO_TURNS: &str = r#"{
  "failed": [
    {"type":"turn_started","provider":"fake","turnId":"turn-2"},
    {"type":"timeline","provider":"fake","turnId":"turn-2","item":{"type":"assistant_message","text":"partial"}},
    {"type":"turn_failed","provider":"fake","turnId":"turn-2","error":" boom ","code":"E_FAKE","diagnostic":"stack trace"}
  ],
  "canceled": [
    {"type":"turn_started","provider":"fake","turnId":"turn-3"},
    {"type":"timeline","provider":"fake","turnId":"turn-3","item":{"type":"assistant_message","text":"half"}},
    {"type":"turn_canceled","provider":"fake","turnId":"turn-3","reason":"user stopped"}
  ],
  "coalesce": [
    {"type":"turn_started","provider":"fake","turnId":"turn-4"},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"A"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":""}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"reasoning","text":"R1"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"reasoning","text":"R2"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"B"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"tool_call","callId":"call-2","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"pwd"}}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"tool_call","callId":"call-3","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"id"}}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"tool_call","callId":"call-2","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"pwd","output":"/","exitCode":0}}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"C"}},
    {"type":"usage_updated","provider":"fake","turnId":"turn-4","usage":{"inputTokens":1}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"D"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-4"}
  ],
  "held": [
    {"type":"turn_started","provider":"fake","turnId":"turn-5"}
  ],
  "ask": [
    {"type":"turn_started","provider":"fake","turnId":"turn-7"},
    {"type":"permission_requested","provider":"fake","turnId":"turn-7","request":{"id":"perm-1","provider":"fake","name":"shell","kind":"tool","input":{"command":"rm x","empty":{}},"actions":[{"id":"allow","label":"Allow","behavior":"allow"}]}}
  ],
  "response": [
    {"type":"permission_resolved","provider":"fake","turnId":"turn-7","requestId":"perm-1","resolution":{"behavior":"allow"}},
    {"type":"timeline","provider":"fake","turnId":"turn-7","item":{"type":"assistant_message","text":"Removed."}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-7"}
  ],
  "long": [
    {"type":"turn_started","provider":"fake","turnId":"turn-8"},
    {"type":"timeline","provider":"fake","turnId":"turn-8","item":{"type":"reasoning","text":"long task"}}
  ],
  "interrupt": [
    {"type":"turn_canceled","provider":"fake","turnId":"turn-8","reason":"interrupted by user"}
  ],
  "startSlowDone": [
    {"type":"__startDelay","ms":150},
    {"type":"turn_started","provider":"fake","turnId":"turn-11"},
    {"type":"turn_completed","provider":"fake","turnId":"turn-11"}
  ],
  "startFail": [
    {"type":"__startFail","ms":50,"message":"spawn failed"}
  ],
  "startSlowAbort": [
    {"type":"__startDelay","ms":300},
    {"type":"turn_started","provider":"fake","turnId":"turn-12"},
    {"type":"turn_completed","provider":"fake","turnId":"turn-12"}
  ],
  "slowStart": [
    {"type":"__startDelay","ms":300},
    {"type":"turn_started","provider":"fake","turnId":"turn-9"}
  ],
  "subagents": [
    {"type":"turn_started","provider":"fake","turnId":"turn-10"},
    {"type":"provider_subagent","provider":"fake","event":{"type":"upsert","id":"child-1","title":"Explore","cwd":"/w/child","status":"running","timestamp":"2026-07-12T10:00:00.000Z"}},
    {"type":"provider_subagent","provider":"fake","event":{"type":"timeline","id":"child-1","item":{"type":"assistant_message","text":"Found it."},"timestamp":"2026-07-12T10:00:01.000Z"}},
    {"type":"provider_subagent","provider":"fake","event":{"type":"upsert","id":"child-2","title":"Review","status":"completed","timestamp":"2026-07-12T09:00:00.000Z"}},
    {"type":"timeline","provider":"fake","turnId":"turn-10","item":{"type":"assistant_message","text":"Delegated."}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-10"}
  ],
  "history": [
    {"type":"timeline","provider":"fake","item":{"type":"user_message","text":"<paseo-system>\ninjected\n</paseo-system>"}},
    {"type":"timeline","provider":"fake","item":{"type":"user_message","text":"old question","messageId":"m-1"},"timestamp":"2026-07-12T08:00:00.000Z"},
    {"type":"usage_updated","provider":"fake","usage":{"inputTokens":9}},
    {"type":"provider_subagent","provider":"fake","event":{"type":"upsert","id":"child-h","title":"From history","status":"completed","timestamp":"2026-07-12T08:00:01.000Z"}},
    {"type":"timeline","provider":"fake","item":{"type":"assistant_message","text":"old answer"},"timestamp":""},
    {"type":"timeline","provider":"fake","item":{"type":"tool_call","callId":"h-1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"ls","output":"a"}}}
  ],
  "rpIdle": [
    {"type":"turn_started","provider":"fake","turnId":"turn-13"},
    {"type":"timeline","provider":"fake","turnId":"turn-13","item":{"type":"assistant_message","text":"idle replace"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-13"}
  ],
  "rpAfter": [
    {"type":"turn_started","provider":"fake","turnId":"turn-15"},
    {"type":"timeline","provider":"fake","turnId":"turn-15","item":{"type":"assistant_message","text":"replaced"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-15"}
  ],
  "rpFinishing": [
    {"type":"turn_started","provider":"fake","turnId":"turn-14"},
    {"type":"timeline","provider":"fake","turnId":"turn-14","item":{"type":"assistant_message","text":"finishing"}},
    {"type":"__delay","ms":150},
    {"type":"turn_completed","provider":"fake","turnId":"turn-14"}
  ],
  "rpAfterFinish": [
    {"type":"turn_started","provider":"fake","turnId":"turn-16"},
    {"type":"timeline","provider":"fake","turnId":"turn-16","item":{"type":"assistant_message","text":"after finish"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-16"}
  ],
  "rpHeld": [
    {"type":"turn_started","provider":"fake","turnId":"turn-17"},
    {"type":"__delay","ms":400},
    {"type":"turn_completed","provider":"fake","turnId":"turn-17"}
  ],
  "permission": [
    {"type":"turn_started","provider":"fake","turnId":"turn-6"},
    {"type":"__delay","ms":100},
    {"type":"permission_requested","provider":"fake","turnId":"turn-6","request":{"id":"perm-1","provider":"fake","name":"shell","kind":"tool","title":"Run rm","input":{"command":"rm -rf build"},"actions":[{"id":"allow","label":"Allow","behavior":"allow"},{"id":"deny","label":"Deny","behavior":"deny"}]}}
  ]
}"#;

/// `[config, agentId or null]` for each `createAgent` of the `errors`
/// scenario; `$CWD` stands for the disposable working directory.
const ERROR_CASES: &str = r#"[
  [{"provider":"fake","cwd":"/nonexistent/spocky-cwd"}, null],
  [{"provider":"fake","cwd":"/etc/hosts"}, null],
  [{"provider":"nope","cwd":"$CWD"}, null],
  [{"provider":"gone","cwd":"$CWD"}, null],
  [{"provider":"broken","cwd":"$CWD"}, null],
  [{"provider":"off","cwd":"$CWD"}, null],
  [{"provider":"fake","cwd":"$CWD","providerOptions":{"a":1}}, null],
  [{"provider":"fake","cwd":"$CWD","toolPolicy":{"preapproved":[]}}, null],
  [{"provider":"tools","cwd":"$CWD","toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"}]}}, null],
  [{"provider":"tools","cwd":"$CWD","toolPolicy":{"preapproved":[null]}}, null],
  [{"provider":"tools","cwd":"$CWD","mcpServers":{"s":{"type":"stdio","command":"x"}},"toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"}]}}, "00000000-0000-4000-8000-0000000000b2"],
  [{"provider":"nomcp","cwd":"$CWD","mcpServers":{"m":{"type":"stdio","command":"x"}}}, null],
  [{"provider":"fake","cwd":"$CWD"}, "not-a-uuid"],
  [{"provider":"fake","cwd":"$CWD"}, "00000000-0000-4000-8000-0000000000a1"],
  [{"provider":"fake","cwd":"$CWD"}, "00000000-0000-4000-8000-0000000000a1"]
]"#;

const RUNTIME_INFO: &str =
    r#"{"provider":"fake","sessionId":"sess-1","model":"model-default","modeId":"auto"}"#;
const PERSISTENCE: &str =
    r#"{"provider":"fake","sessionId":"sess-1","nativeHandle":"thread-1","metadata":{"x":1}}"#;
const CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true}"#;
const MODES: &str = r#"[{"id":"auto","label":"Auto"},{"id":"read-only","label":"Read only"}]"#;
const CATALOG: &str = r#"{"models":[{"provider":"fake","id":"model-a","label":"A"},{"provider":"fake","id":"model-default","label":"D","isDefault":true}],"modes":[]}"#;

const NODE_SCRIPT: &str = r#"
const [dist, agentId, otherId, unknownId, turnEventsJson, scenarioTurnsJson, errorCasesJson, runtimeInfoJson, persistenceJson, capabilitiesJson, modesJson, catalogJson, cwd, home] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { AgentManager } = await import(`${dist}/server/agent/agent-manager.js`);
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const { toAgentPayload } = await import(`${dist}/server/agent/agent-projections.js`);
const fs = await import("node:fs");
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const turnIdOf = (events) => events.find((event) => event.type === "turn_started")?.turnId ?? "turn-1";
class FakeSession {
  constructor(spec, calls) { this.provider = spec.provider; this.id = "sess-1"; this.capabilities = spec.capabilities; this.spec = spec; this.calls = calls; this.listeners = []; if (spec.initialTimeline) this.initialTimeline = spec.initialTimeline; }
  subscribe(callback) { this.listeners.push(callback); return () => {}; }
  emitLater(events, ms) {
    setTimeout(async () => {
      for (const event of events) {
        if (event.type === "__delay") { await sleep(event.ms); continue; }
        if (event.type === "__startDelay") continue;
        for (const l of this.listeners) l(event);
      }
    }, ms);
  }
  async startTurn(prompt, options) {
    this.calls.push(["startTurn", prompt, options ?? null]);
    const events = this.spec.turns.shift() ?? JSON.parse(turnEventsJson);
    if (events[0]?.type === "__startFail") { await sleep(events[0].ms); throw new Error(events[0].message); }
    this.emitLater(events, 20);
    if (events[0]?.type === "__startDelay") await sleep(events[0].ms);
    return { turnId: turnIdOf(events) };
  }
  async run() { throw new Error("unused"); }
  tryHandleOutOfBand(prompt) {
    this.calls.push(["tryHandleOutOfBand", prompt]);
    if (typeof prompt !== "string" || !prompt.startsWith("/goal")) return null;
    return {
      run: async ({ emit }) => {
        if (prompt === "/goal fail") throw new Error("goal broke");
        emit({ type: "timeline", provider: "fake", item: { type: "assistant_message", text: "Goal paused." } });
        emit({ type: "usage_updated", provider: "fake", usage: { inputTokens: 1 } });
      },
    };
  }
  async *streamHistory() { for (const event of this.spec.history ?? []) yield event; }
  async getRuntimeInfo() { return JSON.parse(runtimeInfoJson); }
  async getAvailableModes() { return JSON.parse(modesJson); }
  async getCurrentMode() { return "auto"; }
  async setMode() {}
  getPendingPermissions() { return []; }
  async respondToPermission(requestId, response) {
    this.calls.push(["respondToPermission", requestId, response]);
    if (this.spec.response) this.emitLater(this.spec.response, 200);
  }
  describePersistence() { return JSON.parse(persistenceJson); }
  async interrupt() {
    this.calls.push(["interrupt"]);
    if (this.spec.interruptHang) await new Promise(() => {});
    if (this.spec.interrupt) this.emitLater(this.spec.interrupt, 10);
  }
  async close() { this.calls.push(["close"]); }
}
const spec = (provider, overrides = {}) => ({ provider, capabilities: JSON.parse(capabilitiesJson), available: true, turns: [], ...overrides });
const fakeClient = (calls, spec) => ({
  provider: spec.provider,
  capabilities: spec.capabilities,
  async createSession(config, launchContext, options) {
    calls.push(["createSession", config, launchContext ?? null, options ?? null]);
    if (spec.createDelay) await sleep(spec.createDelay);
    return new FakeSession(spec, calls);
  },
  async resumeSession(handle, overrides, launchContext, options) {
    calls.push(["resumeSession", handle, overrides ?? null, launchContext ?? null, options ?? null]);
    return new FakeSession(spec, calls);
  },
  async archiveNativeSession(handle) {
    calls.push(["archiveNativeSession", handle]);
    if (spec.archiveFails) throw new Error("native archive failed");
  },
  async unarchiveNativeSession(handle) { calls.push(["unarchiveNativeSession", handle]); },
  async fetchCatalog(options, context) { calls.push(["fetchCatalog", options, context === undefined ? "no context" : "context"]); return JSON.parse(catalogJson); },
  async isAvailable() {
    if (typeof spec.available === "boolean") return spec.available;
    throw new Error(spec.available);
  },
});
const recordFeed = (manager) => {
  const feed = [];
  manager.subscribe((event) => {
    if (event.type === "agent_state") feed.push(["agent_state", toAgentPayload(event.agent)]);
    else if (event.type === "agent_stream") feed.push(["agent_stream", event.agentId, event.event, event.seq ?? null, event.epoch ?? null, event.timestamp ?? null]);
    else if (event.type === "provider_subagent") feed.push(["provider_subagent", event.event]);
    else feed.push([event.type]);
  });
  return feed;
};
const outcome = async (run) => {
  try {
    return { ok: await run() };
  } catch (error) {
    return { name: error.name, message: error.message };
  }
};
const waitResult = (wait) => ({ status: wait.status, permission: wait.permission, lastMessage: wait.lastMessage });

const main = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/main`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  const created = toAgentPayload(await manager.createAgent(
    { provider: "fake", cwd, title: "  Fake title  ", model: " default " },
    agentId,
    { workspaceId: "wks_1", labels: { surface: "workspace" }, env: { EXTRA: "1" } },
  ));
  const run = await manager.runAgent(agentId, "hello", { clientMessageId: "client-1" });
  const wait = await manager.waitForAgentEvent(agentId);
  await sleep(100);
  await manager.flush();
  await registry.flush();
  const directory = `${home}/main/${fs.readdirSync(`${home}/main`)[0]}`;
  const record = JSON.parse(fs.readFileSync(`${directory}/${agentId}.json`, "utf8"));
  return {
    calls,
    created,
    run,
    wait: waitResult(wait),
    rows: await manager.getTimelineRows(agentId),
    fetch: manager.fetchTimeline(agentId, { direction: "tail", limit: 3 }),
    feed,
    record,
  };
};

const errors = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/errors`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: {
      fake: fakeClient(calls, spec("fake")),
      gone: fakeClient(calls, spec("gone", { available: false })),
      broken: fakeClient(calls, spec("broken", { available: "missing binary" })),
      off: fakeClient(calls, spec("off")),
      tools: fakeClient(calls, spec("tools")),
      nomcp: fakeClient(calls, spec("nomcp", { capabilities: { ...JSON.parse(capabilitiesJson), supportsMcpServers: false } })),
    },
    providerDefinitions: {
      fake: { enabled: true },
      gone: { enabled: true },
      broken: { enabled: true },
      off: { enabled: false },
      tools: { enabled: true, applyToolPolicy: (config, toolPolicy) => ({ ...config, toolPolicy }) },
      nomcp: { enabled: true },
    },
  });
  const results = [];
  for (const [config, id] of JSON.parse(errorCasesJson.replaceAll("$CWD", cwd))) {
    results.push(await outcome(async () => (await manager.createAgent(config, id ?? undefined, {})).id));
  }
  results.push(await outcome(() => manager.runAgent(unknownId, "hi")));
  results.push(await outcome(() => manager.waitForAgentEvent(unknownId)));
  results.push(await outcome(async () => { manager.subscribe(() => {}, { agentId: "bad" }); return null; }));
  await manager.flush();
  await registry.flush();
  return { results, calls };
};

const turns = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/turns`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.failed, scripted.canceled, scripted.coalesce, scripted.held] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const run = (text) => outcome(() => manager.runAgent(agentId, text));
  const wait = (options) => outcome(async () => waitResult(await manager.waitForAgentEvent(agentId, options)));
  const results = [];
  results.push(await run("fail"));
  results.push(await wait());
  results.push(await run("cancel"));
  results.push(await wait());
  results.push(await run("coalesce"));
  results.push(await wait({ waitForActive: true }));
  manager.runAgent(agentId, "hold").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-5";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-5 never started");
    await sleep(5);
  }
  const pre = new AbortController();
  pre.abort("pre");
  results.push(await wait({ signal: pre.signal }));
  const stop = new AbortController();
  setTimeout(() => stop.abort("stop"), 30);
  results.push(await wait({ signal: stop.signal }));
  const timeout = new AbortController();
  setTimeout(() => timeout.abort(new Error("wait timeout")), 30);
  results.push(await wait({ signal: timeout.signal }));
  const plain = new AbortController();
  plain.abort();
  results.push(await wait({ signal: plain.signal }));
  results.push(await run("again"));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId), stored: await registry.get(agentId) };
};

const permission = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/permission`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.permission] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  manager.runAgent(agentId, "ask").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-6";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-6 never started");
    await sleep(5);
  }
  const wait = () => outcome(async () => waitResult(await manager.waitForAgentEvent(agentId)));
  const results = [await wait(), await wait()];
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId) };
};

const lifecycle = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/lifecycle`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.ask, scripted.long, scripted.slowStart], response: scripted.response, interrupt: scripted.interrupt })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const second = manager.streamAgent(agentId, "remove x");
  const secondEvents = [(await second.next()).value];
  const permissionWait = waitResult(await manager.waitForAgentEvent(agentId));
  const pending = manager.getPendingPermissions(agentId);
  const respond = await manager.respondToPermission(agentId, "perm-1", { behavior: "allow" });
  await collect(second, secondEvents);
  await sleep(100);
  const third = manager.streamAgent(agentId, "long task");
  const thirdEvents = [(await third.next()).value];
  await sleep(50);
  const cancel = (await manager.cancelAgentRun(agentId)).status;
  await collect(third, thirdEvents);
  const cancelAgain = (await manager.cancelAgentRun(agentId)).status;
  await sleep(100);
  const starting = outcome(() => manager.runAgent(agentId, "slow"));
  await sleep(100);
  const cancelStarting = (await manager.cancelAgentRun(agentId)).status;
  const startingResult = await starting;
  await sleep(400);
  const rows = await manager.getTimelineRows(agentId);
  const fetch = manager.fetchTimeline(agentId, { direction: "tail", limit: 3 });
  await manager.closeAgent(agentId);
  await manager.closeAgent(agentId);
  await sleep(100);
  await manager.flush();
  await registry.flush();
  const directory = `${home}/lifecycle/${fs.readdirSync(`${home}/lifecycle`)[0]}`;
  const record = JSON.parse(fs.readFileSync(`${directory}/${agentId}.json`, "utf8"));
  return { secondEvents, permissionWait, pending, respond: respond ?? null, thirdEvents, cancel, cancelAgain, cancelStarting, startingResult, rows, fetch, calls, feed, record };
};

const subagents = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/subagents`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.subagents] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const run = await outcome(() => manager.runAgent(agentId, "delegate"));
  await sleep(100);
  const queries = [
    await outcome(async () => manager.listProviderSubagents(agentId)),
    await outcome(async () => manager.listProviderSubagentActivity()),
    await outcome(async () => manager.getProviderSubagent(agentId, "child-1")),
    await outcome(async () => manager.getProviderSubagent(agentId, "nope")),
    await outcome(async () => manager.fetchProviderSubagentTimeline(agentId, "child-1", { direction: "tail", limit: 5 })),
    await outcome(async () => manager.listProviderSubagents(unknownId)),
  ];
  await manager.closeAgent(agentId);
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { run, queries, after: manager.listProviderSubagentActivity(), calls, feed };
};

const hydration = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/hydration`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { history: scripted.history })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const steps = [];
  const step = async (options) => {
    steps.push(await outcome(async () => { await manager.hydrateTimelineFromProvider(agentId, options); return null; }));
    steps.push(await manager.getTimelineRows(agentId));
  };
  await step(undefined);
  await step({ force: true, broadcast: true });
  await step({ force: true, broadcast: true, broadcastTimeline: false });
  await step({ force: true });
  steps.push(await outcome(async () => { await manager.hydrateTimelineFromProvider(unknownId); return null; }));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { steps, subagents: manager.listProviderSubagents(agentId), feed };
};

const resume = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/resume`, logger);
  const fakeSpec = spec("fake", { history: scripted.history, initialTimeline: [{ item: { type: "assistant_message", text: "startup row" }, timestamp: "2026-07-12T07:00:00.000Z" }] });
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, fakeSpec), gone: fakeClient(calls, spec("gone", { available: false })) },
    providerDefinitions: { fake: { enabled: true }, gone: { enabled: true } },
  });
  const feed = recordFeed(manager);
  const handle = { provider: "fake", sessionId: "sess-r", nativeHandle: "thread-r", metadata: { cwd, model: "model-a", title: "Stored" } };
  const results = [];
  results.push(await outcome(async () => toAgentPayload(await manager.resumeAgentFromPersistence(
    handle,
    { modeId: "auto" },
    agentId,
    {
      createdAt: new Date(1700000000000),
      updatedAt: new Date(1700000005000),
      lastUserMessageAt: new Date(1700000004000),
      labels: { surface: "workspace" },
      workspaceId: "wks_9",
      attention: { requiresAttention: true, attentionReason: "finished", attentionTimestamp: new Date(1700000006000) },
    },
    { purpose: "interactive" },
  ))));
  results.push(await manager.getTimelineRows(agentId));
  results.push(await outcome(async () => { await manager.hydrateTimelineFromProvider(agentId); return null; }));
  await registry.upsert({ id: otherId, provider: "fake", cwd: "/nonexistent/spocky-archived", archivedAt: "2026-07-01T00:00:00.000Z" });
  results.push(await outcome(async () => toAgentPayload(await manager.resumeAgentFromPersistence(
    { provider: "fake", sessionId: "sess-a", metadata: { cwd: "/nonexistent/spocky-archived" } },
    undefined,
    otherId,
  ))));
  results.push(await outcome(async () => (await manager.resumeAgentFromPersistence({ provider: "nope", sessionId: "x", metadata: { cwd } })).id));
  results.push(await outcome(async () => (await manager.resumeAgentFromPersistence({ provider: "gone", sessionId: "x", metadata: { cwd } })).id));
  results.push(await outcome(async () => (await manager.resumeAgentFromPersistence(handle, undefined, "not-a-uuid")).id));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, stored: await registry.get(agentId) };
};

const titles = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/titles`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.held] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const results = [manager.hasInFlightRun(agentId), manager.hasInFlightRun(unknownId)];
  results.push(await outcome(async () => { await manager.setTitle(agentId, "  New title  "); return null; }));
  results.push(await outcome(async () => { await manager.setTitle(agentId, "   "); return null; }));
  results.push(await outcome(async () => { await manager.setTitle(unknownId, "x"); return null; }));
  manager.runAgent(agentId, "hold").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-5";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-5 never started");
    await sleep(5);
  }
  results.push(manager.hasInFlightRun(agentId));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, feed, stored: await registry.get(agentId) };
};

const runstart = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/runstart`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.startSlowDone, scripted.startFail, scripted.startSlowAbort] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const start = (id, options) => outcome(async () => { await manager.waitForAgentRunStart(id, options); return "started"; });
  const finished = async (run) => {
    const settled = await run;
    return typeof settled === "string" ? settled : settled.finalText;
  };
  const results = [];
  results.push(await start(unknownId));
  results.push(await start(agentId));
  const slow = manager.runAgent(agentId, "slow").catch((error) => error.message);
  results.push(await start(agentId));
  results.push(await outcome(() => finished(slow)));
  const failed = manager.runAgent(agentId, "fail").catch((error) => error.message);
  results.push(await start(agentId));
  results.push(await outcome(() => finished(failed)));
  const aborted = manager.runAgent(agentId, "abort").catch((error) => error.message);
  const stop = new AbortController();
  setTimeout(() => stop.abort("stop"), 30);
  results.push(await start(agentId, { signal: stop.signal }));
  const pre = new AbortController();
  pre.abort(new Error("pre"));
  results.push(await start(agentId, { signal: pre.signal }));
  results.push(await start(agentId));
  results.push(await outcome(() => finished(aborted)));
  results.push(await start(agentId));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls };
};

const outofband = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/outofband`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const results = [];
  results.push(await outcome(async () => manager.tryRunOutOfBand(agentId, "hello")));
  results.push(await outcome(async () => manager.tryRunOutOfBand(agentId, "/goal pause", { clientMessageId: "client-oob" })));
  await sleep(50);
  results.push(await outcome(async () => manager.tryRunOutOfBand(agentId, "/goal fail", { clientMessageId: "" })));
  await sleep(50);
  results.push(await outcome(async () => manager.tryRunOutOfBand(unknownId, "/goal pause")));
  await sleep(50);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId) };
};

const shutdown = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/shutdown`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake", { createDelay: 150 })) }, providerDefinitions: { fake: { enabled: true } } });
  const order = [];
  const create = (id, name) => manager.createAgent({ provider: "fake", cwd }, id, {}).then(
    () => { order.push(`created ${name}`); },
    (error) => { order.push(`failed ${name}: ${error.message}`); },
  );
  const first = create(agentId, "first");
  await sleep(20);
  await manager.flush();
  order.push("flush done");
  await first;
  const second = create(otherId, "second");
  await sleep(20);
  manager.prepareForShutdown();
  await manager.flushForShutdown();
  order.push("flushForShutdown done");
  await second;
  await create(unknownId, "third");
  await registry.flush();
  return { order, calls };
};

const loading = async () => {
  const { ensureAgentLoaded } = await import(`${dist}/server/agent/agent-loading.js`);
  const calls = [];
  const registry = new AgentStorage(`${home}/loading`, logger);
  const options = () => ({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const first = new AgentManager(options());
  await first.createAgent({ provider: "fake", cwd, model: "m1" }, agentId, { labels: { surface: "x" }, workspaceId: "wks_1" });
  await first.flush();
  await registry.flush();
  const stored = await registry.get(agentId);
  await registry.upsert({ ...stored, id: otherId, persistence: null, title: "No handle" });
  await registry.upsert({ ...stored, id: "00000000-0000-4000-8000-0000000000c3", provider: "ghost" });
  const second = new AgentManager(options());
  const feed = recordFeed(second);
  const deps = (broadcastTimeline) => ({ agentManager: second, agentStorage: registry, logger, broadcastTimeline });
  const load = (id, broadcastTimeline = false) => outcome(async () => toAgentPayload(await ensureAgentLoaded(id, deps(broadcastTimeline))));
  const results = await Promise.all([load(agentId), load(agentId, true)]);
  results.push(await load(agentId));
  results.push(await load(otherId));
  results.push(await load("00000000-0000-4000-8000-0000000000c3"));
  results.push(await load("00000000-0000-4000-8000-0000000000d4"));
  await sleep(50);
  await second.flush();
  await registry.flush();
  return { results, calls, feed };
};

const replaceScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const build = async (name, turns, specExtra, managerExtra = {}) => {
    const calls = [];
    const registry = new AgentStorage(`${home}/replace-${name}`, logger);
    const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake", { turns, ...specExtra })) }, providerDefinitions: { fake: { enabled: true } }, ...managerExtra });
    const feed = recordFeed(manager);
    await manager.createAgent({ provider: "fake", cwd }, agentId, {});
    return { calls, registry, manager, feed };
  };
  const finish = async ({ calls, registry, manager, feed }, extra) => {
    await sleep(100);
    await manager.flush();
    await registry.flush();
    return { ...extra, calls, feed, agent: toAgentPayload(manager.getAgent(agentId)), rows: await manager.getTimelineRows(agentId) };
  };
  const running = await build("running", [scripted.rpIdle, scripted.long, scripted.rpAfter], { interrupt: scripted.interrupt });
  const idleEvents = await collect(await running.manager.replaceAgentRun(agentId, "idle prompt"), []);
  await sleep(50);
  const old = running.manager.streamAgent(agentId, "long task");
  const oldEvents = [(await old.next()).value];
  await sleep(50);
  const replacement = await running.manager.replaceAgentRun(agentId, "replace it");
  await collect(old, oldEvents);
  const newEvents = await collect(replacement, []);
  const a = await finish(running, { idleEvents, oldEvents, newEvents });
  const finishing = await build("finishing", [scripted.rpFinishing, scripted.rpAfterFinish], {});
  const first = finishing.manager.streamAgent(agentId, "finishing");
  const firstEvents = [(await first.next()).value];
  await sleep(30);
  const startedAt = Date.now();
  const second = await finishing.manager.replaceAgentRun(agentId, "while finishing");
  const waited = Date.now() - startedAt >= 80;
  await collect(first, firstEvents);
  const secondEvents = await collect(second, []);
  const b = await finish(finishing, { firstEvents, secondEvents, waited });
  const refused = await build("refused", [scripted.rpHeld], { interruptHang: true }, { rescueTimeouts: { interruptSessionMs: 80 } });
  const held = refused.manager.streamAgent(agentId, "long task");
  const heldEvents = [(await held.next()).value];
  await sleep(50);
  const failure = await outcome(async () => await refused.manager.replaceAgentRun(agentId, "never"));
  const again = await outcome(async () => { refused.manager.streamAgent(agentId, "again"); return null; });
  await collect(held, heldEvents);
  const c = await finish(refused, { heldEvents, failure, again });
  const starting = await build("starting", [scripted.slowStart, scripted.rpAfterFinish], {});
  const slow = starting.manager.streamAgent(agentId, "slow");
  const slowEvents = [];
  const slowDone = collect(slow, slowEvents);
  await sleep(100);
  const afterSlow = await starting.manager.replaceAgentRun(agentId, "while starting");
  await slowDone;
  const afterSlowEvents = await collect(afterSlow, []);
  const d = await finish(starting, { slowEvents, afterSlowEvents });
  return { a, b, c, d };
};

const archive = async () => {
  const PARENT_LABEL = "paseo.parent-agent-id";
  const ids = { same: "00000000-0000-4000-8000-0000000000e1", other: "00000000-0000-4000-8000-0000000000e2", tab: "00000000-0000-4000-8000-0000000000e3", stored: "00000000-0000-4000-8000-0000000000e4" };
  const calls = [];
  const registry = new AgentStorage(`${home}/archive`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  const create = (id, labels, workspaceId) => manager.createAgent({ provider: "fake", cwd }, id, { labels, workspaceId });
  await create(agentId, {}, "wks_1");
  await create(ids.same, { [PARENT_LABEL]: agentId }, "wks_1");
  await create(ids.other, { [PARENT_LABEL]: agentId }, "wks_2");
  await create(ids.tab, { [PARENT_LABEL]: agentId, "paseo.open-agent-tab.c1": "true" }, "wks_1");
  await create(ids.stored, { [PARENT_LABEL]: agentId }, "wks_1");
  await manager.closeAgent(ids.stored);
  await manager.flush();
  await registry.flush();
  const results = [];
  results.push(await outcome(async () => (await manager.archiveAgent(agentId)).archivedAt));
  await manager.flush();
  await registry.flush();
  const stored = {};
  for (const [name, id] of Object.entries({ parent: agentId, ...ids })) stored[name] = await registry.get(id);
  results.push(await outcome(async () => await manager.unarchiveSnapshot(agentId, { workspaceId: "wks_3", labels: { a: "b", gone: null } })));
  results.push(await outcome(async () => await manager.unarchiveSnapshot(agentId)));
  results.push(await outcome(async () => await manager.unarchiveSnapshot(unknownId)));
  results.push(await outcome(async () => { const { record, live, previousParentAgentId } = await manager.detachAgent(ids.same); return { id: record.id, labels: record.labels, live, previousParentAgentId }; }));
  results.push(await outcome(async () => { await manager.clearAgentAttention(ids.same); return null; }));
  results.push(await outcome(async () => { await manager.clearAgentAttention(unknownId); return null; }));
  await manager.flush();
  await registry.flush();
  const afterStored = {};
  for (const [name, id] of Object.entries({ parent: agentId, ...ids })) afterStored[name] = await registry.get(id);
  const byHandleCalls = [];
  const warns = [];
  const warnLogger = { ...logger, child() { return this; }, warn(bindings, message) { warns.push([bindings, message]); } };
  const byHandleRegistry = new AgentStorage(`${home}/archive-handle`, logger);
  const byHandle = new AgentManager({ logger: warnLogger, registry: byHandleRegistry, clients: { fake: fakeClient(byHandleCalls, spec("fake", { archiveFails: true })) }, providerDefinitions: { fake: { enabled: true } } });
  await byHandle.createAgent({ provider: "fake", cwd }, agentId, { labels: {}, workspaceId: "wks_1" });
  await byHandle.closeAgent(agentId);
  await byHandleRegistry.flush();
  const archivedRecord = await outcome(async () => await byHandle.archiveSnapshot(agentId, "2026-07-12T10:00:00.000Z"));
  const handle = (await byHandleRegistry.get(agentId)).persistence;
  const unarchived = await outcome(async () => { await byHandle.unarchiveSnapshotByHandle(handle); return null; });
  await byHandleRegistry.flush();
  return { results, stored, afterStored, calls, feed, byHandle: { archivedRecord, unarchived, record: await byHandleRegistry.get(agentId), calls: byHandleCalls, warns } };
};

process.stdout.write(JSON.stringify({ main: await main(), errors: await errors(), turns: await turns(), permission: await permission(), lifecycle: await lifecycle(), subagents: await subagents(), hydration: await hydration(), resume: await resume(), titles: await titles(), runstart: await runstart(), outofband: await outofband(), shutdown: await shutdown(), loading: await loading(), replace: await replaceScenario(), archive: await archive() }));
"#;

fn json(text: &str) -> JsValue {
    parse(text).expect("fixture JSON")
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

#[allow(clippy::cast_precision_loss, reason = "sequence numbers are small")]
fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
}

/// `isAvailable()`: resolves `Ok`, or rejects with `Err`'s message.
type Availability = Result<bool, String>;

/// One fake provider: what it reports and the turns its sessions play.
#[derive(Clone)]
struct Spec {
    provider: String,
    capabilities: JsValue,
    available: Availability,
    turns: Arc<Mutex<VecDeque<JsValue>>>,
    /// Emitted 200 ms after `respondToPermission`.
    response: Option<JsValue>,
    /// Emitted 10 ms after `interrupt`.
    interrupt: Option<JsValue>,
    /// What `streamHistory()` yields.
    history: Option<JsValue>,
    /// `session.initialTimeline`: `[{ item, timestamp }]`.
    initial_timeline: Option<JsValue>,
    /// `createSession` resolves after this long.
    create_delay: Option<Duration>,
    /// `archiveNativeSession` rejects.
    archive_fails: bool,
    /// `interrupt` never resolves.
    interrupt_hang: bool,
}

fn spec(provider: &str) -> Spec {
    Spec {
        provider: provider.to_owned(),
        capabilities: json(CAPABILITIES),
        available: Ok(true),
        turns: Arc::new(Mutex::new(VecDeque::new())),
        response: None,
        interrupt: None,
        history: None,
        initial_timeline: None,
        create_delay: None,
        archive_fails: false,
        interrupt_hang: false,
    }
}

struct FakeSession {
    spec: Spec,
    listeners: Arc<Mutex<Vec<StreamCallback>>>,
    calls: Arc<Mutex<Vec<JsValue>>>,
}

/// `async *streamHistory()` over the scripted history.
struct History(std::vec::IntoIter<JsValue>);

impl AgentEventStream for History {
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>> {
        let next = self.0.next();
        Box::pin(async move { next.map(Ok) })
    }
}

fn prompt_value(prompt: &AgentPromptInput) -> JsValue {
    match prompt {
        AgentPromptInput::Text(text) => JsValue::String(text.clone()),
        AgentPromptInput::Blocks(blocks) => JsValue::Array(blocks.clone()),
    }
}

fn turn_id_of(events: &JsValue) -> String {
    events
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event.get("type").and_then(JsValue::as_str) == Some("turn_started"))
        .and_then(|event| event.get("turnId").and_then(JsValue::as_str))
        .unwrap_or("turn-1")
        .to_owned()
}

fn event_type(event: &JsValue) -> Option<&str> {
    event.get("type").and_then(JsValue::as_str)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "scripted delays are small whole milliseconds"
)]
fn delay(event: &JsValue) -> Duration {
    Duration::from_millis(event.get("ms").and_then(JsValue::as_f64).expect("ms") as u64)
}

impl FakeSession {
    /// Emits the scripted `events` to every listener after `millis`, as
    /// `setTimeout`, pausing at `__delay` entries.
    fn emit_later(&self, events: JsValue, millis: u64) {
        let listeners = Arc::clone(&self.listeners);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(millis)).await;
            let callbacks = listeners.lock().expect("listeners").clone();
            for event in events.as_array().expect("events") {
                match event_type(event) {
                    Some("__delay") => tokio::time::sleep(delay(event)).await,
                    Some("__startDelay") => {}
                    _ => {
                        for callback in &callbacks {
                            callback(event.clone());
                        }
                    }
                }
            }
        });
    }

    fn record(&self, call: Vec<JsValue>) {
        self.calls.lock().expect("calls").push(JsValue::Array(call));
    }
}

/// The `/goal` handler of the fake session's `tryHandleOutOfBand`.
struct GoalHandler {
    fail: bool,
}

impl OutOfBandHandler for GoalHandler {
    fn run(self: Box<Self>, emit: StreamCallback) -> BoxFuture<'static, AgentResult<()>> {
        Box::pin(async move {
            if self.fail {
                return Err(AgentError::new("goal broke"));
            }
            emit(json(
                r#"{"type":"timeline","provider":"fake","item":{"type":"assistant_message","text":"Goal paused."}}"#,
            ));
            emit(json(
                r#"{"type":"usage_updated","provider":"fake","usage":{"inputTokens":1}}"#,
            ));
            Ok(())
        })
    }
}

impl AgentSession for FakeSession {
    fn try_handle_out_of_band(
        &self,
        prompt: &AgentPromptInput,
    ) -> Option<Option<Box<dyn OutOfBandHandler>>> {
        self.record(vec![text("tryHandleOutOfBand"), prompt_value(prompt)]);
        Some(match prompt {
            AgentPromptInput::Text(text) if text.starts_with("/goal") => {
                Some(Box::new(GoalHandler {
                    fail: text == "/goal fail",
                }) as Box<dyn OutOfBandHandler>)
            }
            _ => None,
        })
    }
    fn provider(&self) -> String {
        self.spec.provider.clone()
    }
    fn id(&self) -> Option<String> {
        Some("sess-1".to_owned())
    }
    fn capabilities(&self) -> JsValue {
        self.spec.capabilities.clone()
    }
    fn run(
        &self,
        _prompt: AgentPromptInput,
        _options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
    fn start_turn(
        &self,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>> {
        let mut call = vec![text("startTurn"), prompt_value(&prompt)];
        call.push(options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(id) = options.client_message_id {
                value.insert("clientMessageId", JsValue::String(id));
            }
            JsValue::Object(value)
        }));
        self.calls.lock().expect("calls").push(JsValue::Array(call));
        let events = self
            .spec
            .turns
            .lock()
            .expect("turns")
            .pop_front()
            .unwrap_or_else(|| json(TURN_EVENTS));
        if let Some(failure) = events
            .as_array()
            .and_then(|events| events.first())
            .filter(|event| event_type(event) == Some("__startFail"))
        {
            let message = failure
                .get("message")
                .and_then(JsValue::as_str)
                .expect("message")
                .to_owned();
            let wait = delay(failure);
            return Box::pin(async move {
                tokio::time::sleep(wait).await;
                Err(AgentError::new(message))
            });
        }
        let turn_id = turn_id_of(&events);
        let start_delay = events
            .as_array()
            .and_then(|events| events.first())
            .filter(|event| event_type(event) == Some("__startDelay"))
            .map(delay);
        self.emit_later(events, 20);
        Box::pin(async move {
            if let Some(start_delay) = start_delay {
                tokio::time::sleep(start_delay).await;
            }
            Ok(turn_id)
        })
    }
    fn subscribe(&self, callback: StreamCallback) -> Unsubscribe {
        self.listeners.lock().expect("listeners").push(callback);
        Box::new(|| {})
    }
    fn stream_history(&self) -> Box<dyn AgentEventStream> {
        let events = self
            .spec
            .history
            .as_ref()
            .and_then(JsValue::as_array)
            .map(<[JsValue]>::to_vec)
            .unwrap_or_default();
        Box::new(History(events.into_iter()))
    }
    fn get_runtime_info(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Ok(json(RUNTIME_INFO)) })
    }
    fn get_available_modes(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Ok(json(MODES)) })
    }
    fn get_current_mode(&self) -> BoxFuture<'_, AgentResult<Option<String>>> {
        Box::pin(async { Ok(Some("auto".to_owned())) })
    }
    fn set_mode(&self, _mode_id: &str) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        Box::pin(async { Ok(None) })
    }
    fn get_pending_permissions(&self) -> AgentResult<Vec<JsValue>> {
        Ok(Vec::new())
    }
    fn respond_to_permission(
        &self,
        request_id: &str,
        response: JsValue,
    ) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        self.record(vec![
            text("respondToPermission"),
            text(request_id),
            response,
        ]);
        if let Some(events) = self.spec.response.clone() {
            self.emit_later(events, 200);
        }
        Box::pin(async { Ok(None) })
    }
    fn initial_timeline(&self) -> Option<Vec<ImportedTimelineEntry>> {
        self.spec.initial_timeline.as_ref().map(|entries| {
            entries
                .as_array()
                .expect("entries")
                .iter()
                .map(|entry| ImportedTimelineEntry {
                    item: entry.get("item").cloned().expect("item"),
                    timestamp: entry
                        .get("timestamp")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                })
                .collect()
        })
    }
    fn describe_persistence(&self) -> Option<JsValue> {
        Some(json(PERSISTENCE))
    }
    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.record(vec![text("interrupt")]);
        if let Some(events) = self.spec.interrupt.clone() {
            self.emit_later(events, 10);
        }
        let hang = self.spec.interrupt_hang;
        Box::pin(async move {
            if hang {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn close(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("close")]));
        Box::pin(async { Ok(()) })
    }
}

fn launch_context_value(launch_context: Option<AgentLaunchContext>) -> JsValue {
    launch_context.map_or(JsValue::Null, |context| {
        let mut value = JsObject::new();
        if let Some(agent_id) = context.agent_id {
            value.insert("agentId", JsValue::String(agent_id));
        }
        if let Some(env) = context.env {
            value.insert("env", JsValue::Object(env));
        }
        JsValue::Object(value)
    })
}

struct FakeClient {
    spec: Spec,
    calls: Arc<Mutex<Vec<JsValue>>>,
}

impl AgentClient for FakeClient {
    fn provider(&self) -> String {
        self.spec.provider.clone()
    }
    fn capabilities(&self) -> JsValue {
        self.spec.capabilities.clone()
    }
    fn create_session(
        &self,
        config: JsValue,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentCreateSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        let context = launch_context_value(launch_context);
        let options = options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(persist) = options.persist_session {
                value.insert("persistSession", JsValue::Bool(persist));
            }
            JsValue::Object(value)
        });
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("createSession"),
            config,
            context,
            options,
        ]));
        let session = FakeSession {
            spec: self.spec.clone(),
            listeners: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::clone(&self.calls),
        };
        let delay = self.spec.create_delay;
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(Arc::new(session) as Arc<dyn AgentSession>)
        })
    }
    fn resume_session(
        &self,
        handle: JsValue,
        overrides: Option<JsValue>,
        launch_context: Option<AgentLaunchContext>,
        options: Option<spocky_session::agent_sdk::AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        let options = options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(purpose) = options.purpose {
                value.insert(
                    "purpose",
                    text(match purpose {
                        AgentResumePurpose::Interactive => "interactive",
                        AgentResumePurpose::History => "history",
                    }),
                );
            }
            JsValue::Object(value)
        });
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("resumeSession"),
            handle,
            overrides.unwrap_or(JsValue::Null),
            launch_context_value(launch_context),
            options,
        ]));
        let session = FakeSession {
            spec: self.spec.clone(),
            listeners: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::clone(&self.calls),
        };
        Box::pin(async move { Ok(Arc::new(session) as Arc<dyn AgentSession>) })
    }
    fn fetch_catalog(
        &self,
        options: FetchCatalogOptions,
        context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        let mut value = JsObject::new();
        match options {
            FetchCatalogOptions::Workspace { cwd, force } => {
                value.insert("scope", text("workspace"));
                value.insert("cwd", JsValue::String(cwd));
                value.insert("force", JsValue::Bool(force));
            }
            FetchCatalogOptions::Global { force } => {
                value.insert("scope", text("global"));
                value.insert("force", JsValue::Bool(force));
            }
        }
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("fetchCatalog"),
            JsValue::Object(value),
            text(if context.is_none() {
                "no context"
            } else {
                "context"
            }),
        ]));
        Box::pin(async { Ok(json(CATALOG)) })
    }
    fn archive_native_session(&self, handle: JsValue) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("archiveNativeSession"), handle]));
        let fails = self.spec.archive_fails;
        Some(Box::pin(async move {
            if fails {
                Err(AgentError::new("native archive failed"))
            } else {
                Ok(())
            }
        }))
    }
    fn unarchive_native_session(&self, handle: JsValue) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("unarchiveNativeSession"), handle]));
        Some(Box::pin(async { Ok(()) }))
    }
    fn is_available(
        &self,
        _signal: Option<AbortSignal>,
        _options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        let available = self.spec.available.clone();
        Box::pin(async move { available.map_err(AgentError::new) })
    }
}

fn feed_entry(event: &AgentManagerEvent) -> JsValue {
    match event {
        AgentManagerEvent::AgentState(agent) => JsValue::Array(vec![
            text("agent_state"),
            to_agent_payload(&agent.payload_view(), None).expect("payload"),
        ]),
        AgentManagerEvent::AgentStream {
            agent_id,
            event,
            seq,
            epoch,
            timestamp,
        } => JsValue::Array(vec![
            text("agent_stream"),
            text(agent_id),
            event.clone(),
            seq.map_or(JsValue::Null, number),
            epoch.as_deref().map_or(JsValue::Null, text),
            timestamp.as_deref().map_or(JsValue::Null, text),
        ]),
        AgentManagerEvent::TimelineReplacement { .. } => {
            JsValue::Array(vec![text("timeline_replacement")])
        }
        AgentManagerEvent::ProviderSubagent(event) => {
            JsValue::Array(vec![text("provider_subagent"), event.clone()])
        }
    }
}

type Calls = Arc<Mutex<Vec<JsValue>>>;
type Feed = Arc<Mutex<Vec<JsValue>>>;

fn manager_with(
    calls: &Calls,
    registry: &AgentStorage,
    providers: Vec<(Spec, ProviderDefinition)>,
) -> AgentManager {
    let (clients, provider_definitions) = providers
        .into_iter()
        .map(|(spec, definition)| {
            let id = spec.provider.clone();
            let client = Arc::new(FakeClient {
                spec,
                calls: Arc::clone(calls),
            }) as Arc<dyn AgentClient>;
            ((id.clone(), client), (id, definition))
        })
        .unzip();
    AgentManager::new(AgentManagerOptions {
        clients,
        provider_definitions,
        registry: Some(registry.clone()),
        ..AgentManagerOptions::default()
    })
}

fn enabled() -> ProviderDefinition {
    ProviderDefinition {
        enabled: true,
        ..ProviderDefinition::default()
    }
}

/// Subscribes a recorder of every manager event for the manager's life,
/// as the JS side never unsubscribes it.
fn record_feed(manager: &AgentManager) -> Feed {
    let feed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&feed);
    let unsubscribe = manager
        .subscribe(
            Arc::new(move |event| sink.lock().expect("feed").push(feed_entry(event))),
            SubscribeOptions::default(),
        )
        .expect("subscribe");
    std::mem::forget(unsubscribe);
    feed
}

fn outcome(result: Result<JsValue, AgentError>) -> JsValue {
    let mut value = JsObject::new();
    match result {
        Ok(ok) => value.insert("ok", ok),
        Err(error) => {
            value.insert("name", JsValue::String(error.name));
            value.insert("message", JsValue::String(error.message));
        }
    }
    JsValue::Object(value)
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key, value);
    }
    JsValue::Object(out)
}

fn read_record(directory: &Path) -> JsValue {
    let project = std::fs::read_dir(directory)
        .expect("records")
        .next()
        .expect("record directory")
        .expect("entry")
        .path();
    parse(&std::fs::read_to_string(project.join(format!("{AGENT_ID}.json"))).expect("record"))
        .expect("record JSON")
}

async fn main_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("main"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    let mut env = JsObject::new();
    env.insert("EXTRA", text("1"));
    let mut labels = JsObject::new();
    labels.insert("surface", text("workspace"));
    let created = manager
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("title", text("  Fake title  ")),
                ("model", text(" default ")),
            ]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                workspace_id: Some("wks_1".to_owned()),
                labels: Some(JsValue::Object(labels)),
                env: Some(env),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    let created = to_agent_payload(&created.payload_view(), None).expect("payload");
    let run = manager
        .run_agent(
            AGENT_ID,
            AgentPromptInput::Text("hello".to_owned()),
            Some(AgentRunOptions {
                client_message_id: Some("client-1".to_owned()),
                ..AgentRunOptions::default()
            }),
        )
        .await
        .expect("run");
    let wait = manager
        .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
        .await
        .expect("wait");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("calls", JsValue::Array(calls)),
        ("created", created),
        ("run", run.to_js()),
        ("wait", wait.to_js()),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
        (
            "fetch",
            manager
                .fetch_timeline(AGENT_ID, FetchDirection::Tail, None, Some(3))
                .expect("fetch")
                .to_js(),
        ),
        ("feed", JsValue::Array(feed)),
        ("record", read_record(&home.join("main"))),
    ])
}

async fn errors_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("errors"));
    let mut gone = spec("gone");
    gone.available = Ok(false);
    let mut broken = spec("broken");
    broken.available = Err("missing binary".to_owned());
    let mut nomcp = spec("nomcp");
    if let JsValue::Object(capabilities) = &mut nomcp.capabilities {
        capabilities.insert("supportsMcpServers", JsValue::Bool(false));
    }
    let tools = ProviderDefinition {
        enabled: true,
        apply_tool_policy: Some(Arc::new(
            |config: &JsValue, tool_policy: Option<&JsValue>| {
                let mut config = match config {
                    JsValue::Object(config) => config.clone(),
                    _ => JsObject::new(),
                };
                config.insert(
                    "toolPolicy",
                    tool_policy.cloned().unwrap_or(JsValue::Undefined),
                );
                JsValue::Object(config)
            },
        )),
        ..ProviderDefinition::default()
    };
    let manager = manager_with(
        &calls,
        &registry,
        vec![
            (spec("fake"), enabled()),
            (gone, enabled()),
            (broken, enabled()),
            (spec("off"), ProviderDefinition::default()),
            (spec("tools"), tools),
            (nomcp, enabled()),
        ],
    );
    let mut results = Vec::new();
    for case in json(&ERROR_CASES.replace("$CWD", cwd))
        .as_array()
        .expect("cases")
    {
        let case = case.as_array().expect("case");
        let agent_id = case[1].as_str().map(str::to_owned);
        let created = manager
            .create_agent(case[0].clone(), agent_id, CreateAgentOptions::default())
            .await
            .map(|snapshot| text(&snapshot.id));
        results.push(outcome(created));
    }
    results.push(outcome(
        manager
            .run_agent(UNKNOWN_ID, AgentPromptInput::Text("hi".to_owned()), None)
            .await
            .map(|run| run.to_js()),
    ));
    results.push(outcome(
        manager
            .wait_for_agent_event(UNKNOWN_ID, WaitForAgentOptions::default())
            .await
            .map(|wait| wait.to_js()),
    ));
    results.push(outcome(
        manager
            .subscribe(
                Arc::new(|_: &AgentManagerEvent| {}),
                SubscribeOptions {
                    agent_id: Some("bad".to_owned()),
                    replay_state: None,
                },
            )
            .map(|_| JsValue::Null),
    ));
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
    ])
}

fn turn_started(feed: &Feed, turn_id: &str) -> bool {
    feed.lock().expect("feed").iter().any(|entry| {
        let entry = entry.as_array().expect("entry");
        entry[0].as_str() == Some("agent_stream")
            && entry[2].get("type").and_then(JsValue::as_str) == Some("turn_started")
            && entry[2].get("turnId").and_then(JsValue::as_str) == Some(turn_id)
    })
}

async fn wait_for_turn_started(feed: &Feed, turn_id: &str) {
    for _ in 0..2000 {
        if turn_started(feed, turn_id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{turn_id} never started");
}

fn scripted(spec: &Spec, names: &[&str]) {
    let turns = json(SCENARIO_TURNS);
    let mut queue = spec.turns.lock().expect("turns");
    for name in names {
        queue.push_back(turns.get(name).expect("turn").clone());
    }
}

/// Waits on the held turn whose signal is aborted before the wait, with a
/// string after it, with an `Error` after it, and with no reason.
async fn aborted_waits(manager: &AgentManager) -> Vec<JsValue> {
    let wait = |options: WaitForAgentOptions| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .wait_for_agent_event(AGENT_ID, options)
                    .await
                    .map(|wait| wait.to_js()),
            )
        }
    };
    let mut results = Vec::new();
    let pre = AbortController::default();
    pre.abort(AbortReason::Value(text("pre")));
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(pre.signal()),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    let stop = AbortController::default();
    let signal = stop.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.abort(AbortReason::Value(text("stop")));
    });
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(signal),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    let timeout = AbortController::default();
    let signal = timeout.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        timeout.abort(AbortReason::Error(AgentError::new("wait timeout")));
    });
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(signal),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    let plain = AbortController::default();
    plain.abort(AbortReason::Value(JsValue::Undefined));
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(plain.signal()),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    results
}

async fn turns_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("turns"));
    let fake = spec("fake");
    scripted(&fake, &["failed", "canceled", "coalesce", "held"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let run = |prompt: &'static str| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .run_agent(AGENT_ID, AgentPromptInput::Text(prompt.to_owned()), None)
                    .await
                    .map(|run| run.to_js()),
            )
        }
    };
    let wait = |options: WaitForAgentOptions| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .wait_for_agent_event(AGENT_ID, options)
                    .await
                    .map(|wait| wait.to_js()),
            )
        }
    };
    let mut results = vec![
        run("fail").await,
        wait(WaitForAgentOptions::default()).await,
        run("cancel").await,
        wait(WaitForAgentOptions::default()).await,
        run("coalesce").await,
        wait(WaitForAgentOptions {
            wait_for_active: true,
            ..WaitForAgentOptions::default()
        })
        .await,
    ];
    let held = tokio::spawn(run("hold"));
    wait_for_turn_started(&feed, "turn-5").await;
    results.extend(aborted_waits(&manager).await);
    results.push(run("again").await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    held.abort();
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
        (
            "stored",
            registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

async fn permission_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("permission"));
    let fake = spec("fake");
    scripted(&fake, &["permission"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let asking = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(AGENT_ID, AgentPromptInput::Text("ask".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-6").await;
    let mut results = Vec::new();
    for _ in 0..2 {
        results.push(outcome(
            manager
                .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
                .await
                .map(|wait| wait.to_js()),
        ));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    asking.abort();
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
    ])
}

async fn collect(stream: &mut TurnEventStream, events: &mut Vec<JsValue>) {
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }
}

async fn lifecycle_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("lifecycle"));
    let turns = json(SCENARIO_TURNS);
    let mut fake = spec("fake");
    scripted(&fake, &["ask", "long", "slowStart"]);
    fake.response = turns.get("response").cloned();
    fake.interrupt = turns.get("interrupt").cloned();
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let prompt = |text: &str| AgentPromptInput::Text(text.to_owned());
    let mut second = manager
        .stream_agent(AGENT_ID, prompt("remove x"), None)
        .expect("second stream");
    let mut second_events = vec![second.next().await.expect("first").expect("event")];
    let permission_wait = manager
        .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
        .await
        .expect("permission wait");
    let pending = manager.get_pending_permissions(AGENT_ID).expect("pending");
    let respond = manager
        .respond_to_permission(
            AGENT_ID,
            "perm-1",
            object(vec![("behavior", text("allow"))]),
        )
        .await
        .expect("respond");
    collect(&mut second, &mut second_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut third = manager
        .stream_agent(AGENT_ID, prompt("long task"), None)
        .expect("third stream");
    let mut third_events = vec![third.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cancel = manager.cancel_agent_run(AGENT_ID).await.expect("cancel");
    collect(&mut third, &mut third_events).await;
    let cancel_again = manager
        .cancel_agent_run(AGENT_ID)
        .await
        .expect("cancel again");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let starting = tokio::spawn({
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .run_agent(AGENT_ID, AgentPromptInput::Text("slow".to_owned()), None)
                    .await
                    .map(|run| run.to_js()),
            )
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancel_starting = manager
        .cancel_agent_run(AGENT_ID)
        .await
        .expect("cancel while starting");
    let starting_result = starting.await.expect("join");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rows = manager.get_timeline_rows(AGENT_ID).expect("rows");
    let fetch = manager
        .fetch_timeline(AGENT_ID, FetchDirection::Tail, None, Some(3))
        .expect("fetch")
        .to_js();
    manager.close_agent(AGENT_ID).await.expect("close");
    manager.close_agent(AGENT_ID).await.expect("close again");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("secondEvents", JsValue::Array(second_events)),
        ("permissionWait", permission_wait.to_js()),
        ("pending", JsValue::Array(pending)),
        ("respond", respond.unwrap_or(JsValue::Null)),
        ("thirdEvents", JsValue::Array(third_events)),
        ("cancel", text(cancel.as_str())),
        ("cancelAgain", text(cancel_again.as_str())),
        ("cancelStarting", text(cancel_starting.as_str())),
        ("startingResult", starting_result),
        ("rows", JsValue::Array(rows)),
        ("fetch", fetch),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        ("record", read_record(&home.join("lifecycle"))),
    ])
}

async fn subagents_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("subagents"));
    let fake = spec("fake");
    scripted(&fake, &["subagents"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let run = outcome(
        manager
            .run_agent(
                AGENT_ID,
                AgentPromptInput::Text("delegate".to_owned()),
                None,
            )
            .await
            .map(|run| run.to_js()),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let queries = vec![
        outcome(
            manager
                .list_provider_subagents(AGENT_ID)
                .map(JsValue::Array),
        ),
        outcome(Ok(JsValue::Array(
            manager.list_provider_subagent_activity(),
        ))),
        outcome(
            manager
                .get_provider_subagent(AGENT_ID, "child-1")
                .map(|subagent| subagent.unwrap_or(JsValue::Null)),
        ),
        outcome(
            manager
                .get_provider_subagent(AGENT_ID, "nope")
                .map(|subagent| subagent.unwrap_or(JsValue::Null)),
        ),
        outcome(
            manager
                .fetch_provider_subagent_timeline(
                    AGENT_ID,
                    "child-1",
                    FetchDirection::Tail,
                    None,
                    Some(5),
                )
                .map(|page| page.to_js()),
        ),
        outcome(
            manager
                .list_provider_subagents(UNKNOWN_ID)
                .map(JsValue::Array),
        ),
    ];
    manager.close_agent(AGENT_ID).await.expect("close");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("run", run),
        ("queries", JsValue::Array(queries)),
        (
            "after",
            JsValue::Array(manager.list_provider_subagent_activity()),
        ),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
    ])
}

async fn hydration_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("hydration"));
    let mut fake = spec("fake");
    fake.history = json(SCENARIO_TURNS).get("history").cloned();
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let mut steps = Vec::new();
    let options = [
        HydrateTimelineOptions::default(),
        HydrateTimelineOptions {
            force: true,
            broadcast: Some(HydrateBroadcast::Now(true)),
            broadcast_timeline: None,
        },
        HydrateTimelineOptions {
            force: true,
            broadcast: Some(HydrateBroadcast::Now(true)),
            broadcast_timeline: Some(false),
        },
        HydrateTimelineOptions {
            force: true,
            ..HydrateTimelineOptions::default()
        },
    ];
    for options in options {
        steps.push(outcome(
            manager
                .hydrate_timeline_from_provider(AGENT_ID, options)
                .await
                .map(|()| JsValue::Null),
        ));
        steps.push(JsValue::Array(
            manager.get_timeline_rows(AGENT_ID).expect("rows"),
        ));
    }
    steps.push(outcome(
        manager
            .hydrate_timeline_from_provider(UNKNOWN_ID, HydrateTimelineOptions::default())
            .await
            .map(|()| JsValue::Null),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("steps", JsValue::Array(steps)),
        (
            "subagents",
            JsValue::Array(manager.list_provider_subagents(AGENT_ID).expect("list")),
        ),
        ("feed", JsValue::Array(feed)),
    ])
}

/// The resume errors: an unregistered client, an unavailable provider,
/// and a malformed agent id.
/// An archived agent resumed for history: its working directory is gone.
async fn resume_archived(manager: &AgentManager, registry: &AgentStorage) -> JsValue {
    registry
        .upsert(object(vec![
            ("id", text(OTHER_ID)),
            ("provider", text("fake")),
            ("cwd", text("/nonexistent/spocky-archived")),
            ("archivedAt", text("2026-07-01T00:00:00.000Z")),
        ]))
        .await
        .expect("archived record");
    outcome(
        manager
            .resume_agent_from_persistence(
                object(vec![
                    ("provider", text("fake")),
                    ("sessionId", text("sess-a")),
                    (
                        "metadata",
                        object(vec![("cwd", text("/nonexistent/spocky-archived"))]),
                    ),
                ]),
                None,
                Some(OTHER_ID.to_owned()),
                ResumeAgentOptions::default(),
                None,
            )
            .await
            .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
    )
}

async fn resume_errors(manager: &AgentManager, handle: &JsValue, cwd: &str) -> Vec<JsValue> {
    let mut results = Vec::new();
    for (provider, agent_id) in [("nope", None), ("gone", None), ("fake", Some("not-a-uuid"))] {
        let handle = if provider == "fake" {
            handle.clone()
        } else {
            object(vec![
                ("provider", text(provider)),
                ("sessionId", text("x")),
                ("metadata", object(vec![("cwd", text(cwd))])),
            ])
        };
        results.push(outcome(
            manager
                .resume_agent_from_persistence(
                    handle,
                    None,
                    agent_id.map(str::to_owned),
                    ResumeAgentOptions::default(),
                    None,
                )
                .await
                .map(|agent| text(&agent.id)),
        ));
    }
    results
}

async fn resume_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("resume"));
    let mut fake = spec("fake");
    fake.history = json(SCENARIO_TURNS).get("history").cloned();
    fake.initial_timeline = Some(json(
        r#"[{"item":{"type":"assistant_message","text":"startup row"},"timestamp":"2026-07-12T07:00:00.000Z"}]"#,
    ));
    let mut gone = spec("gone");
    gone.available = Ok(false);
    let manager = manager_with(
        &calls,
        &registry,
        vec![(fake, enabled()), (gone, enabled())],
    );
    let feed = record_feed(&manager);
    let handle = object(vec![
        ("provider", text("fake")),
        ("sessionId", text("sess-r")),
        ("nativeHandle", text("thread-r")),
        (
            "metadata",
            object(vec![
                ("cwd", text(cwd)),
                ("model", text("model-a")),
                ("title", text("Stored")),
            ]),
        ),
    ]);
    let mut results = vec![outcome(
        manager
            .resume_agent_from_persistence(
                handle.clone(),
                Some(object(vec![("modeId", text("auto"))])),
                Some(AGENT_ID.to_owned()),
                ResumeAgentOptions {
                    created_at_millis: Some(1_700_000_000_000),
                    updated_at_millis: Some(1_700_000_005_000),
                    last_user_message_at_millis: Some(1_700_000_004_000),
                    labels: Some(object(vec![("surface", text("workspace"))])),
                    workspace_id: Some("wks_9".to_owned()),
                    owner: None,
                    attention: Some(AgentAttention::Required {
                        reason: "finished".to_owned(),
                        timestamp_millis: 1_700_000_006_000,
                    }),
                },
                Some(AgentResumeSessionOptions {
                    purpose: Some(AgentResumePurpose::Interactive),
                }),
            )
            .await
            .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
    )];
    results.push(JsValue::Array(
        manager.get_timeline_rows(AGENT_ID).expect("rows"),
    ));
    results.push(outcome(
        manager
            .hydrate_timeline_from_provider(AGENT_ID, HydrateTimelineOptions::default())
            .await
            .map(|()| JsValue::Null),
    ));
    results.push(resume_archived(&manager, &registry).await);
    results.extend(resume_errors(&manager, &handle, cwd).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "stored",
            registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

async fn titles_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("titles"));
    let fake = spec("fake");
    scripted(&fake, &["held"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let mut results = vec![
        JsValue::Bool(manager.has_in_flight_run(AGENT_ID)),
        JsValue::Bool(manager.has_in_flight_run(UNKNOWN_ID)),
    ];
    for (agent_id, title) in [
        (AGENT_ID, "  New title  "),
        (AGENT_ID, "   "),
        (UNKNOWN_ID, "x"),
    ] {
        results.push(outcome(
            manager
                .set_title(agent_id, title)
                .await
                .map(|()| JsValue::Null),
        ));
    }
    let held = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-5").await;
    results.push(JsValue::Bool(manager.has_in_flight_run(AGENT_ID)));
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    held.abort();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("feed", JsValue::Array(feed)),
        (
            "stored",
            registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

/// Replaces ISO timestamps with `<ISO>` and UUIDs other than
/// [`FIXED_IDS`] with `<UUID>`.
fn normalize(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    let digit = |offset: usize| bytes.get(offset).is_some_and(u8::is_ascii_digit);
    let hex = |offset: usize| bytes.get(offset).is_some_and(u8::is_ascii_hexdigit);
    while index < bytes.len() {
        let iso = (0..24).all(|offset| match offset {
            4 | 7 => bytes.get(index + offset) == Some(&b'-'),
            10 => bytes.get(index + offset) == Some(&b'T'),
            13 | 16 => bytes.get(index + offset) == Some(&b':'),
            19 => bytes.get(index + offset) == Some(&b'.'),
            23 => bytes.get(index + offset) == Some(&b'Z'),
            _ => digit(index + offset),
        });
        if iso {
            out.push_str("<ISO>");
            index += 24;
            continue;
        }
        let uuid = (0..36).all(|offset| match offset {
            8 | 13 | 18 | 23 => bytes.get(index + offset) == Some(&b'-'),
            _ => hex(index + offset),
        });
        if uuid {
            let id = &text[index..index + 36];
            out.push_str(if FIXED_IDS.contains(&id) {
                id
            } else {
                "<UUID>"
            });
            index += 36;
            continue;
        }
        let character = text[index..].chars().next().expect("character");
        out.push(character);
        index += character.len_utf8();
    }
    out
}

#[test]
fn normalize_keeps_fixed_ids() {
    let text = format!(
        r#"["2026-10-01T12:34:56.789Z","3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b","{AGENT_ID}","{UNKNOWN_ID}x"]"#
    );
    assert_eq!(
        normalize(&text),
        format!(r#"["<ISO>","<UUID>","{AGENT_ID}","{UNKNOWN_ID}x"]"#)
    );
}

/// The pinned dist modules this test runs, relative to `SPOCKY_PASEO_DIST`,
/// with their SHA-256.
const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/agent-manager.js",
        "09e1a170a75fc6b1f4eca29779feb7ada0c6d607bd33588f545e619174d7fa65",
    ),
    (
        "server/agent/agent-storage.js",
        "f1e3ccb1cf1e4caf75084627304450ddab8f93098291049819b312174f1e17ea",
    ),
    (
        "server/agent/agent-projections.js",
        "725258d3cf93e0de535d27fc245d776983303bc4c7c8874141ed9277516bb690",
    ),
    (
        "server/agent/agent-stream-coalescer.js",
        "391e756b43b6ebafda726effc38c033ab916395947a62e27297e983d94b25b99",
    ),
    (
        "server/agent/agent-timeline-store.js",
        "5473e829162d3256ab76b9b39d965158efbf5b4a29bb01e444626ac3a4bf52e3",
    ),
];

fn assert_pinned_modules(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    for (path, expected) in PINNED_MODULES {
        let bytes = std::fs::read(Path::new(dist).join(path)).expect("pinned module");
        let actual = Sha256::digest(&bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_eq!(&actual, expected, "{path} is not the pinned build");
    }
}

struct Home(std::path::PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(name: &str) -> Home {
    let path = std::env::temp_dir().join(format!("spocky-manager-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("disposable home");
    Home(path)
}

#[tokio::test]
async fn scenarios_match_pinned_manager() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: manager differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let workspace = home("cwd");
    let cwd = std::fs::canonicalize(&workspace.0)
        .expect("cwd")
        .to_string_lossy()
        .into_owned();
    let node_home = home("node-records");
    let rust_home = home("rust-records");
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .args([
            AGENT_ID,
            OTHER_ID,
            UNKNOWN_ID,
            TURN_EVENTS,
            SCENARIO_TURNS,
            ERROR_CASES,
            RUNTIME_INFO,
            PERSISTENCE,
            CAPABILITIES,
            MODES,
            CATALOG,
            &cwd,
        ])
        .arg(&node_home.0)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = normalize(&String::from_utf8_lossy(&output.stdout));
    let rust = object(vec![
        ("main", main_scenario(&cwd, &rust_home.0).await),
        ("errors", errors_scenario(&cwd, &rust_home.0).await),
        ("turns", turns_scenario(&cwd, &rust_home.0).await),
        ("permission", permission_scenario(&cwd, &rust_home.0).await),
        ("lifecycle", lifecycle_scenario(&cwd, &rust_home.0).await),
        ("subagents", subagents_scenario(&cwd, &rust_home.0).await),
        ("hydration", hydration_scenario(&cwd, &rust_home.0).await),
        ("resume", resume_scenario(&cwd, &rust_home.0).await),
        ("titles", titles_scenario(&cwd, &rust_home.0).await),
        ("runstart", runstart_scenario(&cwd, &rust_home.0).await),
        ("outofband", outofband_scenario(&cwd, &rust_home.0).await),
        ("shutdown", shutdown_scenario(&cwd, &rust_home.0).await),
        ("loading", loading_scenario(&cwd, &rust_home.0).await),
        ("replace", replace_scenario(&cwd, &rust_home.0).await),
        ("archive", archive_scenario(&cwd, &rust_home.0).await),
    ]);
    assert_eq!(normalize(&stringify(&rust)), expected);
}

async fn collect_stream(mut stream: TurnEventStream, events: &mut Vec<JsValue>) {
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
}

async fn replace_manager(
    name: &str,
    turns: &[&str],
    home: &Path,
    cwd: &str,
    configure: impl FnOnce(&mut Spec, &mut AgentManagerOptions),
) -> (AgentManager, AgentStorage, Calls, Feed) {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join(format!("replace-{name}")));
    let mut fake = spec("fake");
    scripted(&fake, turns);
    let mut options = AgentManagerOptions::default();
    configure(&mut fake, &mut options);
    let client = Arc::new(FakeClient {
        spec: fake,
        calls: Arc::clone(&calls),
    }) as Arc<dyn AgentClient>;
    options.clients = vec![("fake".to_owned(), client)];
    options.provider_definitions = vec![("fake".to_owned(), enabled())];
    options.registry = Some(registry.clone());
    let manager = AgentManager::new(options);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    (manager, registry, calls, feed)
}

async fn replace_finish(
    manager: &AgentManager,
    registry: &AgentStorage,
    calls: &Calls,
    feed: &Feed,
    mut extra: Vec<(&'static str, JsValue)>,
) -> JsValue {
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let agent = manager.get_agent(AGENT_ID).expect("agent");
    extra.push((
        "calls",
        JsValue::Array(calls.lock().expect("calls").clone()),
    ));
    extra.push(("feed", JsValue::Array(feed.lock().expect("feed").clone())));
    extra.push((
        "agent",
        to_agent_payload(&agent.payload_view(), None).expect("payload"),
    ));
    extra.push((
        "rows",
        JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
    ));
    object(extra)
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn replace_scenario(cwd: &str, home: &Path) -> JsValue {
    let scripted_turns = json(SCENARIO_TURNS);
    let interrupt = scripted_turns.get("interrupt").cloned();
    let text_prompt = |prompt: &str| AgentPromptInput::Text(prompt.to_owned());
    let to_array = |events: Vec<JsValue>| JsValue::Array(events);

    let (manager, registry, calls, feed) = replace_manager(
        "running",
        &["rpIdle", "long", "rpAfter"],
        home,
        cwd,
        |fake, _| fake.interrupt = interrupt,
    )
    .await;
    let mut idle_events = Vec::new();
    collect_stream(
        manager
            .replace_agent_run(AGENT_ID, text_prompt("idle prompt"), None)
            .await
            .expect("idle replace"),
        &mut idle_events,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut old = manager
        .stream_agent(AGENT_ID, text_prompt("long task"), None)
        .expect("old stream");
    let mut old_events = vec![old.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let replacement = manager
        .replace_agent_run(AGENT_ID, text_prompt("replace it"), None)
        .await
        .expect("replace");
    collect_stream(old, &mut old_events).await;
    let mut new_events = Vec::new();
    collect_stream(replacement, &mut new_events).await;
    let a = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("idleEvents", to_array(idle_events)),
            ("oldEvents", to_array(old_events)),
            ("newEvents", to_array(new_events)),
        ],
    )
    .await;

    let (manager, registry, calls, feed) = replace_manager(
        "finishing",
        &["rpFinishing", "rpAfterFinish"],
        home,
        cwd,
        |_, _| {},
    )
    .await;
    let mut first = manager
        .stream_agent(AGENT_ID, text_prompt("finishing"), None)
        .expect("first stream");
    let mut first_events = vec![first.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(30)).await;
    let started_at = std::time::Instant::now();
    let second = manager
        .replace_agent_run(AGENT_ID, text_prompt("while finishing"), None)
        .await
        .expect("replace");
    let waited = started_at.elapsed() >= Duration::from_millis(80);
    collect_stream(first, &mut first_events).await;
    let mut second_events = Vec::new();
    collect_stream(second, &mut second_events).await;
    let b = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("firstEvents", to_array(first_events)),
            ("secondEvents", to_array(second_events)),
            ("waited", JsValue::Bool(waited)),
        ],
    )
    .await;

    let (manager, registry, calls, feed) =
        replace_manager("refused", &["rpHeld"], home, cwd, |fake, options| {
            fake.interrupt_hang = true;
            options.rescue_interrupt_session_ms = Some(80);
        })
        .await;
    let mut held = manager
        .stream_agent(AGENT_ID, text_prompt("long task"), None)
        .expect("held stream");
    let held_events = vec![held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let failure = match manager
        .replace_agent_run(AGENT_ID, text_prompt("never"), None)
        .await
    {
        Ok(_) => outcome(Ok(JsValue::Null)),
        Err(error) => outcome(Err(error)),
    };
    let again = outcome(
        manager
            .stream_agent(AGENT_ID, text_prompt("again"), None)
            .map(|_| JsValue::Null),
    );
    let mut held_events = held_events;
    collect_stream(held, &mut held_events).await;
    let c = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("heldEvents", to_array(held_events)),
            ("failure", failure),
            ("again", again),
        ],
    )
    .await;

    let (manager, registry, calls, feed) = replace_manager(
        "starting",
        &["slowStart", "rpAfterFinish"],
        home,
        cwd,
        |_, _| {},
    )
    .await;
    let slow = manager
        .stream_agent(AGENT_ID, text_prompt("slow"), None)
        .expect("slow stream");
    let slow_done = tokio::spawn(async move {
        let mut events = Vec::new();
        collect_stream(slow, &mut events).await;
        events
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_slow = manager
        .replace_agent_run(AGENT_ID, text_prompt("while starting"), None)
        .await
        .expect("replace");
    let slow_events = slow_done.await.expect("slow events");
    let mut after_slow_events = Vec::new();
    collect_stream(after_slow, &mut after_slow_events).await;
    let d = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("slowEvents", to_array(slow_events)),
            ("afterSlowEvents", to_array(after_slow_events)),
        ],
    )
    .await;
    object(vec![("a", a), ("b", b), ("c", c), ("d", d)])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn archive_scenario(cwd: &str, home: &Path) -> JsValue {
    const PARENT_LABEL: &str = "paseo.parent-agent-id";
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("archive"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    let create =
        |id: &'static str, labels: Vec<(&'static str, JsValue)>, workspace: &'static str| {
            let manager = manager.clone();
            let cwd = cwd.to_owned();
            async move {
                manager
                    .create_agent(
                        object(vec![("provider", text("fake")), ("cwd", text(&cwd))]),
                        Some(id.to_owned()),
                        CreateAgentOptions {
                            labels: Some(object(labels)),
                            workspace_id: Some(workspace.to_owned()),
                            ..CreateAgentOptions::default()
                        },
                    )
                    .await
                    .expect("create");
            }
        };
    let parent = || vec![(PARENT_LABEL, text(AGENT_ID))];
    create(AGENT_ID, vec![], "wks_1").await;
    create(CHILD_SAME_ID, parent(), "wks_1").await;
    create(CHILD_OTHER_WORKSPACE_ID, parent(), "wks_2").await;
    let mut tab = parent();
    tab.push(("paseo.open-agent-tab.c1", text("true")));
    create(CHILD_TAB_ID, tab, "wks_1").await;
    create(CHILD_STORED_ID, parent(), "wks_1").await;
    manager.close_agent(CHILD_STORED_ID).await.expect("close");
    manager.flush().await;
    registry.flush().await;
    let mut results = vec![outcome(
        manager.archive_agent(AGENT_ID).await.map(JsValue::String),
    )];
    manager.flush().await;
    registry.flush().await;
    let names = [
        ("parent", AGENT_ID),
        ("same", CHILD_SAME_ID),
        ("other", CHILD_OTHER_WORKSPACE_ID),
        ("tab", CHILD_TAB_ID),
        ("stored", CHILD_STORED_ID),
    ];
    let stored_records = |registry: &AgentStorage| {
        let registry = registry.clone();
        async move {
            let mut out = JsObject::new();
            for (name, id) in names {
                out.insert(name, registry.get(id).await.unwrap_or(JsValue::Null));
            }
            JsValue::Object(out)
        }
    };
    let stored = stored_records(&registry).await;
    results.push(outcome(
        manager
            .unarchive_snapshot(
                AGENT_ID,
                Some(UnarchiveUpdates {
                    workspace_id: Some("wks_3".to_owned()),
                    labels: Some(json(r#"{"a":"b","gone":null}"#)),
                }),
            )
            .await
            .map(JsValue::Bool),
    ));
    results.push(outcome(
        manager
            .unarchive_snapshot(AGENT_ID, None)
            .await
            .map(JsValue::Bool),
    ));
    results.push(outcome(
        manager
            .unarchive_snapshot(UNKNOWN_ID, None)
            .await
            .map(JsValue::Bool),
    ));
    results.push(outcome(manager.detach_agent(CHILD_SAME_ID).await.map(
        |detached| {
            object(vec![
                (
                    "id",
                    detached
                        .record
                        .get("id")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "labels",
                    detached
                        .record
                        .get("labels")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("live", JsValue::Bool(detached.live)),
                (
                    "previousParentAgentId",
                    detached
                        .previous_parent_agent_id
                        .map_or(JsValue::Null, JsValue::String),
                ),
            ])
        },
    )));
    results.push(outcome(
        manager
            .clear_agent_attention(CHILD_SAME_ID)
            .await
            .map(|()| JsValue::Null),
    ));
    results.push(outcome(
        manager
            .clear_agent_attention(UNKNOWN_ID)
            .await
            .map(|()| JsValue::Null),
    ));
    manager.flush().await;
    registry.flush().await;
    let after_stored = stored_records(&registry).await;
    let by_handle_calls = Calls::default();
    let by_handle_registry = AgentStorage::new(home.join("archive-handle"));
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let mut failing = spec("fake");
    failing.archive_fails = true;
    let by_handle = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            Arc::new(FakeClient {
                spec: failing,
                calls: Arc::clone(&by_handle_calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(by_handle_registry.clone()),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    by_handle
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    by_handle.close_agent(AGENT_ID).await.expect("close");
    by_handle_registry.flush().await;
    let archived_record = outcome(
        by_handle
            .archive_snapshot(AGENT_ID, "2026-07-12T10:00:00.000Z".to_owned())
            .await,
    );
    let handle = by_handle_registry
        .get(AGENT_ID)
        .await
        .and_then(|record| record.get("persistence").cloned())
        .expect("handle");
    let unarchived = outcome(
        by_handle
            .unarchive_snapshot_by_handle(&handle)
            .await
            .map(|()| JsValue::Null),
    );
    by_handle_registry.flush().await;
    let by_handle_record = by_handle_registry
        .get(AGENT_ID)
        .await
        .unwrap_or(JsValue::Null);
    let by_handle_calls = by_handle_calls.lock().expect("calls").clone();
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("stored", stored),
        ("afterStored", after_stored),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "byHandle",
            object(vec![
                ("archivedRecord", archived_record),
                ("unarchived", unarchived),
                ("record", by_handle_record),
                ("calls", JsValue::Array(by_handle_calls)),
                (
                    "warns",
                    JsValue::Array(warns.lock().expect("warns").clone()),
                ),
            ]),
        ),
    ])
}

async fn loading_scenario(cwd: &str, home: &Path) -> JsValue {
    const GHOST_ID: &str = "00000000-0000-4000-8000-0000000000c3";
    const MISSING_ID: &str = "00000000-0000-4000-8000-0000000000d4";
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("loading"));
    let first = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    first
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("model", text("m1")),
            ]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![("surface", text("x"))])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    first.flush().await;
    registry.flush().await;
    let stored = registry.get(AGENT_ID).await.expect("stored");
    let variant = |id: &str, changes: Vec<(&str, JsValue)>| {
        let JsValue::Object(record) = &stored else {
            panic!("record");
        };
        let mut record = record.clone();
        record.insert("id", text(id));
        for (key, value) in changes {
            record.insert(key, value);
        }
        JsValue::Object(record)
    };
    registry
        .upsert(variant(
            OTHER_ID,
            vec![("persistence", JsValue::Null), ("title", text("No handle"))],
        ))
        .await
        .expect("upsert");
    registry
        .upsert(variant(GHOST_ID, vec![("provider", text("ghost"))]))
        .await
        .expect("upsert");
    let second = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&second);
    let deps = |broadcast_timeline: bool| EnsureAgentLoadedDeps {
        agent_manager: second.clone(),
        agent_storage: registry.clone(),
        valid_providers: None,
        broadcast_timeline,
    };
    let load = |id: &'static str, broadcast_timeline: bool| {
        let deps = deps(broadcast_timeline);
        async move {
            outcome(
                ensure_agent_loaded(id, &deps)
                    .await
                    .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
            )
        }
    };
    let (a, b) = tokio::join!(load(AGENT_ID, false), load(AGENT_ID, true));
    let mut results = vec![a, b];
    results.push(load(AGENT_ID, false).await);
    results.push(load(OTHER_ID, false).await);
    results.push(load(GHOST_ID, false).await);
    results.push(load(MISSING_ID, false).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    second.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
    ])
}

async fn shutdown_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("shutdown"));
    let mut fake = spec("fake");
    fake.create_delay = Some(Duration::from_millis(150));
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let order = Arc::new(Mutex::new(Vec::new()));
    let create = |id: &'static str, name: &'static str| {
        let manager = manager.clone();
        let order = Arc::clone(&order);
        let cwd = cwd.to_owned();
        tokio::spawn(async move {
            let line = match manager
                .create_agent(
                    object(vec![("provider", text("fake")), ("cwd", text(&cwd))]),
                    Some(id.to_owned()),
                    CreateAgentOptions::default(),
                )
                .await
            {
                Ok(_) => format!("created {name}"),
                Err(error) => format!("failed {name}: {}", error.message),
            };
            order.lock().expect("order").push(JsValue::String(line));
        })
    };
    let push = |line: &str| order.lock().expect("order").push(text(line));
    let first = create(AGENT_ID, "first");
    tokio::time::sleep(Duration::from_millis(20)).await;
    manager.flush().await;
    push("flush done");
    first.await.expect("first");
    let second = create(OTHER_ID, "second");
    tokio::time::sleep(Duration::from_millis(20)).await;
    manager.prepare_for_shutdown();
    manager.flush_for_shutdown().await;
    push("flushForShutdown done");
    second.await.expect("second");
    create(UNKNOWN_ID, "third").await.expect("third");
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let order = order.lock().expect("order").clone();
    object(vec![
        ("order", JsValue::Array(order)),
        ("calls", JsValue::Array(calls)),
    ])
}

async fn outofband_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("outofband"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let attempt = |id: &str, prompt: &str, client_message_id: Option<&str>| {
        let options = client_message_id.map(|id| AgentRunOptions {
            client_message_id: Some(id.to_owned()),
            ..AgentRunOptions::default()
        });
        outcome(
            manager
                .try_run_out_of_band(
                    id,
                    &AgentPromptInput::Text(prompt.to_owned()),
                    options.as_ref(),
                )
                .map(JsValue::Bool),
        )
    };
    let pause = Duration::from_millis(50);
    let mut results = vec![attempt(AGENT_ID, "hello", None)];
    results.push(attempt(AGENT_ID, "/goal pause", Some("client-oob")));
    tokio::time::sleep(pause).await;
    results.push(attempt(AGENT_ID, "/goal fail", Some("")));
    tokio::time::sleep(pause).await;
    results.push(attempt(UNKNOWN_ID, "/goal pause", None));
    tokio::time::sleep(pause).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
    ])
}

/// `runAgent` registers its pending run synchronously: polls the run once so
/// it is registered before the caller waits, then lets it finish in a task.
async fn start_run(
    manager: &AgentManager,
    prompt: &'static str,
) -> tokio::task::JoinHandle<JsValue> {
    let manager = manager.clone();
    let mut run = Box::pin(async move {
        match manager
            .run_agent(AGENT_ID, AgentPromptInput::Text(prompt.to_owned()), None)
            .await
        {
            Ok(run) => run
                .to_js()
                .get("finalText")
                .cloned()
                .unwrap_or(JsValue::Null),
            Err(error) => JsValue::String(error.message),
        }
    });
    match poll_once(&mut run).await {
        Some(done) => tokio::spawn(async move { done }),
        None => tokio::spawn(run),
    }
}

async fn runstart_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("runstart"));
    let fake = spec("fake");
    scripted(&fake, &["startSlowDone", "startFail", "startSlowAbort"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let start = |id: &'static str, signal: Option<AbortSignal>| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .wait_for_agent_run_start(id, signal)
                    .await
                    .map(|()| text("started")),
            )
        }
    };
    let mut results = vec![start(UNKNOWN_ID, None).await, start(AGENT_ID, None).await];
    let slow = start_run(&manager, "slow").await;
    results.push(start(AGENT_ID, None).await);
    results.push(outcome(Ok(slow.await.expect("slow run"))));
    let failed = start_run(&manager, "fail").await;
    results.push(start(AGENT_ID, None).await);
    results.push(outcome(Ok(failed.await.expect("failed run"))));
    let aborted = start_run(&manager, "abort").await;
    let stop = AbortController::default();
    let stop_signal = stop.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.abort(AbortReason::Value(text("stop")));
    });
    results.push(start(AGENT_ID, Some(stop_signal)).await);
    let pre = AbortController::default();
    pre.abort(AbortReason::Error(AgentError::new("pre")));
    results.push(start(AGENT_ID, Some(pre.signal())).await);
    results.push(start(AGENT_ID, None).await);
    results.push(outcome(Ok(aborted.await.expect("aborted run"))));
    results.push(start(AGENT_ID, None).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
    ])
}

/// Polls `future` once.
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|context| {
        Poll::Ready(match Pin::new(&mut *future).poll(context) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        })
    })
    .await
}

/// A wait on a busy agent holds one subscription until it settles, is
/// aborted, or is dropped by its caller; none of those leak it.
#[tokio::test]
async fn wait_releases_its_subscription() {
    let workspace = home("wait-cwd");
    let records = home("wait-records");
    let cwd = workspace.0.to_string_lossy().into_owned();
    let calls = Calls::default();
    let registry = AgentStorage::new(&records.0);
    let fake = spec("fake");
    scripted(&fake, &["held"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(&cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let held = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-5").await;
    let idle = manager.subscription_count();

    let mut dropped =
        Box::pin(manager.wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default()));
    assert!(poll_once(&mut dropped).await.is_none());
    assert_eq!(manager.subscription_count(), idle + 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut dropped)
            .await
            .is_err()
    );
    drop(dropped);
    assert_eq!(manager.subscription_count(), idle);

    let controller = AbortController::default();
    let mut aborted = Box::pin(manager.wait_for_agent_event(
        AGENT_ID,
        WaitForAgentOptions {
            signal: Some(controller.signal()),
            ..WaitForAgentOptions::default()
        },
    ));
    assert!(poll_once(&mut aborted).await.is_none());
    assert_eq!(manager.subscription_count(), idle + 1);
    controller.abort(AbortReason::Value(text("stop")));
    let error = aborted.await.expect_err("aborted");
    assert_eq!(
        (error.name.as_str(), error.message.as_str()),
        ("AbortError", "stop")
    );
    assert_eq!(manager.subscription_count(), idle);
    held.abort();
}
