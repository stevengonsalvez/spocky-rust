//! Mixed-version check for recorded divergence DIV-001: the Rust writer
//! persists values nested deeper than V8 `JSON.stringify` can write (it fails
//! between 3,000 and 5,000 levels on node 22.20.0), so pinned Paseo must still
//! read such a record back. `JSON.parse` in node 22 is iterative.
//!
//! Needs `SPOCKY_PINNED_NODE`, the path of the pinned node 22.20.0 binary,
//! which the lane acceptance command sets. Without it the test FAILS; set
//! `SPOCKY_ALLOW_SKIP=1` to skip it explicitly outside the gate.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_store::agent_record::AgentRecordStore;
use spocky_store::js_value::parse;

const DEPTH: usize = 10_000;

#[test]
fn pinned_node_reads_a_rust_written_10k_deep_record() {
    let Some(node) = std::env::var_os("SPOCKY_PINNED_NODE") else {
        assert!(
            std::env::var_os("SPOCKY_ALLOW_SKIP").is_some(),
            "set SPOCKY_PINNED_NODE to the pinned node 22.20.0 binary (or SPOCKY_ALLOW_SKIP=1)"
        );
        eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: pinned node check not run");
        return;
    };
    let version = Command::new(&node)
        .arg("--version")
        .output()
        .expect("run pinned node");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.20.0",
        "SPOCKY_PINNED_NODE must be the pinned node"
    );

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let home: PathBuf = std::env::temp_dir().join(format!(
        "spocky-store-pinned-node-{}-{nonce}",
        std::process::id()
    ));
    let handle = format!("{}{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
    let record = parse(&format!(
        r#"{{"id":"deep","provider":"codex","cwd":"/tmp/deep","createdAt":"2026-10-01T10:00:00.000Z","updatedAt":"2026-10-01T10:00:00.000Z","persistence":{{"provider":"codex","sessionId":"s","nativeHandle":{handle}}}}}"#
    ))
    .expect("record parses");
    let path = AgentRecordStore::new(&home)
        .write(record)
        .expect("write deep record")
        .expect("not deleting");

    let script = "const fs = require('node:fs');\
        const record = JSON.parse(fs.readFileSync(process.argv[1], 'utf8'));\
        let depth = 0;\
        for (let value = record.persistence.nativeHandle; Array.isArray(value); value = value[0]) depth += 1;\
        process.stdout.write(String(depth));";
    // `gtimeout` on macOS with coreutils, else `timeout`.
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "60"])
        .arg(&node)
        .args(["-e", script])
        .arg(&path)
        .output()
        .expect("run pinned node under gtimeout");
    fs::remove_dir_all(&home).expect("remove disposable store");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), DEPTH.to_string());
}
