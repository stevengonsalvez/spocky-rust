//! Differential check of `registry_fetch_catalog` against the pinned
//! build's `buildProviderRegistry(...)[provider].fetchCatalog(options,
//! client, context)`, with a scripted fake client: model mapping and
//! merging (profile replacement, additions, defaults, compatibility models),
//! mode decoration, the static-mode default-mode probe, and the `TypeError`
//! of a catalogue without modes.
//!
//! The definition modes come from node's registry, which the port takes as
//! input. Nothing is normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use spocky_session::agent_sdk::{
    AbortController, AbortSignal, ActivityGuard, AgentClient, AgentCreateSessionOptions,
    AgentError, AgentLaunchContext, AgentResult, AgentResumeSessionOptions, AgentSession,
    BoxFuture, FetchCatalogOptions, ProviderRefreshContext, ResolveAgentDefaultModeInput,
};
use spocky_session::provider_catalog::{
    RegistryCatalog, registry_fetch_catalog, resolve_configured_models,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// `[provider, providerOverrides, options, catalog]` per case.
const CASES: &str = r#"[
  ["codex", {}, {"scope":"workspace","cwd":"/w1","force":false},
   {"models":[
      {"id":"m1","label":"M1","isDefault":true,"thinkingOptions":[{"id":"low","label":"Low"},{"id":"high","label":"High","isDefault":true}]},
      {"id":"m2","label":"M2","isSelectable":false,"defaultThinkingOptionId":"x","thinkingOptions":[{"id":"y","isDefault":true}]},
      {"id":"m3","label":"M3","provider":"other","defaultThinkingOptionId":""}],
    "defaultModeId":"auto","extra":{"k":1},
    "modes":[{"id":"auto","label":"A"},{"id":"auto-review","label":"R","icon":"X","colorTier":"safe"},
             {"id":"full-access","label":"F","icon":""},{"id":"other","label":"O"}]}],
  ["codex", {"codex":{"additionalModels":[{"id":"extra","label":"Extra","isDefault":true},{"id":"m2","label":"M2 renamed"}]}},
   {"scope":"global","force":true},
   {"models":[{"id":"m1","label":"M1","isDefault":true},{"id":"m2","label":"M2","isSelectable":false}],"modes":[]}],
  ["codex", {"codex":{"models":[{"id":"p1","label":"P1"}],"additionalModels":[{"id":"p2","label":"P2","isDefault":true}]}},
   {"scope":"workspace","cwd":"/w2","force":false},
   {"models":[],"modes":[]}],
  ["codex", {"codex":{"models":[{"id":"p1","label":"P1"}]}},
   {"scope":"global","force":false},
   {"models":[],"modes":[]}],
  ["pi", {"pi":{"models":[{"id":"q1","label":"Q1","thinkingOptions":[{"id":"t","isDefault":true}]}]}},
   {"scope":"workspace","cwd":"/w3","force":false},
   {"models":[{"id":"ignored"}],"modes":[{"id":"m","label":"M"}],"defaultModeId":null}],
  ["codex", {}, {"scope":"workspace","cwd":"/w4","force":false},
   {"models":[{"id":"m1","label":"M1"}]}]
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, casesJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { buildProviderRegistry } = await import(`${dist}/server/agent/provider-registry.js`);
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const out = [];
for (const [provider, providerOverrides, options, catalog] of JSON.parse(casesJson)) {
  const log = [];
  const definition = buildProviderRegistry(logger, { providerOverrides })[provider];
  const client = {
    provider,
    async fetchCatalog(fetchOptions) {
      log.push(`fetchCatalog ${JSON.stringify(fetchOptions)}`);
      return catalog;
    },
    async resolveDefaultModeId(input) {
      log.push(`resolveDefaultModeId ${JSON.stringify(input.config)} ${input.signal ? "signal" : "none"} ${input.env === undefined ? "noenv" : "env"}`);
      return "auto-review";
    },
  };
  const context = {
    signal: new AbortController().signal,
    async runActivity(name, operation) {
      log.push(`activity ${name}`);
      return await operation();
    },
  };
  const row = { modes: definition.modes, log };
  try {
    row.result = await definition.fetchCatalog(options, client, context);
  } catch (error) {
    row.error = { name: error.name, message: error.message };
  }
  out.push(row);
}
process.stdout.write(JSON.stringify(out));
"#;

type Log = Arc<Mutex<Vec<String>>>;

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
    catalog: JsValue,
    log: Log,
}

impl Fake {
    fn push(&self, line: String) {
        self.log.lock().expect("log").push(line);
    }
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
    fn fetch_catalog(
        &self,
        options: FetchCatalogOptions,
        _context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        self.push(format!("fetchCatalog {}", options_json(&options)));
        let catalog = self.catalog.clone();
        Box::pin(async move { Ok(catalog) })
    }
    fn resolve_default_mode_id(
        &self,
        input: ResolveAgentDefaultModeInput,
    ) -> Option<BoxFuture<'_, AgentResult<Option<String>>>> {
        self.push(format!(
            "resolveDefaultModeId {} {} {}",
            stringify(&input.config),
            if input.signal.is_some() {
                "signal"
            } else {
                "none"
            },
            if input.env.is_none() { "noenv" } else { "env" }
        ));
        Some(Box::pin(async { Ok(Some("auto-review".to_owned())) }))
    }
    fn is_available(
        &self,
        _signal: Option<AbortSignal>,
        _options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        Box::pin(async { Ok(true) })
    }
}

struct Context {
    signal: AbortSignal,
    log: Log,
}

impl ProviderRefreshContext for Context {
    fn signal(&self) -> &AbortSignal {
        &self.signal
    }
    fn begin_activity(&self, name: &str) -> ActivityGuard {
        self.log
            .lock()
            .expect("log")
            .push(format!("activity {name}"));
        ActivityGuard::new(|| {})
    }
}

fn options_of(value: &JsValue) -> FetchCatalogOptions {
    let force = matches!(value.get("force"), Some(JsValue::Bool(true)));
    match value.get("cwd").and_then(JsValue::as_str) {
        Some(cwd) => FetchCatalogOptions::Workspace {
            cwd: cwd.to_owned(),
            force,
        },
        None => FetchCatalogOptions::Global { force },
    }
}

fn models_of(overrides: &JsValue, provider: &str, key: &str) -> Vec<JsValue> {
    overrides
        .get(provider)
        .and_then(|value| value.get(key))
        .and_then(JsValue::as_array)
        .map(<[JsValue]>::to_vec)
        .unwrap_or_default()
}

async fn rust_output(node_rows: &[JsValue]) -> String {
    let mut rows = Vec::new();
    for (case, node_row) in parse(CASES)
        .expect("cases")
        .as_array()
        .expect("cases")
        .iter()
        .zip(node_rows)
    {
        let case = case.as_array().expect("case");
        let provider = case[0].as_str().expect("provider").to_owned();
        let log: Log = Arc::default();
        let client: Arc<dyn AgentClient> = Arc::new(Fake {
            provider: provider.clone(),
            catalog: case[3].clone(),
            log: Arc::clone(&log),
        });
        let definition_modes = node_row
            .get("modes")
            .and_then(JsValue::as_array)
            .map(<[JsValue]>::to_vec)
            .unwrap_or_default();
        let hook = registry_fetch_catalog(RegistryCatalog {
            definition_modes: definition_modes.clone(),
            profile_models: resolve_configured_models(
                &provider,
                client.as_ref(),
                &models_of(&case[1], &provider, "models"),
            ),
            additional_models: resolve_configured_models(
                &provider,
                client.as_ref(),
                &models_of(&case[1], &provider, "additionalModels"),
            ),
            profile_models_are_additive: false,
            provider,
        });
        let context = Arc::new(Context {
            signal: AbortController::default().signal(),
            log: Arc::clone(&log),
        });
        let result = hook(options_of(&case[2]), client, context).await;
        let mut row = JsObject::new();
        row.insert("modes", JsValue::Array(definition_modes));
        row.insert(
            "log",
            JsValue::Array(
                log.lock()
                    .expect("log")
                    .iter()
                    .map(|line| JsValue::String(line.clone()))
                    .collect(),
            ),
        );
        match result {
            Ok(catalog) => row.insert("result", catalog),
            Err(error) => {
                let mut thrown = JsObject::new();
                thrown.insert("name", JsValue::String(error.name));
                thrown.insert("message", JsValue::String(error.message));
                row.insert("error", JsValue::Object(thrown));
            }
        }
        rows.push(JsValue::Object(row));
    }
    stringify(&JsValue::Array(rows))
}

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/agent/provider-registry.js",
    "db1b3c18d8306d13f8805144dbcc35e2a0274bb1f1e3d178cfa924f904258eac",
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
async fn registry_fetch_catalog_matches_pinned_build() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: provider catalog differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    // The global scope's default-mode probe uses the process working
    // directory; node runs in this test's.
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(CASES)
        .current_dir(std::env::current_dir().expect("cwd"))
        .env("HOME", "/home/test")
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let node_text = String::from_utf8_lossy(&output.stdout).into_owned();
    let node_rows = parse(&node_text).expect("node output");
    let rust = rust_output(node_rows.as_array().expect("rows")).await;
    assert_eq!(rust, node_text);
}
