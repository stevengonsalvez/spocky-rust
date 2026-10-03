//! Differential check of the stored-agent helpers against the pinned build:
//! `persistence-hooks.js` (`buildConfigOverrides`, `buildSessionConfig`,
//! `isStoredAgentProviderAvailable`, `resolveStoredAgentUpdatedAt`,
//! `extractTimestamps`, `extractAttention`, `toAgentPersistenceHandle`)
//! and `agent-projections.js` (`buildStoredAgentPayload`), over the same
//! stored records. Dates print as `toISOString`, as `JSON.stringify` does.
//!
//! Every input is fixed, so nothing is normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::Path;
use std::process::Command;

use spocky_session::agent_projection::{AgentAttention, build_stored_agent_payload};
use spocky_session::clock::iso_from_millis;
use spocky_session::persistence_hooks::{
    build_config_overrides, build_session_config, extract_attention, extract_timestamps,
    is_stored_agent_provider_available, resolve_stored_agent_updated_at,
    to_agent_persistence_handle,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const VALID: &[&str] = &["codex", "claude"];

const RECORDS: &str = r#"[
  {"id":"a1","provider":"codex","cwd":"/w","workspaceId":"wks_1",
   "createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:05:00.000Z","lastActivityAt":"2026-07-12T10:06:00.000+01:00",
   "lastUserMessageAt":"2026-07-12T10:04:00Z","lastStatus":"idle","lastModeId":"auto","title":"T",
   "config":{"model":" m ","modeId":"auto","thinkingOptionId":"high","featureValues":{"f":true},"providerOptions":null,
             "mcpServers":{"paseo":{"type":"http","url":"http://127.0.0.1:9/mcp/agents"},"other":{"command":"x"}}},
   "runtimeInfo":{"provider":"codex","sessionId":"s1","model":null,"thinkingOptionId":"low","extra":{"k":1}},
   "persistence":{"provider":"codex","sessionId":"s1","nativeHandle":"n1","metadata":{"mcpServers":{"x":{}},"cwd":"/w"}},
   "requiresAttention":true,"attentionReason":"finished","attentionTimestamp":"2026-07-12T10:07:00.000Z",
   "labels":{"a":"b","n":1},"owner":{"kind":"user"},"archivedAt":"2026-07-12T11:00:00.000Z"},
  {"id":"a2","provider":"claude","cwd":"/x","createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z",
   "lastActivityAt":"","lastUserMessageAt":null,"lastStatus":"closed","config":null,"persistence":null,
   "requiresAttention":false,"attentionReason":null,"attentionTimestamp":null},
  {"id":"a3","provider":"ghost","cwd":"/y","createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"bad","lastActivityAt":"2026-07-12T09:00:00.000Z",
   "lastStatus":"idle","persistence":{"provider":"ghost","sessionId":"s3"},"requiresAttention":true,"attentionReason":"error"},
  {"id":"a4","provider":"codex","cwd":"/z","createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z",
   "lastStatus":"running","runtimeInfo":{"provider":"codex","sessionId":null},
   "persistence":{"provider":"claude","sessionId":""},"labels":{}},
  {"id":"a5","provider":"codex","cwd":"/z","createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z",
   "lastStatus":"idle","persistence":{"provider":"claude","sessionId":"s5","metadata":{"mcpServers":{}}}},
  {"id":"a6","provider":"codex","cwd":"/z","createdAt":"Jan 3 2020 00:00:00 GMT","updatedAt":"Wed, 01 Jan 2020 12:00:00 GMT",
   "lastActivityAt":"2020-01-02 00:00:00 GMT","lastUserMessageAt":"1/3/2020 00:00:00 GMT","lastStatus":"idle",
   "requiresAttention":true,"attentionReason":"finished","attentionTimestamp":"1 Jan 2020 00:00:00 UTC"},
  {"id":"a7","provider":"codex","cwd":"/z","createdAt":"Jan 3 2020 00:00:00 +0530","updatedAt":"2020-01-01T00:00:00.000Z",
   "lastStatus":"idle","lastUserMessageAt":"not a date"}
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, recordsJson, validJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const hooks = await import(`${dist}/server/persistence-hooks.js`);
const { buildStoredAgentPayload } = await import(`${dist}/server/agent/agent-projections.js`);
const valid = JSON.parse(validJson);
const out = JSON.parse(recordsJson).map((record) => {
  const row = {
    overrides: hooks.buildConfigOverrides(record),
    sessionConfig: hooks.buildSessionConfig(record, { validProviders: valid }),
    available: hooks.isStoredAgentProviderAvailable(record, valid),
    updatedAt: hooks.resolveStoredAgentUpdatedAt(record),
    timestamps: hooks.extractTimestamps(record),
    attention: hooks.extractAttention(record),
    handle: hooks.toAgentPersistenceHandle(valid, record.persistence),
  };
  try {
    row.payload = buildStoredAgentPayload(record, valid);
  } catch (error) {
    row.payloadError = { name: error.name, message: error.message };
  }
  return row;
});
process.stdout.write(JSON.stringify(out));
"#;

fn date(millis: Option<i64>) -> JsValue {
    millis.map_or(JsValue::Null, |millis| {
        JsValue::String(iso_from_millis(millis))
    })
}

fn rust_output() -> String {
    let valid: Vec<String> = VALID.iter().map(|&provider| provider.to_owned()).collect();
    let rows = parse(RECORDS)
        .expect("records")
        .as_array()
        .expect("records")
        .iter()
        .map(|record| {
            let mut row = JsObject::new();
            row.insert("overrides", build_config_overrides(record));
            row.insert(
                "sessionConfig",
                build_session_config(record, Some(&valid)).unwrap_or(JsValue::Null),
            );
            row.insert(
                "available",
                JsValue::Bool(is_stored_agent_provider_available(record, Some(&valid))),
            );
            row.insert("updatedAt", resolve_stored_agent_updated_at(record));
            let timestamps = extract_timestamps(record);
            let mut stamps = JsObject::new();
            stamps.insert("createdAt", date(timestamps.created_at_millis));
            stamps.insert("updatedAt", date(timestamps.updated_at_millis));
            stamps.insert(
                "lastUserMessageAt",
                date(timestamps.last_user_message_at_millis),
            );
            stamps.insert("labels", timestamps.labels.unwrap_or(JsValue::Undefined));
            stamps.insert(
                "workspaceId",
                timestamps
                    .workspace_id
                    .map_or(JsValue::Undefined, JsValue::String),
            );
            stamps.insert("owner", timestamps.owner.unwrap_or(JsValue::Undefined));
            row.insert("timestamps", JsValue::Object(stamps));
            let mut attention = JsObject::new();
            match extract_attention(record) {
                AgentAttention::None => attention.insert("requiresAttention", JsValue::Bool(false)),
                AgentAttention::Required {
                    reason,
                    timestamp_millis,
                } => {
                    attention.insert("requiresAttention", JsValue::Bool(true));
                    attention.insert("attentionReason", JsValue::String(reason));
                    attention.insert("attentionTimestamp", date(Some(timestamp_millis)));
                }
            }
            row.insert("attention", JsValue::Object(attention));
            row.insert(
                "handle",
                to_agent_persistence_handle(&valid, record.get("persistence"))
                    .unwrap_or(JsValue::Null),
            );
            match build_stored_agent_payload(record, &valid) {
                Ok(payload) => row.insert("payload", payload),
                Err(error) => {
                    let mut thrown = JsObject::new();
                    thrown.insert("name", JsValue::String(error.name));
                    thrown.insert("message", JsValue::String(error.message));
                    row.insert("payloadError", JsValue::Object(thrown));
                }
            }
            JsValue::Object(row)
        })
        .collect();
    stringify(&JsValue::Array(rows))
}

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/persistence-hooks.js",
        "1eda994c0b9dac6998a51d1997c057ff9ed9283e0b110bb0158fd4d902df19d4",
    ),
    (
        "server/agent/agent-projections.js",
        "725258d3cf93e0de535d27fc245d776983303bc4c7c8874141ed9277516bb690",
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

#[test]
fn stored_agent_helpers_match_pinned_build() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: stored agent differential not run");
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
    let valid = stringify(&JsValue::Array(
        VALID
            .iter()
            .map(|&provider| JsValue::String(provider.to_owned()))
            .collect(),
    ));
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(RECORDS)
        .arg(valid)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
