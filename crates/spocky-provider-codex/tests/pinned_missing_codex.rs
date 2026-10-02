//! G4 missing codex against the pinned Paseo build: with no `codex` on PATH
//! the Rust provider and the pinned `CodexAppServerAgentClient` must report
//! the same availability and the same failure text from `createSession`,
//! `resumeSession` and both forms of `fetchCatalog`.

mod support;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spocky_provider_codex::{CodexProvider, ResumeHandle, SessionConfig};
use support::DisposableRoot;

fn outcome<T>(result: Result<T, String>, value: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(ok) => json!({"value": value(ok)}),
        Err(error) => json!({"error": error}),
    }
}

fn rust_run(root: &DisposableRoot, path_dir: &Path) -> Value {
    let provider = CodexProvider::new(
        None,
        None,
        vec![
            ("PATH".into(), path_dir.as_os_str().to_owned()),
            ("HOME".into(), root.join("home").into_os_string()),
        ],
    );
    let config = SessionConfig {
        cwd: root.project(),
        mode_id: Some("full-access".to_owned()),
        ..SessionConfig::default()
    };
    let resume = ResumeHandle {
        session_id: "thread-1".to_owned(),
        metadata: serde_json::from_value(json!({"cwd": root.project()})).ok(),
    };
    json!({
        "available": outcome(provider.is_available(), |found| json!(found)),
        "create": outcome(provider.create_session(config, None, false), |session| json!(session.id())),
        "resume": outcome(
            provider.resume_session(&resume, &serde_json::Map::new(), None, false),
            |session| json!(session.id()),
        ),
        "catalog": outcome(provider.fetch_catalog_signalled(None, false), |catalog| catalog),
        "catalogSignalled": outcome(
            provider.fetch_catalog_signalled(Some(Instant::now() + Duration::from_secs(60)), true),
            |catalog| catalog,
        ),
    })
}

fn pinned_run(root: &DisposableRoot, path_dir: &Path) -> Value {
    let pinned = support::pinned_paseo();
    let mut child = Command::new(&pinned.node)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/pinned_missing_codex.mjs"))
        .arg(&pinned.module)
        .arg(root.project())
        .env_clear()
        .env("PATH", path_dir)
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pinned node");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("wait for pinned node").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned missing-codex run exceeded 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("pinned output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "pinned run failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(stdout.trim()).expect("pinned run JSON")
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn missing_codex_matches_pinned() {
    let root = DisposableRoot::new("missing-codex");
    // An empty directory on PATH: `which -a codex` finds nothing.
    let path_dir = root.join("empty-path");
    std::fs::create_dir_all(&path_dir).expect("empty path dir");
    let rust = rust_run(&root, &path_dir);
    let pinned = pinned_run(&root, &path_dir);
    assert_eq!(
        serde_json::to_string(&rust).unwrap(),
        serde_json::to_string(&pinned).unwrap(),
        "missing-codex behavior differs from pinned"
    );
    assert_eq!(rust["available"], json!({"value": false}));
    assert_eq!(
        rust["create"]["error"],
        json!(spocky_provider_codex::launch::CODEX_NOT_FOUND_MESSAGE)
    );
}
