//! Differential check of agent stream coalescing against the pinned build's
//! `AgentStreamCoalescer`, driven by the same operations on a fake clock and
//! fake timers: the `handle` results, timer schedules and `onFlush` payloads
//! must match in order.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;

use spocky_session::stream_coalescer::{AgentStreamCoalescer, CoalescerFlush};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// `["handle", now, agentId, event]`, `["advance", now]` (fire due timers in
/// due then creation order, with the clock at `now`), `["flushFor", now,
/// agentId]`, `["flushAll", now]`, `["discard", now, agentId]`.
const OPS: &str = r#"[
  ["handle", 1000, "a", {"type":"timeline","provider":"codex","turnId":"t1","item":{"type":"assistant_message","text":"He"}}],
  ["handle", 1010, "a", {"type":"timeline","provider":"codex","turnId":"t1","item":{"type":"assistant_message","text":"llo"}}],
  ["handle", 1020, "a", {"type":"timeline","provider":"codex","turnId":"t1","item":{"type":"assistant_message","text":""}}],
  ["handle", 1030, "a", {"type":"timeline","provider":"codex","turnId":"t1","item":{"type":"assistant_message","text":" you","messageId":"m2"}}],
  ["handle", 1031, "a", {"type":"timeline","provider":"codex","turnId":"t1","item":{"type":"reasoning","text":"r1"}}],
  ["handle", 1032, "a", {"type":"timeline","provider":"codex","turnId":"t2","item":{"type":"reasoning","text":"r2"}}],
  ["handle", 1033, "a", {"type":"turn_started","provider":"codex","turnId":"t2"}],
  ["handle", 1034, "a", {"type":"timeline","provider":"codex","item":{"type":"user_message","text":"u"}}],
  ["advance", 1069],
  ["advance", 1070],
  ["handle", 1080, "a", {"type":"timeline","provider":"codex","turnId":"t2","item":{"type":"tool_call","callId":"c1","name":"shell","status":"running","detail":{"type":"shell","command":"x"},"error":null}}],
  ["handle", 1090, "a", {"type":"timeline","provider":"codex","turnId":"t2","item":{"type":"tool_call","callId":"c1","name":"shell","status":"running","detail":{"type":"shell","command":"x","output":"1"},"error":null}}],
  ["handle", 1095, "a", {"type":"timeline","provider":"codex","turnId":"t2","item":{"type":"assistant_message","text":"mid"}}],
  ["handle", 1100, "a", {"type":"timeline","provider":"codex","turnId":"t2","item":{"type":"tool_call","callId":"c1","name":"shell","status":"completed","detail":{"type":"shell","command":"x","output":"12"},"error":null}}],
  ["advance", 1200],
  ["handle", 1300, "b", {"type":"timeline","provider":"p","item":{"type":"assistant_message"}}],
  ["handle", 1310, "b", {"type":"timeline","provider":"p","item":{"type":"assistant_message","text":"x"}}],
  ["handle", 1311, "b", {"type":"timeline","provider":"p","item":{"type":"tool_call","callId":0,"status":"running"}}],
  ["handle", 1312, "b", {"type":"timeline","provider":"p","item":{"type":"tool_call","callId":-0,"status":"running","n":2}}],
  ["handle", 1313, "b", {"type":"timeline","provider":"p","item":{"type":"tool_call","callId":{"k":1},"status":"running"}}],
  ["handle", 1314, "b", {"type":"timeline","provider":"p","item":{"type":"tool_call","callId":{"k":1},"status":"running","n":3}}],
  ["handle", 1315, "c", {"type":"timeline","provider":"p","item":{"type":"reasoning","text":"c"}}],
  ["handle", 1316, "c", {"type":"timeline","provider":"q","item":{"type":"reasoning","text":"d"}}],
  ["handle", 1317, "b", {"type":"timeline","provider":"p","item":{"type":"reasoning","text":"y"}}],
  ["flushAll", 1320],
  ["advance", 1400],
  ["handle", 1500, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":"z"}}],
  ["handle", 1510, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":"zz"}}],
  ["flushFor", 1520, "a"],
  ["advance", 1600],
  ["handle", 1610, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":"after"}}],
  ["handle", 1620, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":"more"}}],
  ["discard", 1630, "a"],
  ["advance", 1700],
  ["handle", 1710, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":"fresh"}}],
  ["handle", 1711, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":1}}],
  ["handle", 1712, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":2}}],
  ["advance", 1800],
  ["handle", 1900, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":1}}],
  ["handle", 1901, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":2}}],
  ["handle", 1902, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":null}}],
  ["advance", 2000],
  ["handle", 2060, "a", {"type":"timeline","provider":"codex","item":{"type":"reasoning","text":"exactly one window later"}}],
  ["advance", 2200]
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, opsJson] = process.argv.slice(1);
const { AgentStreamCoalescer } = await import(`${dist}/server/agent/agent-stream-coalescer.js`);
let clock = 0;
let nextId = 0;
const timers = new Map();
const out = [];
const coalescer = new AgentStreamCoalescer({
  windowMs: 60,
  now: () => clock,
  timers: {
    setTimeout(callback, ms) {
      const id = ++nextId;
      timers.set(id, { due: clock + ms, callback });
      out.push(["schedule", ms]);
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  },
  onFlush: (payload) => out.push(["flush", payload]),
});
for (const [op, now, agentId, event] of JSON.parse(opsJson)) {
  clock = now;
  if (op === "handle") out.push(["handle", coalescer.handle(agentId, event)]);
  if (op === "flushFor") coalescer.flushFor(agentId);
  if (op === "flushAll") coalescer.flushAll();
  if (op === "discard") coalescer.flushAndDiscard(agentId);
  if (op === "advance") {
    const due = [...timers].filter(([, timer]) => timer.due <= clock)
      .sort((a, b) => a[1].due - b[1].due || a[0] - b[0]);
    for (const [id, timer] of due) {
      if (timers.has(id)) {
        timers.delete(id);
        timer.callback();
      }
    }
  }
}
process.stdout.write(JSON.stringify(out));
"#;

fn flush_value(flush: CoalescerFlush) -> JsValue {
    let mut payload = JsObject::new();
    payload.insert("agentId", JsValue::String(flush.agent_id));
    payload.insert("item", flush.item);
    payload.insert("provider", flush.provider);
    if let Some(turn_id) = flush.turn_id {
        payload.insert("turnId", turn_id);
    }
    JsValue::Array(vec![
        JsValue::String("flush".to_owned()),
        JsValue::Object(payload),
    ])
}

struct FakeTimer {
    id: u64,
    due: f64,
    agent_id: String,
    token: u64,
}

fn rust_output() -> String {
    let mut coalescer = AgentStreamCoalescer::new(60.0);
    let mut timers: Vec<FakeTimer> = Vec::new();
    let mut next_id = 0;
    let mut out: Vec<JsValue> = Vec::new();
    for op in parse(OPS).expect("ops").as_array().expect("array") {
        let op = op.as_array().expect("op");
        let now = op[1].as_f64().expect("now");
        let agent = || op[2].as_str().expect("agent");
        match op[0].as_str().expect("name") {
            "handle" => {
                let outcome = coalescer.handle(agent(), &op[3], now);
                out.extend(outcome.flushes.into_iter().map(flush_value));
                if let Some(timer) = outcome.timer {
                    next_id += 1;
                    out.push(JsValue::Array(vec![
                        JsValue::String("schedule".to_owned()),
                        JsValue::Number(timer.delay_ms),
                    ]));
                    timers.push(FakeTimer {
                        id: next_id,
                        due: now + timer.delay_ms,
                        agent_id: timer.agent_id,
                        token: timer.token,
                    });
                }
                out.push(JsValue::Array(vec![
                    JsValue::String("handle".to_owned()),
                    JsValue::Bool(outcome.coalesced),
                ]));
            }
            "flushFor" => out.extend(
                coalescer
                    .flush_for(agent(), now)
                    .into_iter()
                    .map(flush_value),
            ),
            "flushAll" => out.extend(coalescer.flush_all(now).into_iter().map(flush_value)),
            "discard" => out.extend(
                coalescer
                    .flush_and_discard(agent(), now)
                    .into_iter()
                    .map(flush_value),
            ),
            _ => {
                let mut due: Vec<FakeTimer> = Vec::new();
                timers.retain_mut(|timer| {
                    if timer.due <= now {
                        due.push(FakeTimer {
                            id: timer.id,
                            due: timer.due,
                            agent_id: std::mem::take(&mut timer.agent_id),
                            token: timer.token,
                        });
                        false
                    } else {
                        true
                    }
                });
                due.sort_by(|a, b| a.due.total_cmp(&b.due).then(a.id.cmp(&b.id)));
                for timer in due {
                    out.extend(
                        coalescer
                            .fire(&timer.agent_id, timer.token, now)
                            .into_iter()
                            .map(flush_value),
                    );
                }
            }
        }
    }
    stringify(&JsValue::Array(out))
}

#[test]
fn coalescing_matches_pinned_coalescer() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: coalescer differential not run");
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
        .arg(OPS)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
