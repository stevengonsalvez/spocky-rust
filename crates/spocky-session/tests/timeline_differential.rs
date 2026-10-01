//! Differential check of the timeline store and projection against the
//! pinned build's `InMemoryAgentTimelineStore`: the same seeds, appends,
//! enrichments and fetches must produce byte-identical raw rows and pages.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;

use spocky_session::timeline::{
    FetchDirection, SeedRow, TimelineCursor, TimelineError, TimelineRow, TimelineSeed,
    TimelineStore,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

/// `(turnId or "", item JSON, providerMessageId or "")`: every merge rule,
/// including tool-call detail and metadata merges, failed and canceled
/// errors, turn boundaries, plugin identity, message ids, a metadata slot
/// that is undefined first, and a submitted message that already has a
/// provider message id when it is enriched.
const APPENDS: &[(&str, &str, &str)] = &[
    (
        "t1",
        r#"{"type":"user_message","text":"hi","clientMessageId":"c1"}"#,
        "",
    ),
    (
        "t1",
        r#"{"type":"user_message","text":"again","clientMessageId":"c2"}"#,
        "pm-0",
    ),
    ("t1", r#"{"type":"assistant_message","text":"Hel"}"#, ""),
    ("t1", r#"{"type":"assistant_message","text":"lo"}"#, ""),
    (
        "t1",
        r#"{"type":"assistant_message","text":" there","messageId":"m2"}"#,
        "",
    ),
    (
        "t1",
        r#"{"type":"assistant_message","text":"!","messageId":"m2"}"#,
        "",
    ),
    ("t1", r#"{"type":"reasoning","text":"think "}"#, ""),
    ("t1", r#"{"type":"reasoning","text":"more"}"#, ""),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call1","name":"shell","status":"running","detail":{"type":"unknown","input":{}},"error":null}"#,
        "",
    ),
    ("t1", r#"{"type":"assistant_message","text":"mid"}"#, ""),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call1","name":"shell","status":"completed","detail":{"type":"shell","command":"ls","output":"a"},"metadata":{"exit":0}}"#,
        "",
    ),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call2","name":"shell","status":"running","detail":{"type":"shell","command":"x"},"metadata":{"a":1}}"#,
        "",
    ),
    (
        "t1",
        r#"{"type":"tool_call","callId":"call2","name":"shell","status":"failed","detail":{"type":"unknown"}}"#,
        "",
    ),
    (
        "t2",
        r#"{"type":"tool_call","callId":"call1","name":"shell","status":"completed","detail":{"type":"shell"}}"#,
        "",
    ),
    (
        "t2",
        r#"{"type":"plugin","pluginId":"p","id":"x","data":1}"#,
        "",
    ),
    (
        "",
        r#"{"type":"plugin","pluginId":"p","id":"x","data":2}"#,
        "",
    ),
    ("t2", r#"{"type":"assistant_message","text":"a"}"#, ""),
    ("t3", r#"{"type":"assistant_message","text":"b"}"#, ""),
    ("t3", r#"{"type":"error","message":"boom"}"#, ""),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call3","name":"read","status":"running","detail":{"type":"unknown"}}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call3","name":"read","status":"canceled","detail":{"type":"unknown"},"error":"x"}"#,
        "",
    ),
    ("t3", r#"{"type":"reasoning","text":"r1"}"#, ""),
    ("t3", r#"{"type":"assistant_message","text":"after"}"#, ""),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call5","name":"edit","status":"running","detail":{"type":"unknown"}}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call5","name":"edit","status":"running","detail":{"type":"edit","path":"f"}}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call5","name":"edit","status":"completed","detail":{"type":"edit","path":"f"},"metadata":{"z":1}}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"todo","items":[{"text":"x","completed":false}]}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"assistant_message","text":"end","messageId":"m9"}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call7","name":"shell","status":"running","detail":{"type":"shell","command":"a"},"metadata":{"a":1}}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call7","name":"shell","status":"completed","detail":{"type":"shell","command":"a"},"metadata":"ab"}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call8","name":"shell","status":"running","detail":{"type":"shell","command":"b"},"metadata":[1]}"#,
        "",
    ),
    (
        "t3",
        r#"{"type":"tool_call","callId":"call8","name":"shell","status":"completed","detail":{"type":"shell","command":"b"},"metadata":{"b":2}}"#,
        "",
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

/// Stored source rows for seeding: a gap before seq 5, turn and provider
/// message ids, assistant chunks that merge, and a tool call lifecycle.
const SEED_ROWS: &str = r#"[
  {"seq":5,"timestamp":"S5","item":{"type":"user_message","text":"q","clientMessageId":"c9"},"turnId":"t1","providerMessageId":"pm9"},
  {"seq":6,"timestamp":"S6","item":{"type":"assistant_message","text":"x"},"turnId":"t1"},
  {"seq":7,"timestamp":"S7","item":{"type":"assistant_message","text":"y"},"turnId":"t1"},
  {"seq":8,"timestamp":"S8","item":{"type":"tool_call","callId":"k","name":"shell","status":"running","detail":{"type":"unknown"}},"turnId":"t1"},
  {"seq":9,"timestamp":"S9","item":{"type":"tool_call","callId":"k","name":"shell","status":"completed","detail":{"type":"shell","command":"ls"}},"turnId":"t1"}
]"#;

/// Items for the items seed path.
const SEED_ITEMS: &str = r#"[
  {"type":"assistant_message","text":"i1"},
  {"type":"assistant_message","text":"i2"},
  {"type":"reasoning","text":"r"}
]"#;

/// One more item appended to every seeded store.
const LATE_ITEM: &str = r#"{"type":"assistant_message","text":"late"}"#;

/// Tool calls whose lifecycle merge reads a missing or null detail, which
/// throws a `TypeError` in the baseline: incoming missing, existing missing,
/// existing null, both missing (the existing detail is read first), then a
/// valid merge and an unrelated item afterwards.
const BROKEN_ITEMS: &str = r#"[
  {"type":"tool_call","callId":"d","name":"shell","status":"running","detail":{"type":"unknown"}},
  {"type":"tool_call","callId":"d","name":"shell","status":"completed"},
  {"type":"tool_call","callId":"e","name":"shell","status":"running"},
  {"type":"tool_call","callId":"e","name":"shell","status":"completed","detail":{"type":"shell"}},
  {"type":"tool_call","callId":"f","name":"shell","status":"running","detail":null},
  {"type":"tool_call","callId":"f","name":"shell","status":"completed","detail":{"type":"shell"}},
  {"type":"tool_call","callId":"h","name":"shell","status":"running"},
  {"type":"tool_call","callId":"h","name":"shell","status":"completed","detail":null},
  {"type":"tool_call","callId":"d","name":"shell","status":"completed","detail":{"type":"shell"}},
  {"type":"assistant_message","text":"after"}
]"#;

/// Seed rows whose second row throws while `initialize` projects them.
const BROKEN_SEED_ROWS: &str = r#"[
  {"seq":1,"timestamp":"G1","item":{"type":"tool_call","callId":"g","name":"shell","status":"running","detail":{"type":"shell"}}},
  {"seq":2,"timestamp":"G2","item":{"type":"tool_call","callId":"g","name":"shell","status":"completed"}}
]"#;

/// The pinned store, driven through the same calls. Every result is written
/// as the raw object the store returns, so `seq`, key order and the enriched
/// `providerMessageId` are compared byte for byte. Epochs and timestamps are
/// fixed inputs, so nothing is normalized.
const NODE_SCRIPT: &str = r#"
const [dist, appendsJson, fetchesJson, seedRowsJson, seedItemsJson, lateItemJson, brokenJson, brokenSeedJson] = process.argv.slice(1);
const { InMemoryAgentTimelineStore } = await import(`${dist}/server/agent/agent-timeline-store.js`);
const store = new InMemoryAgentTimelineStore();
const fetchAll = (agentId) => JSON.parse(fetchesJson).map(([direction, epoch, seq, limit]) =>
  store.fetch(agentId, { direction, ...(epoch ? { cursor: { epoch, seq } } : {}), ...(limit >= 0 ? { limit } : {}) }));
const report = (agentId, late) => ({
  late: late ? store.append(agentId, JSON.parse(lateItemJson), { timestamp: "TL", turnId: "t1" }) : null,
  rows: store.getRows(agentId),
  fetches: fetchAll(agentId),
});
store.initialize("empty", { epoch: "E", timestamp: "T0" });
const empty = report("empty", false);
store.initialize("a", { epoch: "E", timestamp: "T0" });
const appended = JSON.parse(appendsJson).map(([turnId, item, providerMessageId], index) =>
  store.append("a", JSON.parse(item), {
    timestamp: `T${index + 1}`,
    ...(turnId ? { turnId } : {}),
    ...(providerMessageId ? { providerMessageId } : {}),
  }));
const main = report("a", false);
const enriched = [
  store.enrichSubmittedUserMessage("a", "c1", "provider-1"),
  store.enrichSubmittedUserMessage("a", "c2", "provider-2"),
  store.enrichSubmittedUserMessage("a", "missing", "provider-3"),
];
const afterEnrich = report("a", false);
store.initialize("rows", { epoch: "E", nextSeq: 3, timestamp: "TS", rows: JSON.parse(seedRowsJson), items: JSON.parse(seedItemsJson) });
store.initialize("gap", { epoch: "E", nextSeq: 20, rows: JSON.parse(seedRowsJson) });
store.initialize("items", { epoch: "E", nextSeq: 4, timestamp: "TI", items: JSON.parse(seedItemsJson) });
store.initialize("projected", { epoch: "E", rows: store.getRows("a") });
const attempt = (run) => {
  try {
    return { ok: run() };
  } catch (error) {
    return { name: error.constructor.name, message: error.message };
  }
};
store.initialize("broken", { epoch: "E", timestamp: "TB" });
const brokenAppends = JSON.parse(brokenJson).map((item, index) =>
  attempt(() => store.append("broken", item, { timestamp: `B${index}`, turnId: "t1" })));
const broken = { appends: brokenAppends, ...report("broken", false) };
const seedFailure = attempt(() => store.initialize("seedbroken", { epoch: "E", rows: JSON.parse(brokenSeedJson) }));
store.initialize("keep", { epoch: "K", timestamp: "TK", items: JSON.parse(seedItemsJson) });
const keepFailure = attempt(() => store.initialize("keep", { epoch: "K2", rows: JSON.parse(brokenSeedJson) }));
const keep = { failure: keepFailure, epoch: store.getEpoch("keep"), ...report("keep", false) };
process.stdout.write(JSON.stringify({
  empty,
  appended,
  main,
  enriched,
  afterEnrich,
  seeded: ["rows", "gap", "items", "projected"].map((agentId) => report(agentId, true)),
  lastItem: store.getLastItem("a"),
  lastAssistant: store.getLastAssistantMessage("a"),
  submitted: store.getSubmittedUserMessage("a", "c1"),
  broken,
  seedFailure,
  seedFailureStored: store.has("seedbroken"),
  keep,
}));
"#;

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

#[allow(
    clippy::cast_precision_loss,
    reason = "sequence numbers stay far below 2^53"
)]
fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "fixture sequence numbers are small integers"
)]
fn integer(value: &JsValue) -> i64 {
    value.as_f64().expect("number") as i64
}

fn optional_string(value: Option<&JsValue>) -> Option<String> {
    value.and_then(JsValue::as_str).map(str::to_owned)
}

fn report(store: &mut TimelineStore, agent_id: &str, late: bool) -> JsValue {
    let late = if late {
        let row = store
            .append(
                agent_id,
                parse(LATE_ITEM).expect("late item"),
                Some("TL".to_owned()),
                Some("t1".to_owned()),
                None,
            )
            .expect("agent timeline exists");
        row.to_js()
    } else {
        JsValue::Null
    };
    let rows = store
        .rows(agent_id)
        .expect("timeline")
        .iter()
        .map(|row| row.to_js(false))
        .collect();
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
            store
                .fetch(agent_id, direction, cursor.as_ref(), limit)
                .expect("agent timeline exists")
                .to_js()
        })
        .collect();
    let mut output = JsObject::new();
    output.insert("late", late);
    output.insert("rows", JsValue::Array(rows));
    output.insert("fetches", JsValue::Array(fetches));
    JsValue::Object(output)
}

fn seed_rows() -> Vec<SeedRow> {
    parse(SEED_ROWS)
        .expect("seed rows")
        .as_array()
        .expect("array")
        .iter()
        .map(|row| {
            SeedRow::Source(TimelineRow {
                seq: integer(row.get("seq").expect("seq")),
                timestamp: optional_string(row.get("timestamp")).expect("timestamp"),
                item: row.get("item").expect("item").clone(),
                turn_id: optional_string(row.get("turnId")),
                provider_message_id: optional_string(row.get("providerMessageId")),
            })
        })
        .collect()
}

fn seed_items() -> Vec<JsValue> {
    parse(SEED_ITEMS)
        .expect("seed items")
        .as_array()
        .expect("array")
        .to_vec()
}

fn seed(
    rows: Vec<SeedRow>,
    items: Vec<JsValue>,
    next_seq: Option<i64>,
    timestamp: Option<&str>,
) -> TimelineSeed {
    TimelineSeed {
        items,
        rows,
        epoch: Some("E".to_owned()),
        next_seq,
        timestamp: timestamp.map(str::to_owned),
    }
}

/// `{ ok: value }` for a call that returned, else `{ name, message }` of
/// the `TypeError`; an `undefined` result writes `{}`.
fn attempt_value(outcome: Result<Option<JsValue>, TimelineError>) -> JsValue {
    let mut value = JsObject::new();
    match outcome {
        Ok(Some(ok)) => value.insert("ok", ok),
        Ok(None) => {}
        Err(TimelineError::Type(error)) => {
            value.insert("name", text("TypeError"));
            value.insert("message", text(&error.0));
        }
        Err(TimelineError::UnknownAgent(error)) => panic!("unexpected {error}"),
    }
    JsValue::Object(value)
}

fn broken_output(store: &mut TimelineStore) -> (JsValue, JsValue) {
    store
        .initialize_with("broken", seed(Vec::new(), Vec::new(), None, Some("TB")))
        .expect("seed");
    let appends = parse(BROKEN_ITEMS)
        .expect("broken items")
        .as_array()
        .expect("array")
        .iter()
        .enumerate()
        .map(|(index, item)| {
            attempt_value(
                store
                    .append(
                        "broken",
                        item.clone(),
                        Some(format!("B{index}")),
                        Some("t1".to_owned()),
                        None,
                    )
                    .map(|row| Some(row.to_js())),
            )
        })
        .collect();
    let report = report(store, "broken", false);
    let JsValue::Object(report) = &report else {
        unreachable!("report is an object")
    };
    let mut broken = JsObject::new();
    broken.insert("appends", JsValue::Array(appends));
    for (key, value) in report.iter() {
        broken.insert(key, value.clone());
    }
    let seed_failure = attempt_value(
        store
            .initialize_with(
                "seedbroken",
                seed(broken_seed_rows(), Vec::new(), None, None),
            )
            .map(|()| None)
            .map_err(TimelineError::Type),
    );
    (JsValue::Object(broken), seed_failure)
}

fn broken_seed_rows() -> Vec<SeedRow> {
    parse(BROKEN_SEED_ROWS)
        .expect("broken seed")
        .as_array()
        .expect("array")
        .iter()
        .map(|row| {
            SeedRow::Source(TimelineRow {
                seq: integer(row.get("seq").expect("seq")),
                timestamp: optional_string(row.get("timestamp")).expect("timestamp"),
                item: row.get("item").expect("item").clone(),
                turn_id: None,
                provider_message_id: None,
            })
        })
        .collect()
}

/// A failed reseed of an agent that has a timeline keeps its epoch and rows.
fn keep_output(store: &mut TimelineStore) -> JsValue {
    let mut first = seed(Vec::new(), seed_items(), None, Some("TK"));
    first.epoch = Some("K".to_owned());
    store.initialize_with("keep", first).expect("seed");
    let mut reseed = seed(broken_seed_rows(), Vec::new(), None, None);
    reseed.epoch = Some("K2".to_owned());
    let failure = attempt_value(
        store
            .initialize_with("keep", reseed)
            .map(|()| None)
            .map_err(TimelineError::Type),
    );
    let report = report(store, "keep", false);
    let JsValue::Object(report) = &report else {
        unreachable!("report is an object")
    };
    let mut keep = JsObject::new();
    keep.insert("failure", failure);
    keep.insert("epoch", text(store.epoch("keep").expect("timeline")));
    for (key, value) in report.iter() {
        keep.insert(key, value.clone());
    }
    JsValue::Object(keep)
}

fn rust_output() -> String {
    let mut store = TimelineStore::default();
    store
        .initialize_with("empty", seed(Vec::new(), Vec::new(), None, Some("T0")))
        .expect("seed");
    let empty = report(&mut store, "empty", false);
    store
        .initialize_with("a", seed(Vec::new(), Vec::new(), None, Some("T0")))
        .expect("seed");
    let appended = APPENDS
        .iter()
        .enumerate()
        .map(|(index, (turn, item, provider_message_id))| {
            let row = store
                .append(
                    "a",
                    parse(item).expect("item JSON"),
                    Some(format!("T{}", index + 1)),
                    (!turn.is_empty()).then(|| (*turn).to_owned()),
                    (!provider_message_id.is_empty()).then(|| (*provider_message_id).to_owned()),
                )
                .expect("agent timeline exists");
            row.to_js()
        })
        .collect();
    let main = report(&mut store, "a", false);
    let enriched = [
        ("c1", "provider-1"),
        ("c2", "provider-2"),
        ("missing", "provider-3"),
    ]
    .iter()
    .map(|(client, provider)| {
        store
            .enrich_submitted_user_message("a", client, provider)
            .expect("timeline")
            .map_or(JsValue::Null, |row| row.to_js(false))
    })
    .collect();
    let after_enrich = report(&mut store, "a", false);
    store
        .initialize_with("rows", seed(seed_rows(), seed_items(), Some(3), Some("TS")))
        .expect("seed");
    store
        .initialize_with("gap", seed(seed_rows(), Vec::new(), Some(20), None))
        .expect("seed");
    store
        .initialize_with("items", seed(Vec::new(), seed_items(), Some(4), Some("TI")))
        .expect("seed");
    let projected = store
        .rows("a")
        .expect("timeline")
        .iter()
        .cloned()
        .map(SeedRow::Projected)
        .collect();
    store
        .initialize_with("projected", seed(projected, Vec::new(), None, None))
        .expect("seed");
    let seeded = ["rows", "gap", "items", "projected"]
        .iter()
        .map(|agent_id| report(&mut store, agent_id, true))
        .collect();
    let mut output = JsObject::new();
    output.insert("empty", empty);
    output.insert("appended", JsValue::Array(appended));
    output.insert("main", main);
    output.insert("enriched", JsValue::Array(enriched));
    output.insert("afterEnrich", after_enrich);
    output.insert("seeded", JsValue::Array(seeded));
    let (broken, seed_failure) = broken_output(&mut store);
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
            .map_or(JsValue::Null, |row| row.to_js(false)),
    );
    output.insert("broken", broken);
    output.insert("seedFailure", seed_failure);
    output.insert("seedFailureStored", JsValue::Bool(store.has("seedbroken")));
    output.insert("keep", keep_output(&mut store));
    stringify(&JsValue::Object(output))
}

fn json_list<T>(rows: &[T], encode: impl Fn(&T) -> JsValue) -> String {
    stringify(&JsValue::Array(rows.iter().map(encode).collect()))
}

/// The pinned dist modules this test runs, relative to `SPOCKY_PASEO_DIST`,
/// with their SHA-256: a different build fails instead of silently passing.
const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/agent-timeline-store.js",
        "5473e829162d3256ab76b9b39d965158efbf5b4a29bb01e444626ac3a4bf52e3",
    ),
    (
        "server/agent/timeline-projection.js",
        "2e8dc2a93535f4971250c837fb864694750acc8834b5e3a368f3e533ddb9d800",
    ),
];

fn assert_pinned_modules(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    for (path, expected) in PINNED_MODULES {
        let bytes = std::fs::read(std::path::Path::new(dist).join(path)).expect("pinned module");
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
    assert_pinned_modules(&dist);
    let appends = json_list(APPENDS, |(turn, item, provider_message_id)| {
        JsValue::Array(vec![text(turn), text(item), text(provider_message_id)])
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
        .args([
            SEED_ROWS,
            SEED_ITEMS,
            LATE_ITEM,
            BROKEN_ITEMS,
            BROKEN_SEED_ROWS,
        ])
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
