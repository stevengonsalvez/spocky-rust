//! `updateNativeThreadArchiveState` against the pinned Paseo build. The
//! recorded stdio of a real `codex app-server` 0.159.0
//! (`tests/fixtures/native_archive.json`, recorded by
//! `native_archive_and_restore_run_against_real_codex`) is replayed to the
//! Rust provider and to the pinned `CodexAppServerAgentClient`: archive, a
//! restore, a restore of a thread that is not archived (`thread/read`
//! settles it), and a restore of an unknown thread (the first error is the
//! result). The outcome and every line the client sent to the app-server
//! must be equal, compared as raw text.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{
    CodexProvider, NativeArchiveState, ProviderCommand, ProviderRuntimeSettings,
};
use support::DisposableRoot;

const FIXTURE: &str = "native_archive.json";

/// (scenario, state, the outcome pinned reports as JSON text).
const SCENARIOS: [(&str, NativeArchiveState, &str); 4] = [
    ("archive", NativeArchiveState::Archive, r#"{"ok":true}"#),
    ("restore", NativeArchiveState::Restore, r#"{"ok":true}"#),
    (
        "restore_not_archived",
        NativeArchiveState::Restore,
        r#"{"ok":true}"#,
    ),
    (
        "restore_unknown",
        NativeArchiveState::Restore,
        r#"{"error":{"message":"no archived rollout found for thread id 00000000-0000-4000-8000-000000000000"}}"#,
    ),
];

/// The `threadId` the recorded client sent in its `thread/archive` or
/// `thread/unarchive` request.
fn recorded_thread_id(scenario: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE);
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture"))
        .expect("fixture JSON");
    fixture["scenarios"][scenario]["in"]
        .as_array()
        .expect("recorded client lines")
        .iter()
        .map(|line| serde_json::from_str::<Value>(line.as_str().unwrap()).expect("line JSON"))
        .find(|line| {
            matches!(
                line["method"].as_str(),
                Some("thread/archive" | "thread/unarchive")
            )
        })
        .and_then(|line| line["params"]["threadId"].as_str().map(str::to_owned))
        .expect("recorded thread id")
}

fn replay_command(scenario: &str, root: &DisposableRoot, log: &Path) -> Vec<String> {
    support::replay_argv(&support::pinned_paseo(), FIXTURE, scenario, root, log)
}

fn rust_outcome(
    scenario: &str,
    state: NativeArchiveState,
    thread_id: &str,
    root: &DisposableRoot,
    log: &Path,
) -> String {
    let provider = CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: replay_command(scenario, root, log),
            }),
            env: None,
        }),
        None,
        vec![
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("HOME".into(), root.join("home").into_os_string()),
        ],
    );
    match provider.update_native_thread_archive_state(thread_id, state) {
        Ok(()) => json!({"ok": true}).to_string(),
        Err(message) => json!({"error": {"message": message}}).to_string(),
    }
}

fn pinned_outcome(
    scenario: &str,
    state: NativeArchiveState,
    thread_id: &str,
    root: &DisposableRoot,
    log: &Path,
) -> String {
    let pinned = support::pinned_paseo();
    let argv = replay_command(scenario, root, log);
    support::run_pinned_node(
        &pinned,
        "tests/support/pinned_native_archive.mjs",
        &[
            pinned.module.to_string_lossy().into_owned(),
            serde_json::to_string(&argv).unwrap(),
            match state {
                NativeArchiveState::Archive => "archive",
                NativeArchiveState::Restore => "restore",
            }
            .to_owned(),
            thread_id.to_owned(),
        ],
        root,
        Duration::from_secs(60),
    )
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn native_archive_state_matches_pinned() {
    support::assert_fixture_digest(FIXTURE);
    for (scenario, state, expected) in SCENARIOS {
        let root = DisposableRoot::new("native-archive-replay");
        let thread_id = recorded_thread_id(scenario);
        let rust_log = root.join("rust-client.jsonl");
        let pinned_log = root.join("pinned-client.jsonl");
        let rust = rust_outcome(scenario, state, &thread_id, &root, &rust_log);
        let pinned = pinned_outcome(scenario, state, &thread_id, &root, &pinned_log);
        assert_eq!(rust, pinned, "{scenario}: outcome differs from pinned");
        assert_eq!(
            pinned, expected,
            "{scenario}: the outcome the recording gives"
        );
        let sent = |log: &Path| std::fs::read_to_string(log).expect("client log");
        assert_eq!(
            sent(&rust_log),
            sent(&pinned_log),
            "{scenario}: the lines sent to the app-server differ from pinned"
        );
    }
}
