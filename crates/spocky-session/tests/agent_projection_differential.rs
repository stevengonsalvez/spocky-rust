//! Differential check of the stored agent record projection against the
//! pinned build: `toStoredAgentRecord` and `AgentStorage.applySnapshot` must
//! produce byte-identical records, and throw the same `TypeError`s.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly). Dates are fixed inputs, so nothing is
//! normalized.

use std::process::Command;

use spocky_session::agent_projection::{
    AgentAttention, ManagedAgentRecordView, SnapshotOverrides, apply_snapshot_record,
    to_stored_agent_record,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// Strings equal to this marker stand for an own property holding
/// `undefined`, which JSON cannot carry.
const UNDEFINED: &str = "__undefined__";

/// `[agent, options]` for `toStoredAgentRecord`. Agents give dates as epoch
/// milliseconds.
const PROJECTIONS: &str = r#"[
  [{"id":"a1","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000001000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w"},"persistence":null,
    "attention":{"requiresAttention":false}}, {}],
  [{"id":"a2","provider":"codex","cwd":"/w","workspaceId":"wks_1","createdAt":1700000000000,
    "updatedAt":1700000002000,"lastUserMessageAt":1700000001500,
    "labels":{"surface":"workspace","paseo.parent-agent-id":"p"},"lifecycle":"running",
    "currentModeId":null,
    "config":{"provider":"codex","cwd":"/w","modeId":"plan","model":"gpt","thinkingOptionId":"",
      "featureValues":{"a":1,"b":{},"c":"__undefined__"},
      "providerOptions":{"x":{"y":null,"z":{}},"w":[1,"__undefined__",{}]},
      "toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"},"ab",7]},
      "systemPrompt":"be brief","mcpServers":{"m":{"type":"stdio","command":"x"}},
      "title":"T","internal":false},
    "runtimeInfo":{"provider":"codex","sessionId":"s1","model":null,"modeId":"__undefined__",
      "extra":{"k":[1,{}],"e":{}}},
    "features":[{"id":"f","type":"toggle","value":true},"x"],
    "persistence":{"provider":"codex","sessionId":"s1","nativeHandle":"n",
      "metadata":{"cwd":"/w","empty":{}}},
    "lastError":"boom",
    "attention":{"requiresAttention":true,"attentionReason":"finished",
      "attentionTimestamp":1700000003000},
    "internal":true,"owner":{"kind":"daemon","executionKey":"k"}},
   {"title":"Named","createdAt":"2020-01-01T00:00:00.000Z","internal":false}],
  [{"id":"a3","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000001000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"error","currentModeId":"",
    "config":{"provider":"codex","cwd":"/w","modeId":"full","featureValues":"__undefined__",
      "providerOptions":{"only":{}}},
    "runtimeInfo":{"sessionId":null,"thinkingOptionId":"high"},
    "features":{"not":"an array"},"persistence":{"provider":"codex","metadata":[]},
    "attention":{"requiresAttention":false}}, {"title":""}],
  [{"id":"a4","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000001000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w","toolPolicy":{}},"persistence":null,
    "attention":{"requiresAttention":false}}, {}],
  [{"id":"a5","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000001000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w","toolPolicy":{"preapproved":null}},"persistence":null,
    "attention":{"requiresAttention":false}}, {}],
  [{"id":"a6","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000001000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w","toolPolicy":{"preapproved":"x"}},"persistence":null,
    "attention":{"requiresAttention":false}}, {}]
]"#;

/// `[agent, existing record or null, applySnapshot options or null]`. The
/// last case has `agent.internal` false over an existing `internal: true`
/// with no override (the agent wins) and a null owner.
const SNAPSHOTS: &str = r#"[
  [{"id":"s1","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000005000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w"},"persistence":null,
    "attention":{"requiresAttention":false},"internal":false}, null, null],
  [{"id":"s2","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000005000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w"},"persistence":null,
    "attention":{"requiresAttention":false}},
   {"id":"s2","provider":"codex","cwd":"/old","createdAt":"2019-01-01T00:00:00.000Z",
    "updatedAt":"2019-01-02T00:00:00.000Z","title":"Old","labels":{},"lastStatus":"closed",
    "internal":true,"archivedAt":"2019-01-03T00:00:00.000Z"}, null],
  [{"id":"s3","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000005000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w"},"persistence":null,
    "attention":{"requiresAttention":false},"internal":false},
   {"id":"s3","provider":"codex","cwd":"/w","createdAt":"2019-01-01T00:00:00.000Z",
    "updatedAt":"2019-01-02T00:00:00.000Z","title":"Old","labels":{},"lastStatus":"idle",
    "archivedAt":null},
   {"title":"__undefined__","internal":"__undefined__"}],
  [{"id":"s4","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000005000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w"},"persistence":null,
    "attention":{"requiresAttention":false}},
   {"id":"s4","provider":"codex","cwd":"/w","createdAt":"2019-01-01T00:00:00.000Z",
    "updatedAt":"2019-01-02T00:00:00.000Z","title":null,"labels":{},"lastStatus":"idle",
    "internal":false},
   {"title":"New"}],
  [{"id":"s5","provider":"codex","cwd":"/w","createdAt":1700000000000,"updatedAt":1700000005000,
    "lastUserMessageAt":null,"labels":{},"lifecycle":"idle","currentModeId":null,
    "config":{"provider":"codex","cwd":"/w"},"persistence":null,"owner":null,
    "attention":{"requiresAttention":false},"internal":false},
   {"id":"s5","provider":"codex","cwd":"/w","createdAt":"2019-01-01T00:00:00.000Z",
    "updatedAt":"2019-01-02T00:00:00.000Z","title":"Old","labels":{},"lastStatus":"idle",
    "internal":true}, null]
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, projectionsJson, snapshotsJson, marker] = process.argv.slice(1);
const fs = await import("node:fs");
const os = await import("node:os");
const path = await import("node:path");
const { toStoredAgentRecord } = await import(`${dist}/server/agent/agent-projections.js`);
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const undef = (value) => {
  if (Array.isArray(value)) return value.map((item) => (item === marker ? undefined : undef(item)));
  if (value && typeof value === "object") {
    for (const key of Object.keys(value)) {
      value[key] = value[key] === marker ? undefined : undef(value[key]);
    }
  }
  return value;
};
const agentOf = (input) => {
  const agent = undef(input);
  agent.createdAt = new Date(agent.createdAt);
  agent.updatedAt = new Date(agent.updatedAt);
  agent.lastUserMessageAt = agent.lastUserMessageAt === null ? null : new Date(agent.lastUserMessageAt);
  if (agent.attention.requiresAttention) {
    agent.attention.attentionTimestamp = new Date(agent.attention.attentionTimestamp);
  }
  return agent;
};
const attempt = (run) => {
  try {
    return { ok: run() };
  } catch (error) {
    return { name: error.constructor.name, message: error.message };
  }
};
const projections = JSON.parse(projectionsJson).map(([agent, options]) =>
  attempt(() => toStoredAgentRecord(agentOf(agent), undef(options))));
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const home = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-projection-"));
const snapshots = [];
try {
  const storage = new AgentStorage(home, logger);
  for (const [agent, existing, options] of JSON.parse(snapshotsJson)) {
    if (existing) await storage.upsert(undef(existing));
    await storage.applySnapshot(agentOf(agent), options === null ? undefined : undef(options));
    snapshots.push(await storage.get(agent.id));
  }
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}
process.stdout.write(JSON.stringify({ projections, snapshots }));
"#;

fn undefined_markers(value: &JsValue) -> JsValue {
    match value {
        JsValue::String(text) if text == UNDEFINED => JsValue::Undefined,
        JsValue::Array(items) => JsValue::Array(items.iter().map(undefined_markers).collect()),
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, item) in object.iter() {
                out.insert(key, undefined_markers(item));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "fixture dates are integral epoch milliseconds"
)]
fn millis(value: &JsValue) -> i64 {
    value.as_f64().expect("epoch milliseconds") as i64
}

fn optional_string(value: Option<&JsValue>) -> Option<String> {
    value.and_then(JsValue::as_str).map(str::to_owned)
}

fn present(value: Option<&JsValue>) -> Option<JsValue> {
    value
        .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
        .cloned()
}

fn view(input: &JsValue) -> ManagedAgentRecordView {
    let agent = undefined_markers(input);
    let get = |key: &str| agent.get(key);
    let attention = get("attention").expect("attention");
    ManagedAgentRecordView {
        id: optional_string(get("id")).expect("id"),
        provider: optional_string(get("provider")).expect("provider"),
        cwd: optional_string(get("cwd")).expect("cwd"),
        workspace_id: optional_string(get("workspaceId")),
        created_at_millis: millis(get("createdAt").expect("createdAt")),
        updated_at_millis: millis(get("updatedAt").expect("updatedAt")),
        last_user_message_at_millis: present(get("lastUserMessageAt")).map(|value| millis(&value)),
        labels: get("labels").expect("labels").clone(),
        lifecycle: optional_string(get("lifecycle")).expect("lifecycle"),
        current_mode_id: optional_string(get("currentModeId")),
        config: get("config").expect("config").clone(),
        runtime_info: present(get("runtimeInfo")),
        features: present(get("features")),
        persistence: present(get("persistence")),
        last_error: optional_string(get("lastError")),
        attention: if attention
            .get("requiresAttention")
            .and_then(JsValue::as_bool)
            == Some(true)
        {
            AgentAttention::Required {
                reason: optional_string(attention.get("attentionReason")).expect("reason"),
                timestamp_millis: millis(attention.get("attentionTimestamp").expect("timestamp")),
            }
        } else {
            AgentAttention::None
        },
        internal: get("internal").and_then(JsValue::as_bool),
        // `agent.owner` is copied as is, so a null owner stays null.
        owner: get("owner")
            .filter(|owner| !matches!(owner, JsValue::Undefined))
            .cloned(),
    }
}

fn outcome(result: Result<JsValue, spocky_session::timeline::JsTypeError>) -> JsValue {
    let mut value = JsObject::new();
    match result {
        Ok(record) => value.insert("ok", record),
        Err(error) => {
            value.insert("name", JsValue::String("TypeError".to_owned()));
            value.insert("message", JsValue::String(error.0));
        }
    }
    JsValue::Object(value)
}

/// `Some(value)` for an own property, so a present `undefined` still counts.
fn own(options: &JsValue, key: &str) -> Option<JsValue> {
    options.get(key).cloned()
}

fn rust_output() -> String {
    let projections = parse(PROJECTIONS)
        .expect("projections")
        .as_array()
        .expect("array")
        .iter()
        .map(|case| {
            let case = case.as_array().expect("pair");
            let options = undefined_markers(&case[1]);
            let title = own(&options, "title");
            let created_at = own(&options, "createdAt");
            outcome(to_stored_agent_record(
                &view(&case[0]),
                title.as_ref().and_then(JsValue::as_str),
                created_at.as_ref().and_then(JsValue::as_str),
                own(&options, "internal").and_then(|value| value.as_bool()),
            ))
        })
        .collect();
    let snapshots = parse(SNAPSHOTS)
        .expect("snapshots")
        .as_array()
        .expect("array")
        .iter()
        .map(|case| {
            let case = case.as_array().expect("triple");
            let existing = (!case[1].is_null()).then(|| undefined_markers(&case[1]));
            let options = undefined_markers(&case[2]);
            let overrides = SnapshotOverrides {
                title: own(&options, "title").map(|value| value.as_str().map(str::to_owned)),
                internal: own(&options, "internal").map(|value| value.as_bool()),
            };
            apply_snapshot_record(&view(&case[0]), existing.as_ref(), &overrides)
                .expect("snapshot record")
        })
        .collect();
    let mut output = JsObject::new();
    output.insert("projections", JsValue::Array(projections));
    output.insert("snapshots", JsValue::Array(snapshots));
    stringify(&JsValue::Object(output))
}

#[test]
fn stored_records_match_pinned_projection() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: projection differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
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
        .args([PROJECTIONS, SNAPSHOTS, UNDEFINED])
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
