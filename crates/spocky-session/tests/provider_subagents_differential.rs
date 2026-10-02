//! Differential check of `ProviderSubagentStore` against the pinned build:
//! the same input events must produce the same store events, and the same
//! `list`, `listAll`, `get`, `fetchTimeline` and `deleteParent` results.
//!
//! Normalized: wall-clock ISO timestamps (`<ISO>`, for events without one)
//! and random timeline epochs (`<UUID>`), nothing else.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::Path;
use std::process::Command;

use spocky_session::provider_subagents::ProviderSubagentStore;
use spocky_session::timeline::FetchDirection;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// `[parentAgentId, provider, event]` applied in order.
const INPUTS: &str = r#"[
  ["parent-a", "codex", {"type":"upsert","id":"child-1","title":"Explore","cwd":"/w/child","status":"running","toolCallId":"call-1","timestamp":"2026-07-12T10:00:00.000Z"}],
  ["parent-a", "codex", {"type":"timeline","id":"child-1","item":{"type":"assistant_message","text":"Found it."},"timestamp":"2026-07-12T10:00:01.000Z"}],
  ["parent-a", "codex", {"type":"upsert","id":"child-1","status":"completed","title":null,"timestamp":"2026-07-12T10:00:02.000Z"}],
  ["parent-a", "codex", {"type":"upsert","id":"child-0","subtitle":"early","parentSubagentId":"child-1","status":null,"timestamp":"2026-07-12T09:00:00.000Z"}],
  ["parent-a", "codex", {"type":"timeline","id":"child-2","item":{"type":"reasoning","text":"thinking"},"timestamp":"2026-07-12T10:00:03.000Z"}],
  ["parent-a", "codex", {"type":"timeline","id":"child-2","item":{"type":"reasoning","text":" more"},"timestamp":"2026-07-12T10:00:04.000Z"}],
  ["parent-a", "codex", {"type":"timeline","id":"child-2","item":{"type":"tool_call","callId":"c","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"print","output":"OUTPUT"}},"timestamp":"2026-07-12T10:00:05.000Z"}],
  ["parent-a", "codex", {"type":"rename","id":"child-3","description":"unknown type upserts"}],
  ["parent-b", "claude", {"type":"upsert","id":"child-1","title":"Review","status":"failed","timestamp":"2026-07-12T10:00:03.000Z"}],
  ["parent-b", "claude", {"type":"remove","id":"missing"}],
  ["parent-a", "codex", {"type":"remove","id":"child-0"}]
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, inputsJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { ProviderSubagentStore } = await import(`${dist}/server/agent/provider-subagents/store.js`);
const store = new ProviderSubagentStore();
const inputs = JSON.parse(inputsJson);
inputs[6][2].item.detail.output = "x".repeat(70 * 1024);
const events = inputs.map(([parent, provider, event]) => store.apply(parent, provider, event));
const result = {
  events,
  listA: store.list("parent-a"),
  listB: store.list("parent-b"),
  listAll: store.listAll(),
  get: [store.get("parent-a", "child-1"), store.get("parent-a", "nope")],
  fetch: store.fetchTimeline("parent-a", "child-2", { direction: "tail", limit: 2 }),
  deleteA: store.deleteParent("parent-a"),
  afterDelete: store.listAll(),
};
process.stdout.write(JSON.stringify(result));
"#;

fn rust_output() -> String {
    let mut inputs = parse(INPUTS).expect("inputs");
    if let JsValue::Array(rows) = &mut inputs
        && let JsValue::Array(row) = &mut rows[6]
        && let JsValue::Object(event) = &mut row[2]
    {
        let mut item = event.get("item").cloned().expect("item");
        if let JsValue::Object(item_object) = &mut item {
            let mut detail = item_object.get("detail").cloned().expect("detail");
            if let JsValue::Object(detail) = &mut detail {
                detail.insert("output", JsValue::String("x".repeat(70 * 1024)));
            }
            item_object.insert("detail", detail);
        }
        event.insert("item", item);
    }
    let mut store = ProviderSubagentStore::default();
    let events = inputs
        .as_array()
        .expect("inputs")
        .iter()
        .map(|input| {
            let input = input.as_array().expect("input");
            store
                .apply(
                    input[0].as_str().expect("parent"),
                    input[1].as_str().expect("provider"),
                    &input[2],
                )
                .expect("apply")
        })
        .collect();
    let mut out = JsObject::new();
    out.insert("events", JsValue::Array(events));
    out.insert("listA", JsValue::Array(store.list("parent-a")));
    out.insert("listB", JsValue::Array(store.list("parent-b")));
    out.insert("listAll", JsValue::Array(store.list_all()));
    out.insert(
        "get",
        JsValue::Array(vec![
            store.get("parent-a", "child-1").unwrap_or(JsValue::Null),
            store.get("parent-a", "nope").unwrap_or(JsValue::Null),
        ]),
    );
    out.insert(
        "fetch",
        store
            .fetch_timeline("parent-a", "child-2", FetchDirection::Tail, None, Some(2))
            .expect("fetch")
            .to_js(),
    );
    out.insert("deleteA", JsValue::Array(store.delete_parent("parent-a")));
    out.insert("afterDelete", JsValue::Array(store.list_all()));
    stringify(&JsValue::Object(out))
}

/// The wall-clock window of one test run. `Date.now()` and `new Date()` on
/// either side give values inside it; a timestamp a fixture fixes is outside
/// it and is compared exactly.
struct WallClock {
    from_millis: i64,
}

impl WallClock {
    /// Slack either side of the run, for clock reads and process start.
    const SLACK_MILLIS: i64 = 2_000;

    fn start() -> Self {
        Self {
            from_millis: spocky_session::clock::now_millis() - Self::SLACK_MILLIS,
        }
    }

    /// Whether `iso` is a wall-clock value of this run.
    fn contains(&self, iso: &str) -> bool {
        spocky_store::time::parse_iso_millis(iso).is_some_and(|millis| {
            millis >= self.from_millis
                && millis <= spocky_session::clock::now_millis() + Self::SLACK_MILLIS
        })
    }
}

/// Masks the wall-clock timestamps of one run with `<ISO>` and every UUID
/// with `<UUID>`; fixed fixture timestamps stay as they are.
fn normalize(text: &str, clock: &WallClock) -> String {
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
        if iso && clock.contains(&text[index..index + 24]) {
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

#[test]
fn normalize_masks_only_wall_clock_values() {
    let clock = WallClock::start();
    let now = spocky_session::clock::now_iso();
    let text = format!(
        r#"["2026-07-12T10:00:00.000Z","2031-01-02T03:04:05.678Z","{now}","3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b"]"#
    );
    assert_eq!(
        normalize(&text, &clock),
        r#"["2026-07-12T10:00:00.000Z","2031-01-02T03:04:05.678Z","<ISO>","<UUID>"]"#
    );
}

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/provider-subagents/store.js",
        "f72ffa07af18f3251ee361061dc9c8abff8a4fb6a4c09ffedf8214554b0d0902",
    ),
    (
        "server/agent/agent-timeline-store.js",
        "5473e829162d3256ab76b9b39d965158efbf5b4a29bb01e444626ac3a4bf52e3",
    ),
    (
        "server/agent/agent-timeline-content.js",
        "0460bb5811c3d2b953eb535228ff19731942f711804315ac81100ac552191662",
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
fn subagent_store_matches_pinned_build() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: subagent store differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    let clock = WallClock::start();
    assert_pinned_modules(&dist);
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
        .arg(INPUTS)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        normalize(&rust_output(), &clock),
        normalize(&String::from_utf8_lossy(&output.stdout), &clock)
    );
}
