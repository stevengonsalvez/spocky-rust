//! G2 approval differential against the pinned Paseo build. Each scenario
//! replays one recorded `codex app-server` stdio session (real codex 0.159.0,
//! `tests/fixtures/g2_approvals.json`) to the Rust provider and to the pinned
//! `CodexAppServerAgentClient`, drives the same approval action on both, and
//! requires identical session events, pending permissions before and after,
//! and identical client-to-Codex JSON lines (the approval decisions and
//! `turn/interrupt` included). Nothing is normalized: both sides read the
//! same replayed bytes.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spocky_provider_codex::{
    CodexProvider, Prompt, ProviderCommand, ProviderRuntimeSettings, RunOptions, SessionConfig,
};
use support::{DisposableRoot, Events, PinnedPaseo};

const WAIT: Duration = Duration::from_secs(30);
const MODEL: &str = "gpt-6-astra";

fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The command both clients launch as `codex`: the replay of `scenario`,
/// logging what the client sends to `log`.
fn replay_argv(
    pinned: &PinnedPaseo,
    scenario: &str,
    root: &DisposableRoot,
    log: &Path,
) -> Vec<String> {
    [
        pinned.node.clone(),
        crate_path("tests/support/codex_replay.mjs"),
        crate_path("tests/fixtures/g2_approvals.json"),
        PathBuf::from(scenario),
        root.path.clone(),
        log.to_path_buf(),
    ]
    .iter()
    .map(|part| part.to_string_lossy().into_owned())
    .collect()
}

fn wait_for_any(events: &Events, types: &[&str]) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = events
            .snapshot()
            .into_iter()
            .find(|event| types.iter().any(|kind| event["type"] == *kind))
        {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {types:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn rust_run(argv: Vec<String>, root: &DisposableRoot, prompt: &str, action: &str) -> Value {
    let base_env = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("HOME".into(), root.join("home").into_os_string()),
    ];
    let provider = CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace { argv }),
            env: None,
        }),
        None,
        base_env,
    );
    let session = provider
        .create_session(
            SessionConfig {
                cwd: root.project(),
                mode_id: Some("auto".to_owned()),
                model: Some(MODEL.to_owned()),
                ..SessionConfig::default()
            },
            None,
            false,
        )
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    session
        .start_turn(&Prompt::Text(prompt.to_owned()), &RunOptions::default())
        .expect("start turn");
    let requested = wait_for_any(&events, &["permission_requested"]);
    let before = session.pending_permissions();
    let id = requested["request"]["id"].as_str().expect("request id");
    match action {
        "allow" => session.respond_to_permission(id, &json!({"behavior": "allow"})),
        "deny" => {
            session.respond_to_permission(id, &json!({"behavior": "deny", "message": "Not now"}))
        }
        "deny_interrupt" => session.respond_to_permission(
            id,
            &json!({"behavior": "deny", "message": "Stop", "interrupt": true}),
        ),
        "interrupt" => session.interrupt(),
        other => panic!("unknown action {other}"),
    }
    .expect("action");
    wait_for_any(&events, &["turn_completed", "turn_canceled", "turn_failed"]);
    let after = session.pending_permissions();
    session.close().expect("close");
    json!({"events": events.snapshot(), "pendingBefore": before, "pendingAfter": after})
}

fn pinned_run(
    pinned: &PinnedPaseo,
    argv: &[String],
    root: &DisposableRoot,
    prompt: &str,
    action: &str,
) -> Value {
    let mut child = Command::new(&pinned.node)
        .arg(crate_path("tests/support/pinned_g2_session.mjs"))
        .arg(&pinned.module)
        .arg(serde_json::to_string(argv).unwrap())
        .arg(root.project())
        .arg(MODEL)
        .arg(prompt)
        .arg(action)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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
            panic!("pinned G2 run exceeded 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("pinned output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "pinned G2 run failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(stdout.trim()).expect("pinned run JSON")
}

fn differential(scenario: &str, prompt: &str, action: &str) {
    let pinned = support::pinned_paseo();
    let root = DisposableRoot::new(&format!("g2-{scenario}"));
    let rust_log = root.join("rust-client.jsonl");
    let pinned_log = root.join("pinned-client.jsonl");
    let rust = rust_run(
        replay_argv(&pinned, scenario, &root, &rust_log),
        &root,
        prompt,
        action,
    );
    let argv = replay_argv(&pinned, scenario, &root, &pinned_log);
    let paseo = pinned_run(&pinned, &argv, &root, prompt, action);
    for part in ["events", "pendingBefore", "pendingAfter"] {
        assert_eq!(
            serde_json::to_string(&rust[part]).unwrap(),
            serde_json::to_string(&paseo[part]).unwrap(),
            "{scenario}: {part} differ"
        );
    }
    let read = |log: &Path| std::fs::read_to_string(log).expect("client log");
    let (ours, theirs) = (read(&rust_log), read(&pinned_log));
    for (index, (rust_line, pinned_line)) in ours.lines().zip(theirs.lines()).enumerate() {
        assert_eq!(
            rust_line, pinned_line,
            "{scenario}: client line {index} differs"
        );
    }
    assert_eq!(
        ours.lines().count(),
        theirs.lines().count(),
        "{scenario}: client line count"
    );
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn allowed_command_matches_pinned() {
    differential("allow", "Run echo", "allow");
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn denied_command_matches_pinned() {
    differential("deny", "Run echo", "deny");
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn deny_with_interrupt_matches_pinned() {
    differential("deny_interrupt", "Run echo", "deny_interrupt");
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn interrupt_while_approval_pending_matches_pinned() {
    differential("interrupt_pending", "Run echo", "interrupt");
}
