//! `steerActiveTurn` against the real pinned `codex app-server` (0.159.0):
//! input added to a running turn (`turn/steer`), the unavailable answers that
//! need no request, and a steer that clears a pending approval first. Both
//! sessions are the recorded input of the steer differential
//! (`tests/fixtures/steer.json`).

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{Prompt, RunOptions, SteerOptions, SteerResult};
use support::{DisposableRoot, Events, Reply, ResponsesStub, manager_auto_config, stub_provider};

const WAIT: Duration = Duration::from_secs(90);

fn steer(turn: &str, clear: bool) -> SteerOptions {
    SteerOptions {
        expected_turn_id: turn.to_owned(),
        client_message_id: Some("steer-1".to_owned()),
        clear_pending_permissions: clear,
    }
}

fn types(events: &Events) -> Vec<String> {
    events
        .snapshot()
        .iter()
        .map(|event| event["type"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn a_running_turn_is_steered_and_other_steers_are_unavailable() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![Reply::Hold]);
    let root = DisposableRoot::new("steer-accepted");
    let provider = stub_provider(&root, &stub, &codex);
    let session = provider
        .create_session(manager_auto_config(&root, &provider), None, false)
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    let prompt = Prompt::Text("Also say goodbye".to_owned());
    assert_eq!(
        session.steer_active_turn(&prompt, &steer("codex-turn-0", false)),
        Ok(SteerResult::Unavailable),
        "no turn is running"
    );
    let turn = session
        .start_turn(
            &Prompt::Text("Say hello".to_owned()),
            &RunOptions::default(),
        )
        .expect("start turn");
    events.wait_for("turn_started", WAIT);
    assert_eq!(
        session.steer_active_turn(&prompt, &steer("codex-turn-99", false)),
        Ok(SteerResult::Unavailable),
        "another turn id"
    );
    assert_eq!(
        session.steer_active_turn(&prompt, &steer(&turn, false)),
        Ok(SteerResult::Accepted)
    );
    session.interrupt().expect("interrupt");
    events.wait_for("turn_canceled", WAIT);
    assert_eq!(
        session.steer_active_turn(&prompt, &steer(&turn, false)),
        Ok(SteerResult::Unavailable),
        "the turn ended"
    );
    assert_eq!(session.unported(), Vec::<String>::new());
    assert!(!types(&events).contains(&"turn_failed".to_owned()));
    session.close().expect("close");
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn a_steer_clears_the_pending_approval() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![
        Reply::FunctionCall {
            call_id: "call_echo".to_owned(),
            name: "exec_command".to_owned(),
            arguments: json!({
                "cmd": "echo hi",
                "sandbox_permissions": "require_escalated",
                "justification": "Run echo hi?"
            }),
        },
        Reply::Message {
            id: "msg_done".to_owned(),
            deltas: vec!["Done.".to_owned()],
        },
    ]);
    let root = DisposableRoot::new("steer-clear");
    let provider = stub_provider(&root, &stub, &codex);
    let session = provider
        .create_session(manager_auto_config(&root, &provider), None, false)
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    let turn = session
        .start_turn(&Prompt::Text("Run echo".to_owned()), &RunOptions::default())
        .expect("start turn");
    events.wait_for("permission_requested", WAIT);
    assert_eq!(session.pending_permissions().len(), 1);
    assert_eq!(
        session.steer_active_turn(
            &Prompt::Text("Do not run it".to_owned()),
            &steer(&turn, true)
        ),
        Ok(SteerResult::Accepted)
    );
    assert!(
        session.pending_permissions().is_empty(),
        "denied by the steer"
    );
    let resolved: Vec<Value> = events
        .snapshot()
        .into_iter()
        .filter(|event| event["type"] == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["resolution"]["behavior"], json!("deny"));
    events.wait_for("turn_completed", WAIT);
    assert_eq!(session.unported(), Vec::<String>::new());
    session.close().expect("close");
}
