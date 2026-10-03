//! `steerActiveTurn` against the pinned Paseo build. The recorded stdio of a
//! real `codex app-server` 0.159.0 (`tests/fixtures/steer.json`, recorded by
//! `tests/real_codex_steer.rs`) is replayed to the Rust provider and to the
//! pinned `CodexAppServerAgentClient`: steers with no turn, another turn id,
//! the running turn, and an ended turn; and a steer that clears a pending
//! approval. The session events, the steer results, and every line the client
//! sent to Codex must be equal, compared as raw text.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{
    CodexProvider, Prompt, ProviderCommand, ProviderRuntimeSettings, RunOptions, SessionConfig,
    SteerOptions, SteerResult,
};
use support::{DisposableRoot, Events};

const FIXTURE: &str = "steer.json";
const MODEL: &str = "gpt-6-astra";
const WAIT: Duration = Duration::from_secs(30);

fn status(result: SteerResult) -> &'static str {
    match result {
        SteerResult::Accepted => "accepted",
        SteerResult::Unavailable => "unavailable",
    }
}

fn rust_run(scenario: &str, root: &DisposableRoot, log: &Path) -> String {
    let provider = CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: support::replay_argv(&support::pinned_paseo(), FIXTURE, scenario, root, log),
            }),
            env: None,
        }),
        None,
        vec![
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("HOME".into(), root.join("home").into_os_string()),
        ],
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
    let mut steers: Vec<&str> = Vec::new();
    let mut steer = |text: &str, expected: &str, clear: bool| {
        let options = SteerOptions {
            expected_turn_id: expected.to_owned(),
            client_message_id: Some("steer-1".to_owned()),
            clear_pending_permissions: clear,
        };
        let result = session
            .steer_active_turn(&Prompt::Text(text.to_owned()), &options)
            .expect("steer");
        steers.push(status(result));
    };
    let start = |text: &str| {
        session
            .start_turn(&Prompt::Text(text.to_owned()), &RunOptions::default())
            .expect("start turn")
    };
    match scenario {
        "accepted" => {
            steer("Also say goodbye", "codex-turn-0", false);
            let turn = start("Say hello");
            events.wait_for("turn_started", WAIT);
            steer("Also say goodbye", "codex-turn-99", false);
            steer("Also say goodbye", &turn, false);
            session.interrupt().expect("interrupt");
            events.wait_for("turn_canceled", WAIT);
            steer("Also say goodbye", &turn, false);
        }
        "clears_approval" => {
            let turn = start("Run echo");
            events.wait_for("permission_requested", WAIT);
            steer("Do not run it", &turn, true);
            for terminal in ["turn_completed", "turn_canceled", "turn_failed"] {
                if events
                    .snapshot()
                    .iter()
                    .any(|event| event["type"] == terminal)
                {
                    break;
                }
            }
            events.wait_for("turn_completed", WAIT);
        }
        other => panic!("unknown scenario {other}"),
    }
    let pending_after = session.pending_permissions();
    session.close().expect("close");
    json!({"events": events.snapshot(), "steers": steers, "pendingAfter": pending_after})
        .to_string()
}

fn pinned_run(scenario: &str, root: &DisposableRoot, log: &Path) -> String {
    let pinned = support::pinned_paseo();
    let argv = support::replay_argv(&pinned, FIXTURE, scenario, root, log);
    support::run_pinned_node(
        &pinned,
        "tests/support/pinned_steer_session.mjs",
        &[
            pinned.module.to_string_lossy().into_owned(),
            serde_json::to_string(&argv).unwrap(),
            root.project(),
            MODEL.to_owned(),
            scenario.to_owned(),
        ],
        root,
        Duration::from_secs(60),
    )
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn steering_matches_pinned() {
    support::assert_fixture_digest(FIXTURE);
    for (scenario, steers) in [
        (
            "accepted",
            json!(["unavailable", "unavailable", "accepted", "unavailable"]),
        ),
        ("clears_approval", json!(["accepted"])),
    ] {
        let root = DisposableRoot::new("steer-replay");
        let (rust_log, pinned_log) = (
            root.join("rust-client.jsonl"),
            root.join("pinned-client.jsonl"),
        );
        let rust = rust_run(scenario, &root, &rust_log);
        let pinned = pinned_run(scenario, &root, &pinned_log);
        if rust != pinned {
            let at = rust
                .bytes()
                .zip(pinned.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            let around = |text: &str| {
                let start = at.saturating_sub(120);
                text.get(start..(at + 300).min(text.len()))
                    .unwrap_or("")
                    .to_owned()
            };
            panic!(
                "{scenario}: output differs from pinned at byte {at}\n rust:   ...{}\n pinned: ...{}",
                around(&rust),
                around(&pinned)
            );
        }
        let value: Value = serde_json::from_str(&pinned).expect("pinned output");
        assert_eq!(value["steers"], steers, "{scenario}: the steer results");
        let read = |log: &Path| std::fs::read_to_string(log).expect("client log");
        assert_eq!(
            read(&rust_log),
            read(&pinned_log),
            "{scenario}: the lines sent to the app-server differ from pinned"
        );
    }
}
