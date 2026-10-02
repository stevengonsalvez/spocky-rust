//! Differential check of `ProviderSnapshotManager` against the pinned
//! build's `agent/provider-snapshot-manager.js`, with scripted fake clients
//! for `codex` and `claude` (node's `extraClients`; the other builtin
//! providers are disabled so nothing real is spawned).
//!
//! Each step records the fake clients' calls (`getCatalogCacheKey`,
//! `isAvailable`, `fetchCatalog`, with their options) in call order, the
//! `change` transitions, and the step's result or error. The provider
//! definitions (labels, icons, default and static modes) come from node's
//! registry, which this port takes as input.
//!
//! Normalized: the wall-clock `fetchedAt` of a ready entry, nothing else.
//!
//! Every step loads at most one provider at a time: how two providers'
//! concurrent loads interleave follows microtask depth in node and task
//! scheduling in Rust (see the module docs), so it is not compared.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use spocky_session::agent_sdk::{
    AbortReason, AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError,
    AgentLaunchContext, AgentResult, AgentResumeSessionOptions, AgentSession, BoxFuture,
    FetchCatalogOptions, ProviderRefreshContext,
};
use spocky_session::provider_snapshot_manager::{
    CreateConfigParentAgent, ProviderSnapshotManager, ProviderSnapshotManagerOptions,
    ResolveProviderCreateConfigOptions, SnapshotProviderDefinition,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const CATALOG: &str = r#"{"models":[{"id":"m1","label":"M1","isDefault":true},{"id":"m1","label":"dup"},{"id":"m2","label":"M2","isSelectable":false}],"modes":[{"id":"auto","label":"Auto"},{"id":"yolo","label":"Yolo","isUnattended":true}],"defaultModeId":"auto"}"#;

const STEPS: &str = r#"[
  {"op":"ids"},
  {"op":"getProvider","provider":"codex","cwd":"/w1"},
  {"op":"getProvider","provider":"codex","cwd":"/w1"},
  {"op":"getProvider","provider":"codex","cwd":"/w2"},
  {"op":"getProvider","provider":"claude","cwd":"/w1"},
  {"op":"getProvider","provider":"claude","cwd":"/w2"},
  {"op":"concurrent","provider":"codex","cwd":"/w3","count":3},
  {"op":"push","provider":"codex","method":"isAvailable","items":[{"value":false}]},
  {"op":"resolveCreate","provider":"codex","cwd":"/w4"},
  {"op":"push","provider":"codex","method":"fetchCatalog","items":[{"throw":"catalog exploded"}]},
  {"op":"resolveCreate","provider":"codex","cwd":"/w5"},
  {"op":"push","provider":"codex","method":"isAvailable","items":[{"hang":true}]},
  {"op":"getProvider","provider":"codex","cwd":"/w6"},
  {"op":"push","provider":"codex","method":"fetchCatalog","items":[{"hang":true}]},
  {"op":"getProvider","provider":"codex","cwd":"/w7"},
  {"op":"push","provider":"claude","method":"getCatalogCacheKey","items":[{"throw":"no account"}]},
  {"op":"getProvider","provider":"claude","cwd":"/w8"},
  {"op":"refresh","cwd":"/w1","providers":["codex"]},
  {"op":"resolveCreate","provider":"codex","cwd":"/w1","requestedMode":"auto"},
  {"op":"resolveCreate","provider":"codex","cwd":"/w1","requestedMode":"nope"},
  {"op":"resolveCreate","provider":"codex","cwd":"/w1","unattended":true},
  {"op":"resolveCreate","provider":"codex","cwd":"/w1","parent":{"provider":"claude","currentModeId":"bypassPermissions","config":{"provider":"claude","cwd":"/w1"}}},
  {"op":"resolveCreate","provider":"codex","cwd":"/w1","parent":{"provider":"claude","currentModeId":"default","config":{"provider":"claude","cwd":"/w1"},"availableModes":[{"id":"default","label":"Default"}]}},
  {"op":"resolveCreate","provider":"codex","cwd":"/w1","parent":{"provider":"codex","currentModeId":"auto","config":{"provider":"codex","cwd":"/w1"}}},
  {"op":"models","provider":"codex","cwd":"/w1"},
  {"op":"defaultModel","provider":"codex","cwd":"/w1"},
  {"op":"defaultModel","provider":"codex","cwd":"/w1","requestedModel":"  custom  "},
  {"op":"resolveCreate","provider":"nobody","cwd":"/w1"},
  {"op":"resolveCreate","provider":"copilot","cwd":"/w1"},
  {"op":"push","provider":"claude","method":"getCatalogCacheKey","items":[{"value":"acct-9"},{"value":"acct-9"}]},
  {"op":"concurrentTargets","provider":"claude","cwds":["/w10","/w11"]},
  {"op":"refreshAndRead","provider":"claude","cwd":"/w10"},
  {"op":"reset"},
  {"op":"getProvider","provider":"codex","cwd":"/w1"},
  {"op":"getProvider","provider":"claude","cwd":"/w1"},
  {"op":"refreshSettings","providers":["codex"]},
  {"op":"list","cwd":"/w1"},
  {"op":"snapshot","cwd":"/w1"},
  {"op":"settle"},
  {"op":"snapshot","cwd":"/w1"},
  {"op":"push","provider":"claude","method":"getCatalogCacheKey","items":[{"value":"acct-2"}]},
  {"op":"snapshot","cwd":"/w1"},
  {"op":"settle"},
  {"op":"snapshot","cwd":"/w1"}
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, protocolDist, stepsJson, catalogJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { ProviderSnapshotManager } = await import(`${dist}/server/agent/provider-snapshot-manager.js`);
const { BUILTIN_PROVIDER_IDS } = await import(`${protocolDist}/provider-manifest.js`);
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const log = [];
const queues = {};
const next = (provider, method) => (queues[`${provider}.${method}`] ?? []).shift();
const respond = async (item, fallback, signal) => {
  if (!item) return fallback();
  if (item.throw) throw new Error(item.throw);
  if (item.hang) {
    if (!signal) return new Promise(() => {});
    return new Promise((_resolve, reject) => {
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    });
  }
  return item.value;
};
const fake = (provider, keyed) => {
  const client = {
    provider,
    capabilities: {},
    async createSession() { throw new Error("unused"); },
    async resumeSession() { throw new Error("unused"); },
    isAvailable(signal, options) {
      log.push(`${provider} isAvailable ${JSON.stringify(options)} ${signal ? "signal" : "no signal"}`);
      return respond(next(provider, "isAvailable"), () => true);
    },
    fetchCatalog(options, context) {
      log.push(`${provider} fetchCatalog ${JSON.stringify(options)} ${context?.signal ? "signal" : "no signal"}`);
      return respond(next(provider, "fetchCatalog"), () => JSON.parse(catalogJson), context?.signal);
    },
  };
  if (keyed) {
    client.getCatalogCacheKey = (options) => {
      log.push(`${provider} getCatalogCacheKey ${JSON.stringify(options)}`);
      return respond(next(provider, "getCatalogCacheKey"), () => "acct-1");
    };
  }
  return client;
};
const overrides = Object.fromEntries(
  BUILTIN_PROVIDER_IDS.filter((id) => id !== "codex" && id !== "claude").map((id) => [id, { enabled: false }]),
);
const createManager = () => {
  const created = new ProviderSnapshotManager({
    logger,
    extraClients: { codex: fake("codex", false), claude: fake("claude", true) },
    providerOverrides: overrides,
    refreshTimeoutMs: 200,
  });
  created.on("change", ({ current }) => {
    log.push(`change ${current.cwd} ${current.records.map(({ entry }) => `${entry.provider}:${entry.status}`).join(",")}`);
  });
  return created;
};
let manager = createManager();
const defs = manager.listRegisteredProviderIds().map((provider) => {
  const definition = manager.generation.definitions[provider];
  const initial = manager.generation.providerStates.get(provider).initial.entry;
  return { provider, enabled: definition.enabled, source: initial.source, label: definition.label,
    description: definition.description ?? null, iconSvg: definition.iconSvg ?? null,
    defaultModeId: definition.defaultModeId ?? null, modes: definition.modes ?? null };
});
const run = async (step) => {
  switch (step.op) {
    case "ids": return manager.listRegisteredProviderIds();
    case "getProvider": return await manager.getProvider({ provider: step.provider, cwd: step.cwd, wait: true });
    case "concurrent": return await Promise.all(Array.from({ length: step.count },
      () => manager.getProvider({ provider: step.provider, cwd: step.cwd, wait: true })));
    case "concurrentTargets": return await Promise.all(step.cwds.map(
      (cwd) => manager.getProvider({ provider: step.provider, cwd, wait: true })));
    case "refreshAndRead": return (await Promise.all([
      manager.refreshSnapshotForCwd({ cwd: step.cwd, providers: [step.provider] }),
      manager.getProvider({ provider: step.provider, cwd: step.cwd, wait: true }),
    ]))[1];
    case "push": (queues[`${step.provider}.${step.method}`] ??= []).push(...step.items); return null;
    case "resolveCreate": return await manager.resolveCreateConfig({ cwd: step.cwd, provider: step.provider,
      requestedMode: step.requestedMode, featureValues: { f: 1 }, parent: step.parent ?? null, unattended: step.unattended ?? false });
    case "refresh": await manager.refreshSnapshotForCwd({ cwd: step.cwd, providers: step.providers }); return null;
    case "refreshSettings": await manager.refreshSettingsSnapshot({ providers: step.providers }); return null;
    case "models": return await manager.listModels({ provider: step.provider, cwd: step.cwd, wait: true });
    case "defaultModel": return (await manager.resolveDefaultModel({ provider: step.provider, cwd: step.cwd, requestedModel: step.requestedModel })) ?? null;
    case "list": return await manager.listProviders({ cwd: step.cwd, wait: true });
    case "snapshot": return manager.getSnapshot(step.cwd);
    case "settle": await new Promise((resolve) => setTimeout(resolve, 100)); return null;
    case "reset": manager.destroy(); manager = createManager(); return null;
  }
  throw new Error(`unknown step ${step.op}`);
};
const steps = [];
for (const step of JSON.parse(stepsJson)) {
  let row;
  try {
    row = { op: step.op, result: (await run(step)) ?? null };
  } catch (error) {
    row = { op: step.op, error: error.message };
  }
  row.log = log.splice(0);
  steps.push(row);
}
process.stdout.write(JSON.stringify({ defs, steps }));
"#;

type Log = Arc<Mutex<Vec<String>>>;
type Queues = Arc<Mutex<HashMap<String, Vec<JsValue>>>>;

fn options_json(options: &FetchCatalogOptions) -> String {
    match options {
        FetchCatalogOptions::Global { force } => format!(r#"{{"scope":"global","force":{force}}}"#),
        FetchCatalogOptions::Workspace { cwd, force } => format!(
            r#"{{"scope":"workspace","cwd":{},"force":{force}}}"#,
            stringify(&JsValue::String(cwd.clone()))
        ),
    }
}

struct Fake {
    provider: String,
    keyed: bool,
    log: Log,
    queues: Queues,
}

impl Fake {
    fn next(&self, method: &str) -> Option<JsValue> {
        let mut queues = self.queues.lock().expect("queues");
        let queue = queues
            .entry(format!("{}.{method}", self.provider))
            .or_default();
        (!queue.is_empty()).then(|| queue.remove(0))
    }

    fn push(&self, line: String) {
        self.log.lock().expect("log").push(line);
    }
}

fn abort_error(signal: &AbortSignal) -> AgentError {
    match signal.reason() {
        Some(AbortReason::Error(error)) => error.clone(),
        _ => AgentError::new("aborted"),
    }
}

async fn respond(
    item: Option<JsValue>,
    fallback: JsValue,
    signal: Option<AbortSignal>,
) -> AgentResult<JsValue> {
    let Some(item) = item else {
        return Ok(fallback);
    };
    if let Some(message) = item.get("throw").and_then(JsValue::as_str) {
        return Err(AgentError::new(message));
    }
    if item.get("hang").is_some() {
        let Some(signal) = signal else {
            return std::future::pending().await;
        };
        signal.wait().await;
        return Err(abort_error(&signal));
    }
    Ok(item.get("value").cloned().expect("value"))
}

impl AgentClient for Fake {
    fn provider(&self) -> String {
        self.provider.clone()
    }
    fn capabilities(&self) -> JsValue {
        JsValue::Object(JsObject::new())
    }
    fn create_session(
        &self,
        _config: JsValue,
        _launch_context: Option<AgentLaunchContext>,
        _options: Option<AgentCreateSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
    fn resume_session(
        &self,
        _handle: JsValue,
        _overrides: Option<JsValue>,
        _launch_context: Option<AgentLaunchContext>,
        _options: Option<AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
    fn get_catalog_cache_key(
        &self,
        options: &FetchCatalogOptions,
    ) -> Option<BoxFuture<'static, AgentResult<Option<String>>>> {
        if !self.keyed {
            return None;
        }
        self.push(format!(
            "{} getCatalogCacheKey {}",
            self.provider,
            options_json(options)
        ));
        let item = self.next("getCatalogCacheKey");
        Some(Box::pin(async move {
            let key = respond(item, JsValue::String("acct-1".to_owned()), None).await?;
            Ok(key.as_str().map(str::to_owned))
        }))
    }
    fn fetch_catalog(
        &self,
        options: FetchCatalogOptions,
        context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        let signal = context.map(|context| context.signal().clone());
        self.push(format!(
            "{} fetchCatalog {} {}",
            self.provider,
            options_json(&options),
            if signal.is_some() {
                "signal"
            } else {
                "no signal"
            }
        ));
        let item = self.next("fetchCatalog");
        Box::pin(async move { respond(item, parse(CATALOG).expect("catalog"), signal).await })
    }
    fn is_available(
        &self,
        signal: Option<AbortSignal>,
        options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        let options = options
            .as_ref()
            .map_or_else(|| "undefined".to_owned(), options_json);
        self.push(format!(
            "{} isAvailable {options} {}",
            self.provider,
            if signal.is_some() {
                "signal"
            } else {
                "no signal"
            }
        ));
        let item = self.next("isAvailable");
        Box::pin(async move {
            Ok(matches!(
                respond(item, JsValue::Bool(true), None).await?,
                JsValue::Bool(true)
            ))
        })
    }
}

fn text(value: Option<&JsValue>) -> Option<String> {
    value.and_then(JsValue::as_str).map(str::to_owned)
}

fn present(value: Option<&JsValue>) -> Option<JsValue> {
    value
        .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
        .cloned()
}

fn definitions(defs: &JsValue, log: &Log, queues: &Queues) -> Vec<SnapshotProviderDefinition> {
    defs.as_array()
        .expect("defs")
        .iter()
        .map(|def| {
            let provider = text(def.get("provider")).expect("provider");
            SnapshotProviderDefinition {
                enabled: matches!(def.get("enabled"), Some(JsValue::Bool(true))),
                custom: text(def.get("source")).as_deref() == Some("custom"),
                label: text(def.get("label")).expect("label"),
                description: text(def.get("description")),
                icon_svg: text(def.get("iconSvg")),
                default_mode_id: text(def.get("defaultModeId")),
                modes: present(def.get("modes")),
                client: Arc::new(Fake {
                    keyed: provider == "claude",
                    provider: provider.clone(),
                    log: Arc::clone(log),
                    queues: Arc::clone(queues),
                }),
                provider,
                fetch_catalog: None,
                resolve_create_config: None,
                is_create_config_unattended: None,
            }
        })
        .collect()
}

fn snapshot_value(
    snapshot: &spocky_session::provider_snapshot_manager::ProviderSnapshot,
) -> JsValue {
    let mut out = JsObject::new();
    out.insert("cwd", JsValue::String(snapshot.cwd.clone()));
    out.insert(
        "records",
        JsValue::Array(
            snapshot
                .records
                .iter()
                .map(|record| {
                    let mut row = JsObject::new();
                    row.insert("entry", record.entry.clone());
                    row.insert("contentHash", JsValue::String(record.content_hash.clone()));
                    JsValue::Object(row)
                })
                .collect(),
        ),
    );
    JsValue::Object(out)
}

fn create_result(result: &spocky_session::agent_sdk::ResolveAgentCreateConfigResult) -> JsValue {
    let mut out = JsObject::new();
    out.insert(
        "modeId",
        result
            .mode_id
            .clone()
            .map_or(JsValue::Undefined, JsValue::String),
    );
    out.insert(
        "featureValues",
        result.feature_values.clone().unwrap_or(JsValue::Undefined),
    );
    JsValue::Object(out)
}

#[allow(clippy::too_many_lines)]
async fn run_step(
    manager: &ProviderSnapshotManager,
    queues: &Queues,
    step: &JsValue,
) -> Result<JsValue, AgentError> {
    let cwd = text(step.get("cwd"));
    let cwd = cwd.as_deref();
    let provider = text(step.get("provider")).unwrap_or_default();
    let providers: Option<Vec<String>> =
        step.get("providers")
            .and_then(JsValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            });
    Ok(match text(step.get("op")).as_deref().expect("op") {
        "ids" => JsValue::Array(
            manager
                .list_registered_provider_ids()
                .into_iter()
                .map(JsValue::String)
                .collect(),
        ),
        "getProvider" => manager.get_provider(cwd, &provider, true).await?,
        "concurrent" => {
            let count = step.get("count").and_then(JsValue::as_f64).expect("count");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let handles: Vec<_> = (0..count as usize)
                .map(|_| {
                    let manager = manager.clone();
                    let cwd = cwd.map(str::to_owned);
                    let provider = provider.clone();
                    tokio::spawn(async move {
                        manager.get_provider(cwd.as_deref(), &provider, true).await
                    })
                })
                .collect();
            let mut out = Vec::new();
            for handle in handles {
                out.push(handle.await.expect("join")?);
            }
            JsValue::Array(out)
        }
        "concurrentTargets" => {
            let cwds: Vec<String> = step
                .get("cwds")
                .and_then(JsValue::as_array)
                .expect("cwds")
                .iter()
                .filter_map(|cwd| cwd.as_str().map(str::to_owned))
                .collect();
            let (first, second) = tokio::join!(
                manager.get_provider(Some(&cwds[0]), &provider, true),
                manager.get_provider(Some(&cwds[1]), &provider, true),
            );
            JsValue::Array(vec![first?, second?])
        }
        "refreshAndRead" => {
            let only = [provider.clone()];
            let cwd = cwd.expect("cwd");
            let ((), read) = tokio::join!(
                manager.refresh_snapshot_for_cwd(cwd, Some(&only)),
                manager.get_provider(Some(cwd), &provider, true),
            );
            read?
        }
        "push" => {
            let method = text(step.get("method")).expect("method");
            queues
                .lock()
                .expect("queues")
                .entry(format!("{provider}.{method}"))
                .or_default()
                .extend(
                    step.get("items")
                        .and_then(JsValue::as_array)
                        .expect("items")
                        .iter()
                        .cloned(),
                );
            JsValue::Null
        }
        "resolveCreate" => {
            let parent = step.get("parent").map(|parent| CreateConfigParentAgent {
                provider: text(parent.get("provider")).expect("provider"),
                current_mode_id: text(parent.get("currentModeId")),
                config: parent.get("config").cloned().expect("config"),
                features: present(parent.get("features")),
                available_modes: present(parent.get("availableModes")),
            });
            let result = manager
                .resolve_create_config(ResolveProviderCreateConfigOptions {
                    cwd: cwd.map(str::to_owned),
                    provider,
                    requested_mode: text(step.get("requestedMode")),
                    feature_values: Some(parse(r#"{"f":1}"#).expect("features")),
                    parent,
                    unattended: matches!(step.get("unattended"), Some(JsValue::Bool(true))),
                })
                .await?;
            create_result(&result)
        }
        "refresh" => {
            manager
                .refresh_snapshot_for_cwd(cwd.expect("cwd"), providers.as_deref())
                .await;
            JsValue::Null
        }
        "refreshSettings" => {
            manager
                .refresh_settings_snapshot(providers.as_deref())
                .await;
            JsValue::Null
        }
        "models" => JsValue::Array(manager.list_models(cwd, &provider, true).await?),
        "defaultModel" => manager
            .resolve_default_model(&provider, text(step.get("requestedModel")).as_deref(), cwd)
            .await
            .map_or(JsValue::Null, JsValue::String),
        "list" => JsValue::Array(manager.list_providers(cwd, None, true).await),
        "snapshot" => snapshot_value(&manager.get_snapshot(cwd)),
        "settle" => {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            JsValue::Null
        }
        other => panic!("unknown step {other}"),
    })
}

async fn rust_steps(defs: &JsValue) -> JsValue {
    let log: Log = Arc::default();
    let queues: Queues = Arc::default();
    let mut manager = create_manager(defs, &log, &queues);
    let mut rows = Vec::new();
    for step in parse(STEPS).expect("steps").as_array().expect("steps") {
        let mut row = JsObject::new();
        row.insert("op", step.get("op").cloned().expect("op"));
        if step.get("op").and_then(JsValue::as_str) == Some("reset") {
            manager.destroy();
            manager = create_manager(defs, &log, &queues);
            row.insert("result", JsValue::Null);
        } else {
            match run_step(&manager, &queues, step).await {
                Ok(result) => row.insert("result", result),
                Err(error) => row.insert("error", JsValue::String(error.message)),
            }
        }
        let lines: Vec<String> = log.lock().expect("log").drain(..).collect();
        row.insert(
            "log",
            JsValue::Array(lines.into_iter().map(JsValue::String).collect()),
        );
        rows.push(JsValue::Object(row));
    }
    JsValue::Array(rows)
}

fn create_manager(defs: &JsValue, log: &Log, queues: &Queues) -> ProviderSnapshotManager {
    let manager = ProviderSnapshotManager::new(ProviderSnapshotManagerOptions {
        definitions: definitions(defs, log, queues),
        refresh_timeout_ms: Some(200.0),
        home: Some("/home/test".to_owned()),
    });
    let change_log = Arc::clone(log);
    manager.on_change(Arc::new(move |transition| {
        let statuses: Vec<String> = transition
            .current
            .records
            .iter()
            .map(|record| {
                format!(
                    "{}:{}",
                    text(record.entry.get("provider")).unwrap_or_default(),
                    text(record.entry.get("status")).unwrap_or_default()
                )
            })
            .collect();
        change_log.lock().expect("log").push(format!(
            "change {} {}",
            transition.current.cwd,
            statuses.join(",")
        ));
    }));
    manager
}

/// Replaces each `"fetchedAt":"<ISO>"` value with `<ISO>`.
fn normalize(text: &str) -> String {
    let marker = r#""fetchedAt":""#;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(marker) {
        out.push_str(&rest[..index + marker.len()]);
        rest = &rest[index + marker.len()..];
        let end = rest.find('"').unwrap_or(rest.len());
        out.push_str("<ISO>");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn normalize_replaces_only_fetched_at() {
    assert_eq!(
        normalize(
            r#"{"fetchedAt":"2026-10-02T01:02:03.456Z","updatedAt":"2026-10-02T01:02:03.456Z"}"#
        ),
        r#"{"fetchedAt":"<ISO>","updatedAt":"2026-10-02T01:02:03.456Z"}"#
    );
}

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/agent/provider-snapshot-manager.js",
    "728534c891ca9db46313894a67f461ffc915fdf575d0ee4ecbb4cdf54fad119b",
)];

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

#[tokio::test]
async fn provider_snapshot_manager_matches_pinned_build() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: provider snapshot differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    // `SPOCKY_PASEO_DIST` is `<runtime>/packages/server/dist/server`.
    let protocol = Path::new(&dist).join("../../../protocol/dist");
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
        .arg(protocol)
        .arg(STEPS)
        .arg(CATALOG)
        .env("HOME", "/home/test")
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let node_output = parse(&String::from_utf8_lossy(&output.stdout)).expect("node output");
    let rust = rust_steps(node_output.get("defs").expect("defs")).await;
    assert_eq!(
        normalize(&stringify(&rust)),
        normalize(&stringify(node_output.get("steps").expect("steps")))
    );
}
