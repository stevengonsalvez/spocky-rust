use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use spocky_store::{AgentStore, StoredAgentRecord};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after Unix epoch")
            .as_nanos();
        // The clock ticks in microseconds on macOS, so parallel tests can read
        // the same nanosecond value; the counter keeps their directories apart.
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "spocky-store-baseline-roundtrip-{}-{nonce}-{serial}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create disposable store directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove disposable store directory");
    }
}

#[test]
fn frozen_baseline_agent_survives_atomic_write_and_restart() {
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/baseline-agent.json");
    let fixture_text = fs::read_to_string(fixture_path).expect("read frozen baseline fixture");
    let fixture_json: Value = serde_json::from_str(&fixture_text).expect("fixture is JSON");
    let record = StoredAgentRecord::from_json(&fixture_text).expect("fixture follows schema");

    assert_eq!(record.id(), "agent-baseline-roundtrip");
    assert_eq!(record.provider(), "codex");
    assert_eq!(record.cwd(), "/tmp/paseo contract project");
    assert_eq!(record.created_at(), "2026-09-20T09:00:00.000Z");
    assert_eq!(record.updated_at(), "2026-09-20T09:05:00.000Z");
    assert_eq!(record.last_status(), "closed");

    let disposable = TestDir::new();
    let store = AgentStore::new(disposable.path());
    let written_path = store.write(&record).expect("atomic agent write succeeds");

    assert!(written_path.ends_with("tmp-paseo contract project/agent-baseline-roundtrip.json"));
    let entries = fs::read_dir(written_path.parent().expect("record parent"))
        .expect("read record directory")
        .map(|entry| entry.expect("read directory entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "atomic temp file must be renamed away");

    let on_disk: Value = serde_json::from_str(
        &fs::read_to_string(&written_path).expect("read atomically written record"),
    )
    .expect("written record is JSON");
    assert_eq!(on_disk, fixture_json);

    let restarted_store = AgentStore::new(disposable.path());
    let reloaded = restarted_store
        .load("agent-baseline-roundtrip")
        .expect("restart scan succeeds")
        .expect("record survives restart");
    assert_eq!(reloaded.as_value(), &fixture_json);
    assert_eq!(
        reloaded.as_value()["config"]["providerOptions"]["futureProviderOption"],
        fixture_json["config"]["providerOptions"]["futureProviderOption"]
    );
    assert_eq!(
        reloaded.as_value()["persistence"]["nativeHandle"]["futureNativeField"],
        fixture_json["persistence"]["nativeHandle"]["futureNativeField"]
    );
}

#[test]
fn invalid_required_field_is_rejected() {
    let error = StoredAgentRecord::from_json(
        r#"{"id":"agent","provider":"codex","cwd":"/tmp","createdAt":"now"}"#,
    )
    .expect_err("missing updatedAt must fail");

    assert_eq!(
        error.to_string(),
        "missing required string field 'updatedAt'"
    );
}

#[test]
fn project_directory_names_match_baseline_path_rules() {
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/baseline-agent.json");
    let fixture_text = fs::read_to_string(fixture_path).expect("read frozen baseline fixture");
    let fixture_json: Value = serde_json::from_str(&fixture_text).expect("fixture is JSON");

    for (cwd, expected) in [
        ("/tmp/project/", "tmp-project"),
        ("/", "root"),
        (r"D:\Users\dev\MyProject", "D-Users-dev-MyProject"),
        (r"D:\", "D"),
        (r"\\server\share\folder\", "server-share-folder"),
    ] {
        let mut value = fixture_json.clone();
        value["cwd"] = Value::String(cwd.to_owned());
        let record = StoredAgentRecord::from_json(&value.to_string()).expect("record is valid");
        let disposable = TestDir::new();
        let written = AgentStore::new(disposable.path())
            .write(&record)
            .expect("record writes");

        assert_eq!(
            written
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some(expected),
            "cwd {cwd}"
        );
    }
}
