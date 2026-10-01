//! Differential check of what the agent manager hands a provider when it
//! creates an agent, configured as the daemon runs it on a fresh home: a
//! plugin lifecycle with no plugin loaded, no provider settings (so the
//! Paseo tool policy is unset), the agent MCP URL and token set, and an
//! empty daemon system prompt. A codex client (the pinned codex
//! capabilities) records `createSession(config, launchContext, options)`
//! for a full-access agent, an agent whose config carries keys outside the
//! schema in another order, an internal agent, which skips the plugin
//! parse, and two configs the parse rejects with zod's issue list (an
//! unknown key in the strict tool policy; a wrong type and a malformed MCP
//! server).
//!
//! Normalized: wall-clock ISO timestamps and random UUIDs, nothing else.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use spocky_session::agent_manager::{
    AgentManager, AgentManagerOptions, CreateAgentOptions, ProviderDefinition,
};
use spocky_session::agent_projection::to_agent_payload;
use spocky_session::agent_sdk::{
    AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError, AgentEventStream,
    AgentLaunchContext, AgentPromptInput, AgentResult, AgentRunOptions, AgentSession,
    AgentStreamEvent, BoxFuture, FetchCatalogOptions, ProviderRefreshContext, StreamCallback,
    Unsubscribe,
};
use spocky_session::agent_storage::AgentStorage;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const AGENT_ID: &str = "00000000-0000-4000-8000-0000000000c1";
const SECOND_ID: &str = "00000000-0000-4000-8000-0000000000c2";
const INTERNAL_ID: &str = "00000000-0000-4000-8000-0000000000c3";
const UNKNOWN_KEY_ID: &str = "00000000-0000-4000-8000-0000000000c4";
const WRONG_TYPE_ID: &str = "00000000-0000-4000-8000-0000000000c5";
const FIXED_IDS: [&str; 5] = [
    AGENT_ID,
    SECOND_ID,
    INTERNAL_ID,
    UNKNOWN_KEY_ID,
    WRONG_TYPE_ID,
];

/// `CODEX_APP_SERVER_CAPABILITIES` from the pinned codex provider.
const CODEX_CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":false,"supportsRewindBoth":false}"#;
const CATALOG: &str = r#"{"models":[{"provider":"codex","id":"gpt-5.1-codex","label":"GPT-5.1 Codex","isDefault":true}],"modes":[]}"#;
const PERSISTENCE: &str =
    r#"{"provider":"codex","sessionId":"thread-1","nativeHandle":"thread-1"}"#;
const MCP_BASE_URL: &str = "http://127.0.0.1:43210/mcp/agents";
const MCP_TOKEN: &str = "agent-mcp-token";

/// `[agentId, config, options]` for each `createAgent`; `$CWD` stands for
/// the disposable working directory.
const CREATES: &str = r#"[
  ["00000000-0000-4000-8000-0000000000c1",
   {"provider":"codex","cwd":"$CWD","modeId":"full-access"},
   {"workspaceId":"wks_1","labels":{"surface":"workspace"}}],
  ["00000000-0000-4000-8000-0000000000c2",
   {"title":"  Named  ","modeId":"full-access","extraneous":{"a":1},"cwd":"$CWD","provider":"codex","model":" default ","systemPrompt":"be brief","daemonAppendSystemPrompt":"ignored"},
   {"workspaceId":"wks_1","env":{"B":"2","A":"1"}}],
  ["00000000-0000-4000-8000-0000000000c3",
   {"provider":"codex","cwd":"$CWD","internal":true,"extraneous":true},
   {}],
  ["00000000-0000-4000-8000-0000000000c4",
   {"provider":"codex","cwd":"$CWD","toolPolicy":{"preapproved":[],"unknown":1}},
   {}],
  ["00000000-0000-4000-8000-0000000000c5",
   {"provider":"codex","cwd":"$CWD","modeId":5,"mcpServers":{"m":{"type":"stdio"}}},
   {"env":{"A":"1"}}]
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, createsJson, capabilitiesJson, catalogJson, persistenceJson, mcpBaseUrl, mcpToken, cwd, home] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { AgentManager } = await import(`${dist}/server/agent/agent-manager.js`);
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const { toAgentPayload } = await import(`${dist}/server/agent/agent-projections.js`);
const { validateBeforeRequest } = await import(`${dist}/server/plugins/lifecycle/index.js`);
const { resolvePaseoToolPolicy } = await import(`${dist}/server/agent/paseo-tool-policy.js`);
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const calls = [];
class CodexSession {
  constructor() { this.provider = "codex"; this.id = "thread-1"; this.capabilities = JSON.parse(capabilitiesJson); }
  subscribe() { return () => {}; }
  async startTurn() { throw new Error("unused"); }
  async run() { throw new Error("unused"); }
  async *streamHistory() {}
  async getRuntimeInfo() { return { provider: "codex", sessionId: "thread-1", model: null, modeId: "full-access" }; }
  async getAvailableModes() { return []; }
  async getCurrentMode() { return "full-access"; }
  async setMode() {}
  getPendingPermissions() { return []; }
  async respondToPermission() {}
  describePersistence() { return JSON.parse(persistenceJson); }
  async interrupt() {}
  async close() {}
}
const client = {
  provider: "codex",
  capabilities: JSON.parse(capabilitiesJson),
  async createSession(config, launchContext, options) {
    calls.push(["createSession", config, launchContext ?? null, options ?? null]);
    return new CodexSession();
  },
  async resumeSession() { throw new Error("unused"); },
  async fetchCatalog(options) { calls.push(["fetchCatalog", options]); return JSON.parse(catalogJson); },
  async isAvailable() { return true; },
};
// The daemon's PluginService with no plugin loaded: before-hooks only parse.
const pluginLifecycle = { before: async (name, request) => validateBeforeRequest(name, request), emit() {} };
const registry = new AgentStorage(home, logger);
const manager = new AgentManager({
  pluginLifecycle,
  clients: { codex: client },
  providerDefinitions: { codex: { enabled: true } },
  registry,
  appendSystemPrompt: "",
  mcpAuthToken: mcpToken,
  resolvePaseoToolPolicy: (provider) => resolvePaseoToolPolicy(provider, undefined),
  logger,
});
manager.setMcpBaseUrl(mcpBaseUrl);
const created = [];
for (const [agentId, config, options] of JSON.parse(createsJson.replaceAll("$CWD", cwd))) {
  try {
    created.push({ ok: toAgentPayload(await manager.createAgent(config, agentId, options)) });
  } catch (error) {
    created.push({ name: error.name, message: error.message });
  }
}
await manager.flush();
await registry.flush();
const records = [];
for (const [agentId] of JSON.parse(createsJson)) records.push(await registry.get(agentId));
process.stdout.write(JSON.stringify({ created, calls, records }));
"#;

fn json(text: &str) -> JsValue {
    parse(text).expect("fixture JSON")
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

type Calls = Arc<Mutex<Vec<JsValue>>>;

struct NoHistory;

impl AgentEventStream for NoHistory {
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>> {
        Box::pin(async { None })
    }
}

struct CodexSession;

impl AgentSession for CodexSession {
    fn provider(&self) -> String {
        "codex".to_owned()
    }
    fn id(&self) -> Option<String> {
        Some("thread-1".to_owned())
    }
    fn capabilities(&self) -> JsValue {
        json(CODEX_CAPABILITIES)
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
        _prompt: AgentPromptInput,
        _options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
    fn subscribe(&self, _callback: StreamCallback) -> Unsubscribe {
        Box::new(|| {})
    }
    fn stream_history(&self) -> Box<dyn AgentEventStream> {
        Box::new(NoHistory)
    }
    fn get_runtime_info(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async {
            Ok(json(
                r#"{"provider":"codex","sessionId":"thread-1","model":null,"modeId":"full-access"}"#,
            ))
        })
    }
    fn get_available_modes(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Ok(json("[]")) })
    }
    fn get_current_mode(&self) -> BoxFuture<'_, AgentResult<Option<String>>> {
        Box::pin(async { Ok(Some("full-access".to_owned())) })
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
        Box::pin(async { Ok(()) })
    }
}

struct CodexClient {
    calls: Calls,
}

impl AgentClient for CodexClient {
    fn provider(&self) -> String {
        "codex".to_owned()
    }
    fn capabilities(&self) -> JsValue {
        json(CODEX_CAPABILITIES)
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
        Box::pin(async { Ok(Arc::new(CodexSession) as Arc<dyn AgentSession>) })
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
        Box::pin(async { Ok(true) })
    }
}

async fn rust_output(cwd: &str, home: &Path) -> String {
    let calls = Calls::default();
    let registry = AgentStorage::new(home);
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "codex".to_owned(),
            Arc::new(CodexClient {
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![(
            "codex".to_owned(),
            ProviderDefinition {
                enabled: true,
                ..ProviderDefinition::default()
            },
        )],
        registry: Some(registry.clone()),
        append_system_prompt: Some(String::new()),
        mcp_auth_token: Some(MCP_TOKEN.to_owned()),
        // `resolvePaseoToolPolicy(provider, undefined)`: no provider
        // settings, so no policy.
        resolve_paseo_tool_policy: None,
        plugin_lifecycle: true,
        ..AgentManagerOptions::default()
    });
    manager.set_mcp_base_url(Some(MCP_BASE_URL.to_owned()));
    let mut created = Vec::new();
    for create in json(&CREATES.replace("$CWD", cwd))
        .as_array()
        .expect("creates")
    {
        let create = create.as_array().expect("create");
        let options = &create[2];
        let result = manager
            .create_agent(
                create[1].clone(),
                create[0].as_str().map(str::to_owned),
                CreateAgentOptions {
                    workspace_id: options
                        .get("workspaceId")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                    labels: options.get("labels").cloned(),
                    env: match options.get("env") {
                        Some(JsValue::Object(env)) => Some(env.clone()),
                        _ => None,
                    },
                    ..CreateAgentOptions::default()
                },
            )
            .await;
        let mut outcome = JsObject::new();
        match result {
            Ok(agent) => outcome.insert(
                "ok",
                to_agent_payload(&agent.payload_view(), None).expect("payload"),
            ),
            Err(error) => {
                outcome.insert("name", JsValue::String(error.name));
                outcome.insert("message", JsValue::String(error.message));
            }
        }
        created.push(JsValue::Object(outcome));
    }
    manager.flush().await;
    registry.flush().await;
    let mut records = Vec::new();
    for id in FIXED_IDS {
        records.push(registry.get(id).await.unwrap_or(JsValue::Null));
    }
    let mut out = JsObject::new();
    out.insert("created", JsValue::Array(created));
    out.insert(
        "calls",
        JsValue::Array(calls.lock().expect("calls").clone()),
    );
    out.insert("records", JsValue::Array(records));
    stringify(&JsValue::Object(out))
}

/// Replaces ISO timestamps with `<ISO>` and UUIDs other than the fixed
/// agent ids with `<UUID>`.
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
        r#"["2026-10-01T12:34:56.789Z","3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b","{AGENT_ID}"]"#
    );
    assert_eq!(
        normalize(&text),
        format!(r#"["<ISO>","<UUID>","{AGENT_ID}"]"#)
    );
}

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/agent-manager.js",
        "09e1a170a75fc6b1f4eca29779feb7ada0c6d607bd33588f545e619174d7fa65",
    ),
    (
        "server/plugins/lifecycle/index.js",
        "fff7a2629df50bfff0e7837e34adc13f4876e0760d233509da1f313c98039546",
    ),
    (
        "server/agent/paseo-tool-policy.js",
        "268996ed0aa37ece647d8e5726ef569408b6edb86df4cee4d2524749d70a444c",
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
    let path = std::env::temp_dir().join(format!("spocky-launch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("disposable home");
    Home(std::fs::canonicalize(&path).expect("canonical home"))
}

#[tokio::test]
async fn launch_inputs_match_pinned_manager() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: launch differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let workspace = home("cwd");
    let cwd = workspace.0.to_string_lossy().into_owned();
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
            CREATES,
            CODEX_CAPABILITIES,
            CATALOG,
            PERSISTENCE,
            MCP_BASE_URL,
            MCP_TOKEN,
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
    assert_eq!(
        normalize(&rust_output(&cwd, &rust_home.0).await),
        normalize(&String::from_utf8_lossy(&output.stdout))
    );
}
