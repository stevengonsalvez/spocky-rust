use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_store::{AgentStore, StoredAgentRecord};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-store-legacy-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create disposable store");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove disposable store");
    }
}

fn record(cwd: &str) -> StoredAgentRecord {
    StoredAgentRecord::from_json(&format!(
        r#"{{"id":"agent-1","provider":"codex","cwd":"{cwd}","createdAt":"c","updatedAt":"u"}}"#
    ))
    .expect("valid record")
}

#[test]
fn rewrite_under_a_new_cwd_unlinks_the_stale_file() {
    let disposable = TestDir::new();
    let store = AgentStore::new(disposable.path());
    let first = store.write(&record("/tmp/one")).expect("first write");
    let second = store.write(&record("/tmp/two")).expect("second write");
    assert!(second.ends_with("tmp-two/agent-1.json"));
    assert!(!first.exists(), "writeRecord unlinks the previous path");
    assert_eq!(
        store
            .load("agent-1")
            .expect("load")
            .expect("record present")
            .cwd(),
        "/tmp/two"
    );
}
