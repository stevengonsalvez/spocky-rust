//! Legacy `codex/event/patch_apply_*` notifications against the pinned
//! Paseo build. A hand-authored corpus of `mapCodexPatchNotificationToToolCall`
//! arguments (`tests/fixtures/patch_notification_corpus.json`) goes through
//! the pinned function and through `patch_notification_envelope`. Compared
//! as raw JSON: the envelope `mapCodexToolCallEnvelope` hands
//! `toToolCallFromNormalizedEnvelope` (before the shared edit-detail branch,
//! which is not ported) and the status and error the emitted timeline item
//! carries. A diff over `truncateDiffText`'s 12,000-unit limit is reported
//! unported here, so those cases only check that pinned truncated.

mod support;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spocky_provider_codex::tools::{PatchNotification, patch_notification_envelope};
use support::DisposableRoot;

fn rust_line(input: &Value) -> Result<String, ()> {
    let changes = input.get("changes").unwrap_or(&Value::Null);
    let params = PatchNotification {
        call_id: input.get("callId").and_then(Value::as_str),
        changes,
        cwd: input.get("cwd").and_then(Value::as_str),
        stdout: input.get("stdout").and_then(Value::as_str),
        stderr: input.get("stderr").and_then(Value::as_str),
        success: input.get("success").and_then(Value::as_bool),
        running: input["running"].as_bool().expect("running"),
    };
    match patch_notification_envelope(&params) {
        Ok(None) => Ok("null".to_owned()),
        Ok(Some(envelope)) => Ok(serde_json::to_string(&json!({
            "envelope": envelope.to_json(),
            "final": {"status": envelope.timeline_status(), "error": envelope.timeline_error()},
        }))
        .unwrap()),
        Err(_) => Err(()),
    }
}

fn pinned_lines(root: &DisposableRoot, corpus: &Path) -> Vec<String> {
    let pinned = support::pinned_paseo();
    // Both oracle files are digest-checked before the driver copies them.
    pinned.verified_oracle("codex/tool-call-mapper.js");
    pinned.verified_oracle("codex-app-server-agent.js");
    let providers = pinned.providers_dir();
    let mut child = Command::new(&pinned.node)
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/support/pinned_patch_notification_corpus.mjs"),
        )
        .arg(providers)
        .arg(root.join("pinned-patch"))
        .arg(corpus)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pinned node");
    // The output is larger than a pipe buffer: drain it while waiting.
    let drain = |stream: Option<Box<dyn Read + Send>>| {
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
    let deadline = Instant::now() + Duration::from_secs(90);
    while child.try_wait().expect("wait for pinned node").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned patch corpus run exceeded 90 s");
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
fn legacy_patch_notifications_match_pinned() {
    let corpus_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/patch_notification_corpus.json");
    let corpus: Value =
        serde_json::from_str(&std::fs::read_to_string(&corpus_path).expect("corpus"))
            .expect("json");
    let cases = corpus["cases"].as_array().expect("cases");
    let root = DisposableRoot::new("patch-notification-corpus");
    let pinned = pinned_lines(&root, &corpus_path);
    assert_eq!(pinned.len(), cases.len(), "one pinned line per case");

    let mut mismatches = Vec::new();
    let mut unported = Vec::new();
    for (case, pinned_line) in cases.iter().zip(&pinned) {
        let name = case["name"].as_str().unwrap_or("?");
        match rust_line(&case["input"]) {
            Ok(rust) if rust == *pinned_line => {}
            Ok(rust) => {
                mismatches.push(format!("{name}\n  rust:   {rust}\n  pinned: {pinned_line}"));
            }
            Err(()) => {
                unported.push(name.to_owned());
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
    assert_eq!(
        unported,
        ["long diff over limit", "long diff end over limit"],
        "exactly the over-limit diff cases are unported"
    );
}
