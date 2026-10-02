//! `mapFileChangeItem` against the pinned Paseo build. A hand-authored corpus
//! of Codex `fileChange` thread items (`tests/fixtures/file_change_corpus.json`)
//! goes through the pinned mapper and through `map_file_change_item`; the
//! envelopes (`callId`, `name`, `input`, `output`, `status`, `error`, `cwd`)
//! are compared as raw JSON text, with `null` where the item schema rejects
//! the item. A diff longer than `truncateDiffText`'s 12,000 units is reported
//! unported by the Rust side (that function belongs to the shared tool-detail
//! primitives), so those cases check only that the pinned side truncated.

mod support;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use spocky_provider_codex::tools::map_file_change_item;
use support::DisposableRoot;

fn rust_line(case: &Value) -> Result<String, ()> {
    let item = case["item"].as_object().expect("item object");
    let cwd = case.get("cwd").and_then(Value::as_str);
    match map_file_change_item(item, cwd) {
        Ok(None) => Ok("null".to_owned()),
        Ok(Some(envelope)) => Ok(serde_json::to_string(&envelope.to_json()).unwrap()),
        Err(_) => Err(()),
    }
}

fn pinned_lines(root: &DisposableRoot, corpus: &Path) -> Vec<String> {
    let pinned = support::pinned_paseo();
    let mapper = pinned
        .module
        .parent()
        .expect("agent providers dir")
        .join("codex/tool-call-mapper.js");
    let mut child = Command::new(&pinned.node)
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/support/pinned_file_change_corpus.mjs"),
        )
        .arg(&mapper)
        .arg(root.join("pinned-mapper"))
        .arg(corpus)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pinned node");
    // The corpus output is larger than a pipe buffer, so drain both streams
    // while waiting; reading only after the exit would deadlock the child.
    let drain = |stream: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = drain(child.stdout.take().map(|s| Box::new(s) as _));
    let stderr = drain(child.stderr.take().map(|s| Box::new(s) as _));
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("wait for pinned node").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned file-change corpus run exceeded 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().expect("pinned node status");
    let (stdout, stderr) = (stdout.join().unwrap(), stderr.join().unwrap());
    assert!(
        status.success(),
        "pinned corpus run failed: {stdout}\n{stderr}"
    );
    stdout.lines().map(str::to_owned).collect()
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn file_change_envelopes_match_pinned() {
    let corpus_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/file_change_corpus.json");
    let corpus: Value =
        serde_json::from_str(&std::fs::read_to_string(&corpus_path).expect("corpus"))
            .expect("json");
    let cases = corpus["cases"].as_array().expect("cases");
    let root = DisposableRoot::new("file-change-corpus");
    let pinned = pinned_lines(&root, &corpus_path);
    assert_eq!(pinned.len(), cases.len(), "one pinned line per case");

    let mut mismatches = Vec::new();
    let mut unported = 0;
    for (case, pinned_line) in cases.iter().zip(&pinned) {
        let name = case["name"].as_str().unwrap_or("?");
        match rust_line(case) {
            Ok(rust) if rust == *pinned_line => {}
            Ok(rust) => {
                mismatches.push(format!("{name}\n  rust:   {rust}\n  pinned: {pinned_line}"));
            }
            Err(()) => {
                unported += 1;
                if !pinned_line.contains("...[truncated ") {
                    mismatches.push(format!(
                        "{name}: unported by the diff limit, but pinned did not truncate: {pinned_line}"
                    ));
                }
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ from pinned:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
    // Only diff text is cut at the limit; a long plain text is not, so the
    // plain long case matches pinned like the rest.
    assert_eq!(unported, 1, "exactly the over-limit diff case is unported");
}
