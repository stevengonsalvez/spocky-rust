//! Differential check of the agent manager against the pinned build's
//! `AgentManager`: the same scripted fake provider drives `createAgent`,
//! `runAgent` and `waitForAgentEvent` on both, and the subscriber feed
//! (agent payloads and stream events), the provider calls, the timeline,
//! the run result and the stored record must match.
//!
//! Normalized: wall-clock ISO timestamps (`<ISO>`) and random UUIDs such as
//! timeline epochs (`<UUID>`), nothing else.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spocky_session::agent_manager::{
    AgentManager, AgentManagerEvent, AgentManagerOptions, CreateAgentOptions, ProviderDefinition,
    SubscribeOptions, WaitForAgentOptions,
};
use spocky_session::agent_projection::to_agent_payload;
use spocky_session::agent_sdk::{
    AbortSignal, AgentClient, AgentCreateSessionOptions, AgentEventStream, AgentLaunchContext,
    AgentPromptInput, AgentResult, AgentRunOptions, AgentSession, AgentStreamEvent, BoxFuture,
    FetchCatalogOptions, ProviderRefreshContext, StreamCallback, Unsubscribe,
};
use spocky_session::agent_storage::AgentStorage;
use spocky_session::timeline::FetchDirection;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const AGENT_ID: &str = "00000000-0000-4000-8000-0000000000a1";

/// What the fake session emits after `startTurn` resolves, per turn.
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

const RUNTIME_INFO: &str =
    r#"{"provider":"fake","sessionId":"sess-1","model":"model-default","modeId":"auto"}"#;
const PERSISTENCE: &str =
    r#"{"provider":"fake","sessionId":"sess-1","nativeHandle":"thread-1","metadata":{"x":1}}"#;
const CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true}"#;
const MODES: &str = r#"[{"id":"auto","label":"Auto"},{"id":"read-only","label":"Read only"}]"#;
const CATALOG: &str = r#"{"models":[{"provider":"fake","id":"model-a","label":"A"},{"provider":"fake","id":"model-default","label":"D","isDefault":true}],"modes":[]}"#;

const NODE_SCRIPT: &str = r#"
const [dist, agentId, turnEventsJson, runtimeInfoJson, persistenceJson, capabilitiesJson, modesJson, catalogJson, cwd, home] = process.argv.slice(1);
const { AgentManager } = await import(`${dist}/server/agent/agent-manager.js`);
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const { toAgentPayload } = await import(`${dist}/server/agent/agent-projections.js`);
const fs = await import("node:fs");
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const calls = [];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
class FakeSession {
  constructor() { this.provider = "fake"; this.id = "sess-1"; this.capabilities = JSON.parse(capabilitiesJson); this.listeners = []; }
  subscribe(callback) { this.listeners.push(callback); return () => {}; }
  async startTurn(prompt, options) {
    calls.push(["startTurn", prompt, options ?? null]);
    setTimeout(() => { for (const event of JSON.parse(turnEventsJson)) for (const l of this.listeners) l(event); }, 20);
    return { turnId: "turn-1" };
  }
  async run() { throw new Error("unused"); }
  async *streamHistory() {}
  async getRuntimeInfo() { return JSON.parse(runtimeInfoJson); }
  async getAvailableModes() { return JSON.parse(modesJson); }
  async getCurrentMode() { return "auto"; }
  async setMode() {}
  getPendingPermissions() { return []; }
  async respondToPermission() {}
  describePersistence() { return JSON.parse(persistenceJson); }
  async interrupt() {}
  async close() { calls.push(["close"]); }
}
const client = {
  provider: "fake",
  capabilities: JSON.parse(capabilitiesJson),
  async createSession(config, launchContext, options) {
    calls.push(["createSession", config, launchContext ?? null, options ?? null]);
    return new FakeSession();
  },
  async resumeSession() { throw new Error("unused"); },
  async fetchCatalog(options) { calls.push(["fetchCatalog", options]); return JSON.parse(catalogJson); },
  async isAvailable() { return true; },
};
const registry = new AgentStorage(home, logger);
const manager = new AgentManager({ logger, registry, clients: { fake: client }, providerDefinitions: { fake: { enabled: true } } });
const feed = [];
manager.subscribe((event) => {
  if (event.type === "agent_state") feed.push(["agent_state", toAgentPayload(event.agent)]);
  else if (event.type === "agent_stream") feed.push(["agent_stream", event.agentId, event.event, event.seq ?? null, event.epoch ?? null, event.timestamp ?? null]);
  else feed.push([event.type]);
});
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
const record = JSON.parse(fs.readFileSync(`${home}/${fs.readdirSync(home)[0]}/${agentId}.json`, "utf8"));
process.stdout.write(JSON.stringify({
  calls,
  created,
  run,
  wait: { status: wait.status, permission: wait.permission, lastMessage: wait.lastMessage },
  rows: await manager.getTimelineRows(agentId),
  fetch: manager.fetchTimeline(agentId, { direction: "tail", limit: 3 }),
  feed,
  record,
}));
"#;

fn json(text: &str) -> JsValue {
    parse(text).expect("fixture JSON")
}

struct FakeSession {
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

impl AgentSession for FakeSession {
    fn provider(&self) -> String {
        "fake".to_owned()
    }
    fn id(&self) -> Option<String> {
        Some("sess-1".to_owned())
    }
    fn capabilities(&self) -> JsValue {
        json(CAPABILITIES)
    }
    fn run(
        &self,
        _prompt: AgentPromptInput,
        _options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Err(spocky_session::agent_sdk::AgentError::new("unused")) })
    }
    fn start_turn(
        &self,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>> {
        let mut call = vec![
            JsValue::String("startTurn".to_owned()),
            prompt_value(&prompt),
        ];
        call.push(options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(id) = options.client_message_id {
                value.insert("clientMessageId", JsValue::String(id));
            }
            JsValue::Object(value)
        }));
        self.calls.lock().expect("calls").push(JsValue::Array(call));
        let listeners = Arc::clone(&self.listeners);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let callbacks = listeners.lock().expect("listeners").clone();
            for event in json(TURN_EVENTS).as_array().expect("events") {
                for callback in &callbacks {
                    callback(event.clone());
                }
            }
        });
        Box::pin(async { Ok("turn-1".to_owned()) })
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
        _request_id: &str,
        _response: JsValue,
    ) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        Box::pin(async { Ok(None) })
    }
    fn describe_persistence(&self) -> Option<JsValue> {
        Some(json(PERSISTENCE))
    }
    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![JsValue::String("close".to_owned())]));
        Box::pin(async { Ok(()) })
    }
}

struct FakeClient {
    calls: Arc<Mutex<Vec<JsValue>>>,
}

impl AgentClient for FakeClient {
    fn provider(&self) -> String {
        "fake".to_owned()
    }
    fn capabilities(&self) -> JsValue {
        json(CAPABILITIES)
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
            JsValue::String("createSession".to_owned()),
            config,
            context,
            options,
        ]));
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            Ok(Arc::new(FakeSession {
                listeners: Arc::new(Mutex::new(Vec::new())),
                calls,
            }) as Arc<dyn AgentSession>)
        })
    }
    fn resume_session(
        &self,
        _handle: JsValue,
        _overrides: Option<JsValue>,
        _launch_context: Option<AgentLaunchContext>,
        _options: Option<spocky_session::agent_sdk::AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(spocky_session::agent_sdk::AgentError::new("unused")) })
    }
    fn fetch_catalog(
        &self,
        options: FetchCatalogOptions,
        _context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        let mut value = JsObject::new();
        match options {
            FetchCatalogOptions::Workspace { cwd, force } => {
                value.insert("scope", JsValue::String("workspace".to_owned()));
                value.insert("cwd", JsValue::String(cwd));
                value.insert("force", JsValue::Bool(force));
            }
            FetchCatalogOptions::Global { force } => {
                value.insert("scope", JsValue::String("global".to_owned()));
                value.insert("force", JsValue::Bool(force));
            }
        }
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            JsValue::String("fetchCatalog".to_owned()),
            JsValue::Object(value),
        ]));
        Box::pin(async { Ok(json(CATALOG)) })
    }
    fn is_available(
        &self,
        _signal: Option<AbortSignal>,
        _options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        Box::pin(async { Ok(true) })
    }
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

#[allow(clippy::cast_precision_loss, reason = "sequence numbers are small")]
fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
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

fn fake_manager(calls: &Arc<Mutex<Vec<JsValue>>>, registry: &AgentStorage) -> AgentManager {
    AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            Arc::new(FakeClient {
                calls: Arc::clone(calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![(
            "fake".to_owned(),
            ProviderDefinition {
                enabled: true,
                ..ProviderDefinition::default()
            },
        )],
        registry: Some(registry.clone()),
        ..AgentManagerOptions::default()
    })
}

/// The `createAgent` config and options both sides use.
fn create_input(cwd: &str) -> (JsValue, CreateAgentOptions) {
    let mut env = JsObject::new();
    env.insert("EXTRA", text("1"));
    let mut config = JsObject::new();
    config.insert("provider", text("fake"));
    config.insert("cwd", text(cwd));
    config.insert("title", text("  Fake title  "));
    config.insert("model", text(" default "));
    let mut labels = JsObject::new();
    labels.insert("surface", text("workspace"));
    (
        JsValue::Object(config),
        CreateAgentOptions {
            workspace_id: Some("wks_1".to_owned()),
            labels: Some(JsValue::Object(labels)),
            env: Some(env),
            ..CreateAgentOptions::default()
        },
    )
}

async fn rust_output(cwd: &str, home: &std::path::Path) -> String {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let registry = AgentStorage::new(home);
    let manager = fake_manager(&calls, &registry);
    let feed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&feed);
    let _unsubscribe = manager
        .subscribe(
            Arc::new(move |event| sink.lock().expect("feed").push(feed_entry(event))),
            SubscribeOptions::default(),
        )
        .expect("subscribe");
    let (config, options) = create_input(cwd);
    let created = manager
        .create_agent(config, Some(AGENT_ID.to_owned()), options)
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
    let mut run_value = JsObject::new();
    run_value.insert("sessionId", text(&run.session_id));
    run_value.insert("finalText", text(&run.final_text));
    if let Some(usage) = run.usage {
        run_value.insert("usage", usage);
    }
    run_value.insert("timeline", JsValue::Array(run.timeline));
    run_value.insert("canceled", JsValue::Bool(run.canceled));
    let mut wait_value = JsObject::new();
    wait_value.insert("status", text(wait.status.as_str()));
    wait_value.insert("permission", wait.permission.unwrap_or(JsValue::Null));
    wait_value.insert(
        "lastMessage",
        wait.last_message.as_deref().map_or(JsValue::Null, text),
    );
    let record_dir = std::fs::read_dir(home)
        .expect("home")
        .next()
        .expect("record directory")
        .expect("entry")
        .path();
    let record = parse(
        &std::fs::read_to_string(record_dir.join(format!("{AGENT_ID}.json"))).expect("record"),
    )
    .expect("record JSON");
    let mut output = JsObject::new();
    output.insert(
        "calls",
        JsValue::Array(calls.lock().expect("calls").clone()),
    );
    output.insert("created", created);
    output.insert("run", JsValue::Object(run_value));
    output.insert("wait", JsValue::Object(wait_value));
    output.insert(
        "rows",
        JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
    );
    output.insert(
        "fetch",
        manager
            .fetch_timeline(AGENT_ID, FetchDirection::Tail, None, Some(3))
            .expect("fetch")
            .to_js(),
    );
    output.insert("feed", JsValue::Array(feed.lock().expect("feed").clone()));
    output.insert("record", record);
    stringify(&JsValue::Object(output))
}

/// Replaces ISO timestamps with `<ISO>` and UUIDs with `<UUID>`.
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
            out.push_str("<UUID>");
            index += 36;
            continue;
        }
        let character = text[index..].chars().next().expect("character");
        out.push(character);
        index += character.len_utf8();
    }
    out
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
        let bytes = std::fs::read(std::path::Path::new(dist).join(path)).expect("pinned module");
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
async fn create_and_run_match_pinned_manager() {
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
            TURN_EVENTS,
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
    let actual = normalize(&rust_output(&cwd, &rust_home.0).await);
    assert_eq!(actual, expected);
}
