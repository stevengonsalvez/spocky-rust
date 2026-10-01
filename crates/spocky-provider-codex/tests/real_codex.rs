//! Drives the real pinned `codex app-server` (0.159.0) against the scripted
//! local Responses stub. Expected events follow pinned Paseo's mapping of the
//! exact notification sequence Codex 0.159.0 emits for a streamed reply.

mod support;

use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{Prompt, RunOptions};
use support::{
    DisposableRoot, Events, Reply, ResponsesStub, full_access_config, manager_full_access_config,
    stub_provider,
};

const WAIT: Duration = Duration::from_secs(90);

fn pinned_codex_on_path() {
    let output = Command::new("codex")
        .arg("--version")
        .output()
        .expect("codex on PATH");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        support::PINNED_CODEX_VERSION,
        "tests require the pinned Codex binary"
    );
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

fn message(id: &str, deltas: &[&str]) -> Reply {
    Reply::Message {
        id: id.to_owned(),
        deltas: deltas.iter().map(|delta| (*delta).to_owned()).collect(),
    }
}

#[test]
fn happy_path_turns_emit_paseo_events_in_paseo_key_order() {
    pinned_codex_on_path();
    let stub = ResponsesStub::start(vec![
        message("msg_stub_1", &["Hello", " from stub."]),
        message("msg_stub_2", &["Second", " reply."]),
    ]);
    let root = DisposableRoot::new("happy");
    let provider = stub_provider(&root, &stub);
    let gates = provider.gates();
    assert!(gates.goals_enabled && gates.auto_review_enabled);

    let session = provider
        .create_session(manager_full_access_config(&root, &provider), None, false)
        .expect("create session");
    let events = Events::attach(&session);

    let info = session.runtime_info().expect("runtime info");
    let thread_id = session.id().expect("thread id after runtime info");
    let model = info["model"].as_str().expect("resolved model").to_owned();
    let thinking = info["thinkingOptionId"].clone();
    assert_eq!(
        compact(&info),
        compact(&json!({
            "provider": "codex",
            "sessionId": thread_id,
            "model": model,
            "thinkingOptionId": thinking,
            "modeId": "full-access",
            "extra": {"collaborationMode": "Default"},
        }))
    );

    let turn_id = session
        .start_turn(
            &Prompt::Text("Say hello".to_owned()),
            &RunOptions {
                client_message_id: Some("client-message-1".to_owned()),
            },
        )
        .expect("start turn");
    assert_eq!(turn_id, "codex-turn-0");
    let completed = events.wait_for("turn_completed", WAIT);
    let usage = completed["usage"].clone();
    let window = usage["contextWindowMaxTokens"]
        .as_u64()
        .expect("context window");
    assert!(window > 0);
    let expected_usage = json!({
        "inputTokens": 10,
        "cachedInputTokens": 0,
        "outputTokens": 4,
        "contextWindowMaxTokens": window,
        "contextWindowUsedTokens": 14,
    });
    assert_eq!(compact(&usage), compact(&expected_usage));

    let got = events.snapshot();
    let user_message_id = got[2]["item"]["messageId"]
        .as_str()
        .expect("codex user message id")
        .to_owned();
    let expected = [
        json!({"type": "thread_started", "provider": "codex", "sessionId": thread_id}),
        json!({"type": "turn_started", "provider": "codex", "turnId": "codex-turn-0"}),
        json!({"type": "timeline", "provider": "codex", "item": {"type": "user_message", "text": "Say hello", "messageId": user_message_id, "clientMessageId": "client-message-1"}, "turnId": "codex-turn-0"}),
        json!({"type": "timeline", "provider": "codex", "item": {"type": "assistant_message", "messageId": "msg_stub_1", "text": "Hello"}, "turnId": "codex-turn-0"}),
        json!({"type": "timeline", "provider": "codex", "item": {"type": "assistant_message", "messageId": "msg_stub_1", "text": " from stub."}, "turnId": "codex-turn-0"}),
        json!({"type": "usage_updated", "provider": "codex", "usage": expected_usage, "turnId": "codex-turn-0"}),
        json!({"type": "turn_completed", "provider": "codex", "usage": expected_usage, "turnId": "codex-turn-0"}),
    ];
    assert_eq!(
        got.iter().map(compact).collect::<Vec<_>>(),
        expected.iter().map(compact).collect::<Vec<_>>()
    );

    assert_second_turn_reuses_the_loaded_thread(&session, &events, expected.len());

    assert_requests_and_persistence(&session, &stub, &root, &thread_id, &model, &thinking);

    let pid = session.app_server_pid().expect("app-server pid");
    session.close().expect("close");
    assert!(
        !support::process_alive(pid),
        "codex app-server survived close"
    );
    assert_eq!(session.id(), None);
    assert_eq!(
        session.start_turn(&Prompt::Text("late".to_owned()), &RunOptions::default()),
        Err("Codex app-server session is closed".to_owned())
    );
}

/// Each turn made one streamed model request, and the persistence handle
/// matches Paseo's `describePersistence()` shape.
fn assert_requests_and_persistence(
    session: &spocky_provider_codex::CodexSession,
    stub: &ResponsesStub,
    root: &DisposableRoot,
    thread_id: &str,
    model: &str,
    thinking: &Value,
) {
    let requests = stub.requests();
    assert_eq!(requests.len(), 2, "one model request per turn");
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/responses");
        assert_eq!(request.body["model"], json!(model));
        assert_eq!(request.body["stream"], json!(true));
    }
    assert!(compact(&requests[0].body["input"]).contains("Say hello"));

    assert_eq!(
        compact(&session.describe_persistence().expect("persistence")),
        compact(&json!({
            "provider": "codex",
            "sessionId": thread_id,
            "nativeHandle": thread_id,
            "metadata": {
                "provider": "codex",
                "cwd": root.project(),
                "title": null,
                "threadId": thread_id,
                "modeId": "full-access",
                "model": model,
                "thinkingOptionId": thinking,
                "asyncQuestions": [],
            },
        }))
    );
    assert_eq!(session.unported(), Vec::<String>::new());
}

/// A second turn reuses the loaded thread (`thread/loaded/list`) and the
/// turn ordinal advances.
fn assert_second_turn_reuses_the_loaded_thread(
    session: &spocky_provider_codex::CodexSession,
    events: &Events,
    first_turn_events: usize,
) {
    let second = session
        .start_turn(&Prompt::Text("Again".to_owned()), &RunOptions::default())
        .expect("second turn");
    assert_eq!(second, "codex-turn-1");
    let deadline = std::time::Instant::now() + WAIT;
    while events
        .snapshot()
        .iter()
        .filter(|event| event["type"] == "turn_completed")
        .count()
        < 2
    {
        assert!(
            std::time::Instant::now() < deadline,
            "second turn did not complete"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let second_events: Vec<Value> = events
        .snapshot()
        .into_iter()
        .skip(first_turn_events)
        .collect();
    let second_types: Vec<String> = second_events
        .iter()
        .map(|event| {
            format!(
                "{}:{}",
                event["type"].as_str().unwrap(),
                event["item"]["type"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        second_types,
        [
            "turn_started:",
            "timeline:user_message",
            "timeline:assistant_message",
            "timeline:assistant_message",
            "usage_updated:",
            "turn_completed:",
        ]
    );
    assert_eq!(
        compact(&second_events[2]["item"]),
        r#"{"type":"assistant_message","messageId":"msg_stub_2","text":"Second"}"#
    );
    assert!(
        second_events
            .iter()
            .all(|event| event["turnId"] == "codex-turn-1")
    );
    assert_eq!(second_events[1]["item"].get("clientMessageId"), None);
}

#[test]
fn interrupt_cancels_the_active_turn() {
    pinned_codex_on_path();
    let stub = ResponsesStub::start(vec![Reply::Hold]);
    let root = DisposableRoot::new("interrupt");
    let provider = stub_provider(&root, &stub);
    let session = provider
        .create_session(manager_full_access_config(&root, &provider), None, false)
        .expect("create session");
    session.runtime_info().expect("runtime info");
    let events = Events::attach(&session);

    let turn_id = session
        .start_turn(
            &Prompt::Text("Wait for me".to_owned()),
            &RunOptions::default(),
        )
        .expect("start turn");
    events.wait_for("turn_started", WAIT);
    assert_eq!(
        session.start_turn(&Prompt::Text("again".to_owned()), &RunOptions::default()),
        Err("A foreground turn is already active".to_owned())
    );

    session.interrupt().expect("interrupt");
    let canceled = events.wait_for("turn_canceled", WAIT);
    assert_eq!(
        compact(&canceled),
        compact(
            &json!({"type": "turn_canceled", "provider": "codex", "reason": "interrupted", "turnId": turn_id})
        )
    );
    // After the turn ends an interrupt is a no-op, as in Paseo.
    session.interrupt().expect("idle interrupt");
    assert_eq!(session.unported(), Vec::<String>::new());
    session.close().expect("close");
}

#[test]
fn app_server_exit_mid_turn_fails_the_turn() {
    pinned_codex_on_path();
    let stub = ResponsesStub::start(vec![Reply::Hold]);
    let root = DisposableRoot::new("exit");
    let provider = stub_provider(&root, &stub);
    let session = provider
        .create_session(manager_full_access_config(&root, &provider), None, false)
        .expect("create session");
    session.runtime_info().expect("runtime info");
    let events = Events::attach(&session);
    session
        .start_turn(&Prompt::Text("Wait".to_owned()), &RunOptions::default())
        .expect("start turn");
    events.wait_for("turn_started", WAIT);

    let pid = session.app_server_pid().expect("pid");
    let status = Command::new("kill")
        .args(["-s", "KILL", &pid.to_string()])
        .status()
        .expect("kill codex");
    assert!(status.success());

    let failed = events.wait_for("turn_failed", WAIT);
    let error = failed["error"].as_str().expect("error text");
    assert!(
        error.starts_with("Codex app-server exited with code null and signal SIGKILL"),
        "unexpected error: {error}"
    );
    assert_eq!(failed["turnId"], json!("codex-turn-0"));
    let keys: Vec<&str> = failed
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["type", "provider", "error", "turnId"]);
    session.close().expect("close after exit");
}

#[test]
fn turn_start_without_a_model_is_rejected_by_codex() {
    // Without the agent manager's catalog model the collaboration mode
    // settings carry no `model`; pinned Paseo sends the same params and
    // Codex 0.159.0 rejects them.
    pinned_codex_on_path();
    let stub = ResponsesStub::start(vec![]);
    let root = DisposableRoot::new("no-model");
    let provider = stub_provider(&root, &stub);
    let session = provider
        .create_session(full_access_config(&root), None, false)
        .expect("create session");
    session.runtime_info().expect("runtime info");
    assert_eq!(
        session.start_turn(
            &Prompt::Text("Say hello".to_owned()),
            &RunOptions::default()
        ),
        Err("Invalid request: missing field `model`".to_owned())
    );
    assert!(stub.requests().is_empty());
    session.close().expect("close");
}

#[test]
fn dropping_an_unclosed_session_stops_its_app_server() {
    pinned_codex_on_path();
    let stub = ResponsesStub::start(vec![]);
    let root = DisposableRoot::new("drop");
    let provider = stub_provider(&root, &stub);
    let session = provider
        .create_session(full_access_config(&root), None, false)
        .expect("create session");
    let pid = session.app_server_pid().expect("pid");
    assert!(support::process_alive(pid));
    drop(session);
    assert!(!support::process_alive(pid), "codex app-server leaked");
}
