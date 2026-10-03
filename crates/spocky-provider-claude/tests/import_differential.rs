//! Differential check of `listImportableSessions` and `importSession`: the
//! pinned `ClaudeAgentClient` and `ClaudeClient` read the same fixture
//! transcripts (with fixed modification times) and must produce the same
//! `JSON.stringify` text for every call.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::client::{ClaudeClient, ClaudeClientOptions};
use spocky_session::agent_sdk::{
    AgentClient, ImportProviderSessionContext, ImportProviderSessionInput,
    ListImportableSessionsOptions,
};

const CASES: &str = include_str!("import_cases.json");

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/agent/providers/claude/agent.js",
    "c8e8e12df50c2bb45b5d33d08bb4c070e454b5919a9ad62932ffa89337f57e69",
)];

const NODE_SCRIPT: &str = r#"
import { readFileSync } from "node:fs";
const [dist, casesFile] = process.argv.slice(1);
const { ClaudeAgentClient } = await import(`${dist}/server/agent/providers/claude/agent.js`);
const quiet = () => {
  const logger = {};
  for (const level of ["trace", "debug", "info", "warn", "error", "fatal"]) logger[level] = () => {};
  logger.child = () => logger;
  return logger;
};
const client = new ClaudeAgentClient({ logger: quiet() });
const spec = JSON.parse(readFileSync(casesFile, "utf8"));
const lines = [];
for (const call of spec.calls) {
  try {
    const sessions = await client.listImportableSessions(call.options ?? undefined);
    lines.push(JSON.stringify({ name: call.name, ok: sessions.map((s) => ({
      providerHandleId: s.providerHandleId, cwd: s.cwd, title: s.title,
      firstPromptPreview: s.firstPromptPreview, lastPromptPreview: s.lastPromptPreview,
      lastActivityAt: s.lastActivityAt.getTime(),
    })) }));
  } catch (error) {
    lines.push(JSON.stringify({ name: call.name, error: error instanceof Error ? error.message : String(error) }));
  }
}
for (const item of spec.imports) {
  try {
    const result = await client.importSession(
      { providerHandleId: item.providerHandleId, cwd: item.cwd },
      { config: item.config, storedConfig: item.storedConfig, launchContext: undefined },
    );
    const mode = await result.session.getCurrentMode();
    const sessionId = result.session.id;
    await result.session.close();
    lines.push(JSON.stringify({ name: item.name, ok: {
      config: result.config, persistence: result.persistence, timeline: result.timeline,
      providerSubagentEvents: result.providerSubagentEvents, sessionId, mode,
    } }));
  } catch (error) {
    lines.push(JSON.stringify({ name: item.name, error: error instanceof Error ? error.message : String(error) }));
  }
}
process.stdout.write(lines.join("\n") + "\n");
"#;

fn write_fixtures(files: &[JsValue], config_dir: &Path) {
    for file in files {
        let path = config_dir.join(file.get("path").and_then(JsValue::as_str).expect("path"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let bytes: Vec<u8> = if let Some(hex) = file.get("hex").and_then(JsValue::as_str) {
            (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                .collect()
        } else if let Some(text) = file.get("text").and_then(JsValue::as_str) {
            text.as_bytes().to_vec()
        } else {
            file.get("lines")
                .and_then(JsValue::as_array)
                .unwrap_or_default()
                .iter()
                .fold(String::new(), |mut all, line| {
                    all.push_str(&stringify(line));
                    all.push('\n');
                    all
                })
                .into_bytes()
        };
        std::fs::write(&path, bytes).expect("fixture");
        if let Some(seconds) = file.get("mtime").and_then(JsValue::as_f64) {
            let handle = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("open");
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // Fixture seconds.
            handle
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds as u64))
                .expect("mtime");
        }
    }
}

fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

fn options_of(value: &JsValue) -> Option<ListImportableSessionsOptions> {
    if !matches!(value, JsValue::Object(_)) {
        return None;
    }
    Some(ListImportableSessionsOptions {
        limit: value.get("limit").and_then(JsValue::as_f64),
        query: value
            .get("query")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        scan_limit: value.get("scanLimit").and_then(JsValue::as_f64),
        cwd: value
            .get("cwd")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
    })
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

#[test]
#[allow(clippy::too_many_lines)] // One block per kind of call.
fn imports_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let spec = parse(CASES).expect("cases");
    let files = spec
        .get("files")
        .and_then(JsValue::as_array)
        .expect("files");
    let calls = spec
        .get("calls")
        .and_then(JsValue::as_array)
        .expect("calls");
    let imports = spec
        .get("imports")
        .and_then(JsValue::as_array)
        .expect("imports");
    let scratch = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp")
        .join(format!("spocky-import-diff-{}", std::process::id()));
    let node_config = scratch.join("node-config");
    let rust_config = scratch.join("rust-config");
    for config in [&node_config, &rust_config] {
        std::fs::create_dir_all(config).expect("config");
        write_fixtures(files, config);
    }
    let cases_file = scratch.join("cases.json");
    std::fs::write(&cases_file, CASES).expect("cases file");
    let output = Command::new(timeout_binary())
        .env_clear()
        .env("HOME", scratch.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_CONFIG_DIR", &node_config)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(&cases_file)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<String> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(expected.len(), calls.len() + imports.len());
    let env_config = rust_config.to_string_lossy().into_owned();
    let home = scratch.join("home").to_string_lossy().into_owned();
    let client = ClaudeClient::new(ClaudeClientOptions {
        process_env: Some(Arc::new(move || {
            let mut env = JsObject::new();
            env.insert("HOME", text(&home));
            env.insert("PATH", text("/usr/bin:/bin"));
            env.insert("CLAUDE_CONFIG_DIR", text(&env_config));
            env
        })),
        ..ClaudeClientOptions::default()
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let mut failures = Vec::new();
    let mut lines = expected.iter();
    for call in calls {
        let name = call
            .get("name")
            .and_then(JsValue::as_str)
            .unwrap_or_default();
        let options = call.get("options").and_then(options_of);
        let sessions = runtime
            .block_on(client.list_importable_sessions(options).expect("supported"))
            .expect("listed");
        let rows: Vec<JsValue> = sessions
            .iter()
            .map(|session| {
                object(vec![
                    ("providerHandleId", text(&session.provider_handle_id)),
                    ("cwd", text(&session.cwd)),
                    (
                        "title",
                        session.title.as_deref().map_or(JsValue::Null, text),
                    ),
                    (
                        "firstPromptPreview",
                        session
                            .first_prompt_preview
                            .as_deref()
                            .map_or(JsValue::Null, text),
                    ),
                    (
                        "lastPromptPreview",
                        session
                            .last_prompt_preview
                            .as_deref()
                            .map_or(JsValue::Null, text),
                    ),
                    (
                        "lastActivityAt",
                        JsValue::Number(session.last_activity_at_millis),
                    ),
                ])
            })
            .collect();
        let actual = stringify(&object(vec![
            ("name", text(name)),
            ("ok", JsValue::Array(rows)),
        ]));
        let node_line = lines.next().expect("line");
        if &actual != node_line {
            failures.push(format!("{name}\n  node: {node_line}\n  rust: {actual}"));
        }
    }
    for item in imports {
        let name = item
            .get("name")
            .and_then(JsValue::as_str)
            .unwrap_or_default();
        let input = ImportProviderSessionInput {
            provider_handle_id: item
                .get("providerHandleId")
                .and_then(JsValue::as_str)
                .unwrap_or_default()
                .to_owned(),
            cwd: item
                .get("cwd")
                .and_then(JsValue::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        let context = ImportProviderSessionContext {
            config: item.get("config").cloned().unwrap_or(JsValue::Undefined),
            stored_config: item
                .get("storedConfig")
                .cloned()
                .unwrap_or(JsValue::Undefined),
            launch_context: None,
        };
        let result = runtime.block_on(client.import_session(input, context).expect("supported"));
        let actual = match result {
            Ok(imported) => {
                let session = imported.session;
                let mode = runtime.block_on(session.get_current_mode()).ok().flatten();
                let session_id = session.id();
                let _ = runtime.block_on(session.close());
                let timeline: Vec<JsValue> = imported
                    .timeline
                    .iter()
                    .map(|entry| {
                        let mut row = JsObject::new();
                        row.insert("item", entry.item.clone());
                        if let Some(timestamp) = &entry.timestamp {
                            row.insert("timestamp", text(timestamp));
                        }
                        JsValue::Object(row)
                    })
                    .collect();
                stringify(&object(vec![
                    ("name", text(name)),
                    (
                        "ok",
                        object(vec![
                            ("config", imported.config),
                            ("persistence", imported.persistence),
                            ("timeline", JsValue::Array(timeline)),
                            (
                                "providerSubagentEvents",
                                JsValue::Array(
                                    imported.provider_subagent_events.unwrap_or_default(),
                                ),
                            ),
                            (
                                "sessionId",
                                session_id.map_or(JsValue::Null, JsValue::String),
                            ),
                            ("mode", mode.map_or(JsValue::Null, JsValue::String)),
                        ]),
                    ),
                ]))
            }
            Err(error) => stringify(&object(vec![
                ("name", text(name)),
                ("error", text(&error.message)),
            ])),
        };
        let node_line = lines.next().expect("line");
        if &actual != node_line {
            failures.push(format!("{name}\n  node: {node_line}\n  rust: {actual}"));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} calls differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
