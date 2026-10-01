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
//! A scripted `{"type":"__delay","ms":N}` entry pauses the fake's emission
//! and is never emitted; a leading `{"type":"__startDelay","ms":N}` holds
//! `startTurn` that long before it resolves.
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

use spocky_session::agent_manager::{
    AgentManager, AgentManagerEvent, AgentManagerOptions, CreateAgentOptions, ProviderDefinition,
    SubscribeOptions, TurnEventStream, WaitForAgentOptions,
};
use spocky_session::agent_projection::to_agent_payload;
use spocky_session::agent_sdk::{
    AbortController, AbortReason, AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError,
    AgentEventStream, AgentLaunchContext, AgentPromptInput, AgentResult, AgentRunOptions,
    AgentSession, AgentStreamEvent, BoxFuture, FetchCatalogOptions, ProviderRefreshContext,
    StreamCallback, Unsubscribe,
};
use spocky_session::agent_storage::AgentStorage;
use spocky_session::timeline::FetchDirection;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const AGENT_ID: &str = "00000000-0000-4000-8000-0000000000a1";
const OTHER_ID: &str = "00000000-0000-4000-8000-0000000000b2";
const UNKNOWN_ID: &str = "00000000-0000-4000-8000-0000000000ff";

/// The ids the scenarios choose; [`normalize`] keeps them.
const FIXED_IDS: [&str; 3] = [AGENT_ID, OTHER_ID, UNKNOWN_ID];

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
  "slowStart": [
    {"type":"__startDelay","ms":300},
    {"type":"turn_started","provider":"fake","turnId":"turn-9"}
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
const [dist, agentId, unknownId, turnEventsJson, scenarioTurnsJson, errorCasesJson, runtimeInfoJson, persistenceJson, capabilitiesJson, modesJson, catalogJson, cwd, home] = process.argv.slice(1);
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
  constructor(spec, calls) { this.provider = spec.provider; this.id = "sess-1"; this.capabilities = spec.capabilities; this.spec = spec; this.calls = calls; this.listeners = []; }
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
    this.emitLater(events, 20);
    if (events[0]?.type === "__startDelay") await sleep(events[0].ms);
    return { turnId: turnIdOf(events) };
  }
  async run() { throw new Error("unused"); }
  async *streamHistory() {}
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
    return new FakeSession(spec, calls);
  },
  async resumeSession() { throw new Error("unused"); },
  async fetchCatalog(options) { calls.push(["fetchCatalog", options]); return JSON.parse(catalogJson); },
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
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId) };
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

process.stdout.write(JSON.stringify({ main: await main(), errors: await errors(), turns: await turns(), permission: await permission(), lifecycle: await lifecycle() }));
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
}

fn spec(provider: &str) -> Spec {
    Spec {
        provider: provider.to_owned(),
        capabilities: json(CAPABILITIES),
        available: Ok(true),
        turns: Arc::new(Mutex::new(VecDeque::new())),
        response: None,
        interrupt: None,
    }
}

struct FakeSession {
    spec: Spec,
    listeners: Arc<Mutex<Vec<StreamCallback>>>,
    calls: Arc<Mutex<Vec<JsValue>>>,
}

struct EmptyHistory;

impl AgentEventStream for EmptyHistory {
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>> {
        Box::pin(async { None })
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

impl AgentSession for FakeSession {
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
        Box::new(EmptyHistory)
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
    fn describe_persistence(&self) -> Option<JsValue> {
        Some(json(PERSISTENCE))
    }
    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.record(vec![text("interrupt")]);
        if let Some(events) = self.spec.interrupt.clone() {
            self.emit_later(events, 10);
        }
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("close")]));
        Box::pin(async { Ok(()) })
    }
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
        let context = launch_context.map_or(JsValue::Null, |context| {
            let mut value = JsObject::new();
            if let Some(agent_id) = context.agent_id {
                value.insert("agentId", JsValue::String(agent_id));
            }
            if let Some(env) = context.env {
                value.insert("env", JsValue::Object(env));
            }
            JsValue::Object(value)
        });
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
        Box::pin(async move { Ok(Arc::new(session) as Arc<dyn AgentSession>) })
    }
    fn resume_session(
        &self,
        _handle: JsValue,
        _overrides: Option<JsValue>,
        _launch_context: Option<AgentLaunchContext>,
        _options: Option<spocky_session::agent_sdk::AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
    fn fetch_catalog(
        &self,
        options: FetchCatalogOptions,
        _context: Option<Arc<dyn ProviderRefreshContext>>,
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
        ]));
        Box::pin(async { Ok(json(CATALOG)) })
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
    ]);
    assert_eq!(normalize(&stringify(&rust)), expected);
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
