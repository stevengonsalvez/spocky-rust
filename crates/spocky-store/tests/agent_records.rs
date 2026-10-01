//! Expected values come from `STORED_AGENT_SCHEMA.safeParse(JSON.parse(input))`
//! run with zod 4.4.3 (the version pinned in Paseo `package-lock.json`) on the
//! schema text copied verbatim from `agent/agent-storage.ts` at `5de45e2`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_store::agent_record::{AgentRecordStore, parse_stored_agent_record};
use spocky_store::js_json::js_property_order;

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

fn zod(input: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(input).expect("test input is JSON");
    parse_stored_agent_record(&js_property_order(value))
        .map(|parsed| serde_json::to_string(&parsed).expect("render"))
        .map_err(|error| error.field.to_owned())
}

#[test]
fn schema_output_order_strips_unknown_keys_and_keeps_js_property_order() {
    let input = r#"{"zzz":1,"lastStatus":"idle","updatedAt":"u","createdAt":"c","cwd":"/p","provider":"codex","id":"a","owner":{"executionId":"e","kind":"daemon","daemonId":"d"},"archivedAt":null,"internal":false,"labels":{"b":"1","2":"x"},"persistence":{"metadata":{"k":1,"0":2},"sessionId":"s","provider":"codex","nativeHandle":"n","junk":1},"config":{"model":null,"modeId":"full-access","other":1},"runtimeInfo":{"sessionId":null,"provider":"codex","extra":{}}}"#;
    assert_eq!(
        zod(input).expect("valid record"),
        r#"{"id":"a","provider":"codex","cwd":"/p","createdAt":"c","updatedAt":"u","labels":{"2":"x","b":"1"},"lastStatus":"idle","config":{"modeId":"full-access","model":null},"runtimeInfo":{"provider":"codex","sessionId":null,"extra":{}},"persistence":{"provider":"codex","sessionId":"s","nativeHandle":"n","metadata":{"0":2,"k":1}},"internal":false,"archivedAt":null,"owner":{"kind":"daemon","daemonId":"d","executionId":"e"}}"#
    );
}

#[test]
fn schema_defaults_and_nested_shapes() {
    assert_eq!(
        zod(r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u"}"#)
            .expect("minimal record"),
        r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","labels":{},"lastStatus":"closed"}"#
    );
    let input = r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","features":[{"options":[{"metadata":{"z":1},"label":"L","id":"o","x":1}],"value":null,"label":"F","id":"f","type":"select","icon":"i"},{"type":"toggle","value":true,"id":"t","label":"T"}],"config":{"toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"}]},"systemPrompt":null,"mcpServers":{"m":{"command":"x"}}},"attentionReason":null,"requiresAttention":true,"lastError":null,"lastModeId":null,"title":null,"lastUserMessageAt":null,"lastActivityAt":"l","workspaceId":"w"}"#;
    assert_eq!(
        zod(input).expect("full record"),
        r#"{"id":"a","provider":"p","cwd":"/","workspaceId":"w","createdAt":"c","updatedAt":"u","lastActivityAt":"l","lastUserMessageAt":null,"title":null,"labels":{},"lastStatus":"closed","lastModeId":null,"config":{"toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"}]},"systemPrompt":null,"mcpServers":{"m":{"command":"x"}}},"features":[{"type":"select","id":"f","label":"F","icon":"i","value":null,"options":[{"id":"o","label":"L","metadata":{"z":1}}]},{"type":"toggle","id":"t","label":"T","value":true}],"lastError":null,"requiresAttention":true,"attentionReason":null}"#
    );
}

#[test]
fn schema_rejections_match_zod() {
    assert_eq!(
        zod(
            r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","lastStatus":null}"#
        ),
        Err("lastStatus".to_owned())
    );
    assert!(
        zod(r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c","updatedAt":"u","config":{"toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t","x":1}]}}}"#)
            .is_err(),
        "strict tool policy entries reject unknown keys"
    );
    assert_eq!(
        zod(r#"{"id":"a","provider":"p","cwd":"/","createdAt":"c"}"#),
        Err("updatedAt".to_owned())
    );
}

fn record(id: &str, cwd: &str, status: &str) -> Value {
    json!({
        "id": id,
        "provider": "codex",
        "cwd": cwd,
        "createdAt": "2026-10-01T10:00:00.000Z",
        "updatedAt": "2026-10-01T10:00:00.000Z",
        "lastStatus": status,
        "internal": false,
    })
}

#[test]
fn scan_loads_root_and_nested_files_and_skips_invalid_ones() {
    let home = TestDir::new("agent-scan");
    let nested = home.path().join("tmp-project");
    fs::create_dir_all(nested.join("deeper")).expect("create nested buckets");
    fs::write(
        home.path().join("root.json"),
        record("root", "/x", "idle").to_string(),
    )
    .expect("seed root record");
    fs::write(
        nested.join("nested.json"),
        record("nested", "/tmp/project", "closed").to_string(),
    )
    .expect("seed nested record");
    fs::write(nested.join("broken.json"), "{not json").expect("seed broken record");
    fs::write(
        nested.join("bad-status.json"),
        record("bad", "/tmp/project", "paused").to_string(),
    )
    .expect("seed invalid status");
    fs::write(
        nested.join("deeper").join("too-deep.json"),
        record("deep", "/tmp/project", "idle").to_string(),
    )
    .expect("seed record two levels down");
    fs::write(nested.join("notes.txt"), "{}").expect("seed non-json file");

    let mut store = AgentRecordStore::new(home.path());
    let mut ids = store
        .list()
        .iter()
        .map(|value| value["id"].as_str().expect("id").to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["nested", "root"]);
    assert_eq!(
        store.skipped().len(),
        2,
        "broken and bad-status are skipped"
    );
    assert_eq!(
        store.list()[0]["id"],
        "root",
        "root files load before buckets"
    );
}

#[test]
fn write_uses_json_stringify_layout_and_moves_file_on_cwd_change() {
    let home = TestDir::new("agent-write");
    let mut store = AgentRecordStore::new(home.path());
    let first = store
        .write(record("agent-1", "/tmp/project", "idle"))
        .expect("write record");
    assert!(first.ends_with("tmp-project/agent-1.json"));
    assert_eq!(
        fs::read_to_string(&first).expect("read record"),
        "{\n  \"id\": \"agent-1\",\n  \"provider\": \"codex\",\n  \"cwd\": \"/tmp/project\",\n  \"createdAt\": \"2026-10-01T10:00:00.000Z\",\n  \"updatedAt\": \"2026-10-01T10:00:00.000Z\",\n  \"lastStatus\": \"idle\",\n  \"internal\": false\n}"
    );

    let moved = store
        .write(record("agent-1", "/tmp/other", "closed"))
        .expect("rewrite under new cwd");
    assert!(moved.ends_with("tmp-other/agent-1.json"));
    assert!(!first.exists(), "old cwd bucket file is unlinked");
    assert_eq!(store.list().len(), 1);

    let mut reopened = AgentRecordStore::new(home.path());
    assert_eq!(
        reopened.get("agent-1").expect("record survives restart")["lastStatus"],
        "closed"
    );
    reopened.remove("agent-1").expect("remove record");
    assert!(!moved.exists());
    assert!(reopened.get("agent-1").is_none());
}
