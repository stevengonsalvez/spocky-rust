//! Differential check of timeline item size limits against the pinned
//! build's `limitAgentTimelineItemContent`: the same items must come back
//! byte-identical, or throw the same `TypeError`.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;

use spocky_session::timeline_content::limit_agent_timeline_item_content;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// Items with placeholders: `__BIG__` is 65,537 `x`s, `__EXACT__` is
/// 65,536 `y`s, and `__SPLIT__` puts an emoji across the 65,536 cut.
const ITEMS: &str = r#"[
  {"type":"tool_call","callId":"c1","name":"shell","status":"completed","error":null,
   "detail":{"type":"shell","command":"ls","output":"__BIG__","exitCode":0}},
  {"type":"tool_call","callId":"c2","name":"shell","status":"completed","error":null,
   "detail":{"type":"shell","command":"ls","output":"__EXACT__"}},
  {"type":"tool_call","callId":"c3","name":"shell","status":"running","error":null,
   "detail":{"type":"shell","command":"ls","output":"__SPLIT__"}},
  {"type":"tool_call","callId":"c4","name":"shell","status":"failed",
   "error":{"content":"__BIG__","code":1},"detail":{"type":"shell","command":"x"}},
  {"type":"tool_call","callId":"c5","name":"shell","status":"failed","error":"__BIG__",
   "detail":{"type":"shell","command":"x"}},
  {"type":"tool_call","callId":"c6","name":"shell","status":"failed","error":["__BIG__"],
   "detail":{"type":"shell","command":"x"}},
  {"type":"tool_call","callId":"c7","name":"shell","status":"completed",
   "error":{"content":"__BIG__"},"detail":{"type":"shell","command":"x","output":"__BIG__"}},
  {"type":"tool_call","callId":"c8","name":"note","status":"completed","error":null,
   "detail":{"type":"plain_text","label":"L","text":"__SPLIT__","icon":"eye"}},
  {"type":"tool_call","callId":"c9","name":"note","status":"completed","error":null,
   "detail":{"type":"plain_text","label":"L"}},
  {"type":"assistant_message","text":"__BIG__"},
  {"type":"tool_call","callId":"c10","name":"x","status":"running","error":null},
  {"type":"tool_call","callId":"c11","name":"x","status":"running","error":null,"detail":null},
  {"type":"tool_call","callId":"c12","name":"x","status":"running","error":null,"detail":"text"},
  {"type":"tool_call","callId":"c13","name":"shell","status":"failed",
   "error":{"content":"__SPLIT__"},"detail":{"type":"shell","command":"x","output":"__BIG__"}}
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, itemsJson] = process.argv.slice(1);
const { limitAgentTimelineItemContent } = await import(`${dist}/server/agent/agent-timeline-content.js`);
const big = { __BIG__: "x".repeat(65537), __EXACT__: "y".repeat(65536), __SPLIT__: "a".repeat(65535) + "😀tail" };
const fill = (value) => {
  if (typeof value === "string") return big[value] ?? value;
  if (Array.isArray(value)) return value.map(fill);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, fill(item)]));
  }
  return value;
};
const results = JSON.parse(itemsJson).map((item) => {
  try {
    return { ok: limitAgentTimelineItemContent(fill(item)) };
  } catch (error) {
    return { name: error.constructor.name, message: error.message };
  }
});
process.stdout.write(JSON.stringify(results));
"#;

fn fill(value: &JsValue) -> JsValue {
    match value {
        JsValue::String(text) => JsValue::String(match text.as_str() {
            "__BIG__" => "x".repeat(65_537),
            "__EXACT__" => "y".repeat(65_536),
            "__SPLIT__" => format!("{}😀tail", "a".repeat(65_535)),
            other => other.to_owned(),
        }),
        JsValue::Array(items) => JsValue::Array(items.iter().map(fill).collect()),
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, item) in object.iter() {
                out.insert(key, fill(item));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn rust_output() -> String {
    let results = parse(ITEMS)
        .expect("items")
        .as_array()
        .expect("array")
        .iter()
        .map(|item| {
            let mut result = JsObject::new();
            match limit_agent_timeline_item_content(fill(item)) {
                Ok(limited) => result.insert("ok", limited),
                Err(error) => {
                    result.insert("name", JsValue::String("TypeError".to_owned()));
                    result.insert("message", JsValue::String(error.0));
                }
            }
            JsValue::Object(result)
        })
        .collect();
    stringify(&JsValue::Array(results))
}

#[test]
fn item_limits_match_pinned_content_module() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: content limit differential not run");
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
        .arg(ITEMS)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8_lossy(&output.stdout);
    let actual = rust_output();
    assert!(
        actual == expected,
        "content limits differ from the pinned module"
    );
}
