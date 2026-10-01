//! Differential check of the timeline store and projection against the
//! pinned build's `InMemoryAgentTimelineStore`: the same appends and fetches
//! must produce byte-identical pages.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;

use spocky_session::timeline::{
    FetchDirection, ProjectedRow, TimelineCursor, TimelineFetch, TimelineStore,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// `(turnId or "", item JSON)`: every merge rule, including tool-call detail
/// and metadata merges, failed and canceled errors, turn boundaries, plugin
/// identity, message ids, and a metadata slot that is undefined first.
const APPENDS: &[(&str, &str)] = &[
    (
        "t1",
        r#"{"type":"user_message","text":"hi","clientMessageId":"c1"}"#,
    ),
    ("t1", r#"{"type":"assistant_message","text":"Hel"}"#),
    ("t1", r#"{"type":"assistant_message","text":"lo"}"#),
    (
        "t1",
        r#"{"type":"assistant_message","text":" there","messageId":"m2"}"#,
    ),
    (
        "t1",
        r#"{"type":"assistant_message","text":"!","messageId":"m2"}"#,
    ),
    ("t1", r#"{"type":"reasoning","text":"think "}"#),
    ("t1", r#"{"type":"reasoning","text":"more"}"#),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call1","name":"shell","status":"running","detail":{"type":"unknown","input":{}},"error":null}"#,
    ),
    ("t1", r#"{"type":"assistant_message","text":"mid"}"#),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call1","name":"shell","status":"completed","detail":{"type":"shell","command":"ls","output":"a"},"metadata":{"exit":0}}"#,
    ),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call2","name":"shell","status":"running","detail":{"type":"shell","command":"x"},"metadata":{"a":1}}"#,
    ),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call2","name":"shell","status":"failed","detail":{"type":"unknown"}}"#,
    ),
    (
        "t2",
        r#"{"type":"tool_call","callId":"call1","name":"shell","status":"completed","detail":{"type":"shell"}}"#,
    ),
    (
        "t2",
        r#"{"type":"plugin","pluginId":"p","id":"x","data":1}"#,
    ),
    ("", r#"{"type":"plugin","pluginId":"p","id":"x","data":2}"#),
    ("t2", r#"{"type":"assistant_message","text":"a"}"#),
    ("t3", r#"{"type":"assistant_message","text":"b"}"#),
    ("t3", r#"{"type":"error","message":"boom"}"#),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call3","name":"read","status":"running","detail":{"type":"unknown"}}"#,
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call3","name":"read","status":"canceled","detail":{"type":"unknown"},"error":"x"}"#,
    ),
    ("t3", r#"{"type":"reasoning","text":"r1"}"#),
    ("t3", r#"{"type":"assistant_message","text":"after"}"#),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call5","name":"edit","status":"running","detail":{"type":"unknown"}}"#,
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call5","name":"edit","status":"running","detail":{"type":"edit","path":"f"}}"#,
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call5","name":"edit","status":"completed","detail":{"type":"edit","path":"f"},"metadata":{"z":1}}"#,
    ),
    (
        "t3",
        r#"{"type":"todo","items":[{"text":"x","completed":false}]}"#,
    ),
    (
        "t3",
        r#"{"type":"assistant_message","text":"end","messageId":"m9"}"#,
    ),
];

/// `(direction, cursor epoch or "", cursor seq, limit)`; `-1` limit means none.
const FETCHES: &[(&str, &str, i64, i64)] = &[
    ("tail", "", 0, 0),
    ("tail", "", 0, 3),
    ("tail", "", 0, 1),
    ("tail", "", 0, -1),
    ("after", "E", 0, 0),
    ("after", "E", 5, 2),
    ("after", "E", 10, 0),
    ("after", "E", 100, 0),
    ("before", "E", 20, 3),
    ("before", "E", 2, 5),
    ("before", "E", 1, 0),
    ("after", "other-epoch", 3, 0),
    ("after", "E", -5, 2),
];

const NODE_SCRIPT: &str = r#"
const [dist, appendsJson, fetchesJson] = process.argv.slice(1);
const { InMemoryAgentTimelineStore } = await import(`${dist}/server/agent/agent-timeline-store.js`);
const store = new InMemoryAgentTimelineStore();
store.initialize("a", { epoch: "E", timestamp: "T0" });
JSON.parse(appendsJson).forEach(([turnId, item], index) => {
  store.append("a", JSON.parse(item), { timestamp: `T${index + 1}`, ...(turnId ? { turnId } : {}) });
});
const row = (entry) => ({
  item: entry.item, turnId: entry.turnId, providerMessageId: entry.providerMessageId,
  timestamp: entry.timestamp, seqStart: entry.seqStart, seqEnd: entry.seqEnd,
  sourceSeqRanges: entry.sourceSeqRanges, collapsed: entry.collapsed,
});
const fetches = JSON.parse(fetchesJson).map(([direction, epoch, seq, limit]) => {
  const page = store.fetch("a", { direction, ...(epoch ? { cursor: { epoch, seq } } : {}), ...(limit >= 0 ? { limit } : {}) });
  return { ...page, rows: page.rows.map(row) };
});
store.enrichSubmittedUserMessage("a", "c1", "provider-1");
process.stdout.write(JSON.stringify({
  fetches,
  lastItem: store.getLastItem("a"),
  lastAssistant: store.getLastAssistantMessage("a"),
  submitted: row(store.getSubmittedUserMessage("a", "c1")),
}));
"#;

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn optional_text(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Undefined, |value| text(value))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "sequence numbers stay far below 2^53"
)]
fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
}

fn row_value(entry: &ProjectedRow) -> JsValue {
    let mut row = JsObject::new();
    row.insert("item", entry.item.clone());
    row.insert("turnId", optional_text(entry.turn_id.as_ref()));
    row.insert(
        "providerMessageId",
        optional_text(entry.provider_message_id.as_ref()),
    );
    row.insert("timestamp", text(&entry.timestamp));
    row.insert("seqStart", number(entry.seq_start));
    row.insert("seqEnd", number(entry.seq_end));
    row.insert(
        "sourceSeqRanges",
        JsValue::Array(
            entry
                .source_seq_ranges
                .iter()
                .map(|range| {
                    let mut value = JsObject::new();
                    value.insert("startSeq", number(range.start_seq));
                    value.insert("endSeq", number(range.end_seq));
                    JsValue::Object(value)
                })
                .collect(),
        ),
    );
    row.insert(
        "collapsed",
        JsValue::Array(
            entry
                .collapsed
                .iter()
                .map(|kind| text(kind.as_str()))
                .collect(),
        ),
    );
    JsValue::Object(row)
}

fn fetch_value(fetch: &TimelineFetch) -> JsValue {
    let direction = match fetch.direction {
        FetchDirection::Tail => "tail",
        FetchDirection::Before => "before",
        FetchDirection::After => "after",
    };
    let optional_number = |value: Option<i64>| value.map_or(JsValue::Null, number);
    let mut window = JsObject::new();
    window.insert("minSeq", number(fetch.window.min_seq));
    window.insert("maxSeq", number(fetch.window.max_seq));
    window.insert("nextSeq", number(fetch.window.next_seq));
    let mut page = JsObject::new();
    page.insert("epoch", text(&fetch.epoch));
    page.insert("direction", text(direction));
    page.insert("reset", JsValue::Bool(fetch.reset));
    page.insert("staleCursor", JsValue::Bool(fetch.stale_cursor));
    page.insert("gap", JsValue::Bool(fetch.gap));
    page.insert("window", JsValue::Object(window));
    page.insert("hasOlder", JsValue::Bool(fetch.has_older));
    page.insert("hasNewer", JsValue::Bool(fetch.has_newer));
    page.insert("startSeq", optional_number(fetch.start_seq));
    page.insert("endSeq", optional_number(fetch.end_seq));
    page.insert(
        "rows",
        JsValue::Array(fetch.rows.iter().map(row_value).collect()),
    );
    JsValue::Object(page)
}

fn rust_output() -> String {
    let mut store = TimelineStore::default();
    store.initialize(
        "a",
        Vec::new(),
        Some("E".to_owned()),
        None,
        Some("T0".to_owned()),
    );
    for (index, (turn, item)) in APPENDS.iter().enumerate() {
        store
            .append(
                "a",
                parse(item).expect("item JSON"),
                Some(format!("T{}", index + 1)),
                (!turn.is_empty()).then(|| (*turn).to_owned()),
                None,
            )
            .expect("agent timeline exists");
    }
    let fetches = FETCHES
        .iter()
        .map(|(direction, epoch, seq, limit)| {
            let direction = match *direction {
                "tail" => FetchDirection::Tail,
                "before" => FetchDirection::Before,
                _ => FetchDirection::After,
            };
            let cursor = (!epoch.is_empty()).then(|| TimelineCursor {
                epoch: (*epoch).to_owned(),
                seq: *seq,
            });
            let limit = usize::try_from(*limit).ok();
            fetch_value(
                &store
                    .fetch("a", direction, cursor.as_ref(), limit)
                    .expect("agent timeline exists"),
            )
        })
        .collect();
    store
        .enrich_submitted_user_message("a", "c1", "provider-1")
        .expect("agent timeline exists");
    let mut output = JsObject::new();
    output.insert("fetches", JsValue::Array(fetches));
    output.insert(
        "lastItem",
        store
            .last_item("a")
            .expect("timeline")
            .unwrap_or(JsValue::Null),
    );
    output.insert(
        "lastAssistant",
        store
            .last_assistant_message("a")
            .expect("timeline")
            .map_or(JsValue::Null, |message| text(&message)),
    );
    output.insert(
        "submitted",
        store
            .submitted_user_message("a", "c1")
            .expect("timeline")
            .map_or(JsValue::Null, |row| row_value(&row)),
    );
    stringify(&JsValue::Object(output))
}

fn json_list<T>(rows: &[T], encode: impl Fn(&T) -> JsValue) -> String {
    stringify(&JsValue::Array(rows.iter().map(encode).collect()))
}

#[test]
fn timeline_pages_match_pinned_store() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: timeline differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    let appends = json_list(APPENDS, |(turn, item)| {
        JsValue::Array(vec![text(turn), text(item)])
    });
    let fetches = json_list(FETCHES, |(direction, epoch, seq, limit)| {
        JsValue::Array(vec![
            text(direction),
            text(epoch),
            number(*seq),
            number(*limit),
        ])
    });
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
        .arg(&appends)
        .arg(&fetches)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
