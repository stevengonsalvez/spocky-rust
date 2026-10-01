//! Expected values come from `STORED_AGENT_SCHEMA.safeParse(JSON.parse(input))`
//! run with zod 4.4.3 (the version pinned in Paseo `package-lock.json`) on the
//! schema text copied verbatim from `agent/agent-storage.ts` at `5de45e2`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_store::agent_record::{AgentRecordStore, parse_stored_agent_record};
use spocky_store::js_value::{JsValue, parse, stringify, stringify_pretty};

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-store-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create disposable agent store");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove disposable agent store");
    }
}

/// Each case in `tests/fixtures/agent-zod.json` was produced by
/// `tests/oracle/zod-oracle.sh tests/oracle/agent-cases.json`: node runs
/// `JSON.stringify(STORED_AGENT_SCHEMA.parse(JSON.parse(input)), null, 2)`
/// with the pinned zod and the schema text copied from `agent-storage.ts`.
/// `output` is null where the baseline parse throws.
#[test]
fn stored_agent_schema_matches_zod_oracle() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent-zod.json");
    let fixture = parse(&fs::read_to_string(fixture_path).expect("read oracle fixture"))
        .expect("fixture is JSON");
    let cases = fixture.as_array().expect("fixture is an array of cases");
    assert_eq!(cases.len(), 20, "every oracle case is checked");
    for case in cases {
        let field = |key: &str| {
            case.get(key)
                .and_then(JsValue::as_str)
                .expect("string field")
        };
        let (name, input) = (field("name"), field("input"));
        // Fixture strings are JavaScript text; stringify output holds no lone
        // surrogate, so only a literal U+10FFFF needs decoding.
        let expected = case
            .get("output")
            .and_then(JsValue::as_str)
            .map(|text| text.replace("\u{10FFFF}\u{10FFFF}", "\u{10FFFF}"));
        let actual = parse(input)
            .ok()
            .and_then(|value| parse_stored_agent_record(&value).ok())
            .map(|parsed| stringify_pretty(&parsed));
        assert_eq!(actual, expected, "case {name}");
    }
}

fn record(id: &str, cwd: &str, status: &str) -> JsValue {
    parse(&format!(
        r#"{{"id":"{id}","provider":"codex","cwd":"{cwd}","createdAt":"2026-10-01T10:00:00.000Z","updatedAt":"2026-10-01T10:00:00.000Z","lastStatus":"{status}","internal":false}}"#
    ))
    .expect("record literal is JSON")
}

#[test]
fn scan_loads_root_and_nested_files_and_skips_invalid_ones() {
    let home = TestDir::new("agent-scan");
    let nested = home.path().join("tmp-project");
    fs::create_dir_all(nested.join("deeper")).expect("create nested buckets");
    fs::write(
        home.path().join("root.json"),
        stringify(&record("root", "/x", "idle")),
    )
    .expect("seed root record");
    fs::write(
        nested.join("nested.json"),
        stringify(&record("nested", "/tmp/project", "closed")),
    )
    .expect("seed nested record");
    fs::write(nested.join("broken.json"), "{not json").expect("seed broken record");
    fs::write(
        nested.join("bad-status.json"),
        stringify(&record("bad", "/tmp/project", "paused")),
    )
    .expect("seed invalid status");
    fs::write(
        nested.join("deeper").join("too-deep.json"),
        stringify(&record("deep", "/tmp/project", "idle")),
    )
    .expect("seed record two levels down");
    fs::write(nested.join("notes.txt"), "{}").expect("seed non-json file");

    let mut store = AgentRecordStore::new(home.path());
    let mut ids = store
        .list()
        .iter()
        .map(|value| {
            value
                .get("id")
                .and_then(JsValue::as_str)
                .expect("id")
                .to_owned()
        })
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["nested", "root"]);
    assert_eq!(
        store.skipped().len(),
        2,
        "broken and bad-status are skipped"
    );
    assert_eq!(
        store.list()[0].get("id").and_then(JsValue::as_str),
        Some("root"),
        "root files load before buckets"
    );
}

#[test]
fn write_uses_json_stringify_layout_and_moves_file_on_cwd_change() {
    let home = TestDir::new("agent-write");
    let mut store = AgentRecordStore::new(home.path());
    let first = store
        .write(record("agent-1", "/tmp/project", "idle"))
        .expect("write record")
        .expect("not deleting");
    assert!(first.ends_with("tmp-project/agent-1.json"));
    assert_eq!(
        fs::read_to_string(&first).expect("read record"),
        "{\n  \"id\": \"agent-1\",\n  \"provider\": \"codex\",\n  \"cwd\": \"/tmp/project\",\n  \"createdAt\": \"2026-10-01T10:00:00.000Z\",\n  \"updatedAt\": \"2026-10-01T10:00:00.000Z\",\n  \"lastStatus\": \"idle\",\n  \"internal\": false\n}"
    );

    let moved = store
        .write(record("agent-1", "/tmp/other", "closed"))
        .expect("rewrite under new cwd")
        .expect("not deleting");
    assert!(moved.ends_with("tmp-other/agent-1.json"));
    assert!(!first.exists(), "old cwd bucket file is unlinked");
    assert_eq!(store.list().len(), 1);

    let mut reopened = AgentRecordStore::new(home.path());
    assert_eq!(
        reopened
            .get("agent-1")
            .expect("record survives restart")
            .get("lastStatus")
            .and_then(JsValue::as_str),
        Some("closed")
    );
    assert!(reopened.remove("agent-1").is_empty(), "no unlink failures");
    assert!(!moved.exists());
    assert!(reopened.get("agent-1").is_none());
}

#[test]
fn javascript_only_record_loads_and_rewrites_like_node() {
    // Fixture case "javascript-only-input" pins the zod output for this text.
    let input = r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","title":"split \ud83d","persistence":{"provider":"codex","sessionId":"s","metadata":{"big":1e400,"n":12345678901234567890},"nativeHandle":[[[[]]]]}}"#;

    let home = TestDir::new("agent-js-input");
    let bucket = home.path().join("p");
    fs::create_dir_all(&bucket).expect("create bucket");
    fs::write(bucket.join("a.json"), input).expect("seed record");
    let mut store = AgentRecordStore::new(home.path());
    assert!(store.skipped().is_empty());
    let loaded = store.get("a").expect("record is not skipped");
    let path = store
        .write(loaded)
        .expect("rewrite record")
        .expect("not deleting");
    let written = fs::read_to_string(path).expect("read record");
    assert!(written.contains(r#""title": "split \ud83d","#), "{written}");
}

#[test]
fn scan_follows_sorted_readdir_order_and_last_duplicate_wins() {
    let home = TestDir::new("agent-sorted-scan");
    for bucket in ["b-bucket", "a-bucket"] {
        fs::create_dir_all(home.path().join(bucket)).expect("create bucket");
    }
    // libuv scandir sorts names by bytes: "B.json" < "a.json" < "c.json".
    fs::write(
        home.path().join("c.json"),
        stringify(&record("root-c", "/c", "idle")),
    )
    .expect("seed root c");
    fs::write(
        home.path().join("B.json"),
        stringify(&record("root-b", "/b", "idle")),
    )
    .expect("seed root B");
    fs::write(
        home.path().join("b-bucket").join("dup.json"),
        stringify(&record("dup", "/second", "closed")),
    )
    .expect("seed later duplicate");
    fs::write(
        home.path().join("a-bucket").join("dup.json"),
        stringify(&record("dup", "/first", "idle")),
    )
    .expect("seed earlier duplicate");

    let mut store = AgentRecordStore::new(home.path());
    let listed = store
        .list()
        .iter()
        .map(|value| {
            (
                value
                    .get("id")
                    .and_then(JsValue::as_str)
                    .expect("id")
                    .to_owned(),
                value
                    .get("cwd")
                    .and_then(JsValue::as_str)
                    .expect("cwd")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        vec![
            ("root-b".to_owned(), "/b".to_owned()),
            ("root-c".to_owned(), "/c".to_owned()),
            ("dup".to_owned(), "/second".to_owned()),
        ],
        "Map.set keeps the first position and the last value"
    );
}

#[test]
fn delete_tombstone_skips_later_writes_and_unlink_errors_do_not_fail() {
    let home = TestDir::new("agent-delete");
    let mut store = AgentRecordStore::new(home.path());
    let written = store
        .write(record("agent-1", "/tmp/project", "idle"))
        .expect("write")
        .expect("not deleting");
    assert!(!store.is_deleting("agent-1"));
    assert!(store.remove("agent-1").is_empty());
    assert!(store.is_deleting("agent-1"));
    assert!(!store.is_deleting("agent-2"));
    assert!(!written.exists());
    assert_eq!(
        store
            .write(record("agent-1", "/tmp/project", "idle"))
            .expect("write after delete"),
        None,
        "the deleting tombstone outlives the delete"
    );
    assert!(!written.exists());

    // A directory where the record file should be makes unlink fail; the
    // baseline logs it and still finishes the delete.
    let blocked = home.path().join("tmp-blocked");
    let mut other = AgentRecordStore::new(home.path());
    let path = other
        .write(record("agent-2", "/tmp/blocked", "idle"))
        .expect("write")
        .expect("not deleting");
    fs::remove_file(&path).expect("remove file");
    fs::create_dir_all(path.join("child")).expect("replace file with directory");
    let failures = other.remove("agent-2");
    assert_eq!(failures.len(), 1, "unlink of a directory fails");
    assert!(other.get("agent-2").is_none());
    assert!(blocked.exists());
}

#[test]
fn rejections_name_the_failing_field() {
    let field = |input: &str| {
        parse_stored_agent_record(&parse(input).expect("JSON"))
            .expect_err("zod rejects")
            .field
    };
    assert_eq!(
        field(
            r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","lastStatus":null}"#
        ),
        "lastStatus"
    );
    assert_eq!(
        field(r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c"}"#),
        "updatedAt"
    );
}

/// DIV-001 family: zod parses `z.json()` recursively and throws a `RangeError`
/// near 10,000 levels, so the baseline skips this record at load. The port
/// walks iteratively and loads it. Recorded divergence, pinned here.
#[test]
fn deep_provider_options_load_beyond_the_zod_recursion_limit() {
    let depth = 20_000;
    let nested = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    let input = format!(
        r#"{{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","config":{{"providerOptions":{{"deep":{nested}}}}}}}"#
    );
    let parsed = parse_stored_agent_record(&parse(&input).expect("JSON.parse accepts"))
        .expect("the port loads what the baseline skips");
    assert!(
        parsed
            .get("config")
            .and_then(|config| config.get("providerOptions"))
            .and_then(|options| options.get("deep"))
            .is_some()
    );
}
