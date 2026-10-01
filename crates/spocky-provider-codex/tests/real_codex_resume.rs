//! G3 provider behavior against the real pinned `codex app-server` (0.159.0):
//! a fresh app-server resumes a persisted thread (`thread/resume`), replays
//! its timeline from `thread/read`, and continues; the history-only purpose
//! reads the timeline without resuming. Expected shapes follow pinned Paseo's
//! `resumeSession`, `loadPersistedHistory`, and `streamHistory`.

mod support;

use std::time::Duration;

use serde_json::{Map, Value, json};
use spocky_provider_codex::{Prompt, ResumeHandle, RunOptions};
use support::{
    DisposableRoot, Events, Reply, ResponsesStub, manager_full_access_config, stub_provider,
};

const WAIT: Duration = Duration::from_secs(90);

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

fn message(id: &str, text: &str) -> Reply {
    Reply::Message {
        id: id.to_owned(),
        deltas: vec![text.to_owned()],
    }
}

fn is_iso_millis(value: &Value) -> bool {
    // `Date.prototype.toISOString` of whole seconds: `YYYY-MM-DDTHH:mm:ss.000Z`.
    value
        .as_str()
        .is_some_and(|text| text.len() == 24 && &text[10..11] == "T" && &text[19..] == ".000Z")
}

/// Runs one turn on a fresh session and returns its persistence handle and
/// user message id.
fn persisted_turn(
    provider: &spocky_provider_codex::CodexProvider,
    root: &DisposableRoot,
) -> (ResumeHandle, String) {
    let session = provider
        .create_session(manager_full_access_config(root, provider), None, false)
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    session
        .start_turn(
            &Prompt::Text("Say hello".to_owned()),
            &RunOptions::default(),
        )
        .expect("first turn");
    events.wait_for("turn_completed", WAIT);
    let user = events
        .snapshot()
        .into_iter()
        .find(|event| event["item"]["type"] == "user_message")
        .expect("user message");
    let persistence = session.describe_persistence().expect("persistence");
    session.close().expect("close first session");
    let handle = ResumeHandle {
        session_id: persistence["sessionId"].as_str().unwrap().to_owned(),
        metadata: persistence["metadata"].as_object().cloned(),
    };
    (
        handle,
        user["item"]["messageId"].as_str().unwrap().to_owned(),
    )
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn resumed_session_replays_history_then_continues() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![
        message("msg_first", "Hello from stub."),
        message("msg_second", "Back again."),
    ]);
    let root = DisposableRoot::new("resume");
    let provider = stub_provider(&root, &stub, &codex);
    let (handle, user_id) = persisted_turn(&provider, &root);

    let resumed = provider
        .resume_session(&handle, &Map::new(), None, false)
        .expect("resume");
    assert_eq!(resumed.id(), Some(handle.session_id.clone()));
    let history = resumed.stream_history();
    assert_eq!(history.len(), 2);
    for event in &history {
        let keys: Vec<&str> = event
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["type", "provider", "item", "timestamp"]);
        assert!(is_iso_millis(&event["timestamp"]), "{event}");
    }
    assert_eq!(
        compact(&history[0]["item"]),
        compact(&json!({"type": "user_message", "text": "Say hello", "messageId": user_id}))
    );
    assert_eq!(
        compact(&history[1]["item"]),
        compact(
            &json!({"type": "assistant_message", "text": "Hello from stub.", "messageId": "msg_first"})
        )
    );
    assert!(resumed.stream_history().is_empty(), "history replays once");
    let info = resumed.runtime_info().expect("runtime info");
    assert_eq!(info["sessionId"], json!(handle.session_id));
    assert_eq!(info["modeId"], json!("full-access"));
    assert_eq!(
        info["model"],
        handle.metadata.as_ref().unwrap()["model"],
        "resumed model comes from the persisted metadata"
    );

    let events = Events::attach(&resumed);
    let turn = resumed
        .start_turn(&Prompt::Text("Again".to_owned()), &RunOptions::default())
        .expect("resumed turn");
    assert_eq!(turn, "codex-turn-0");
    events.wait_for("turn_completed", WAIT);
    let assistant: Vec<Value> = events
        .snapshot()
        .into_iter()
        .filter(|event| event["item"]["type"] == "assistant_message")
        .collect();
    assert_eq!(
        compact(&assistant[0]["item"]),
        r#"{"type":"assistant_message","messageId":"msg_second","text":"Back again."}"#
    );
    assert_eq!(stub.requests().len(), 2);
    assert_eq!(resumed.unported(), Vec::<String>::new());
    resumed.close().expect("close resumed");
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn history_purpose_reads_without_resuming() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![message("msg_first", "Hello from stub.")]);
    let root = DisposableRoot::new("history");
    let provider = stub_provider(&root, &stub, &codex);
    let (handle, _) = persisted_turn(&provider, &root);

    let archived = provider
        .resume_session(&handle, &Map::new(), None, true)
        .expect("history session");
    assert_eq!(archived.app_server_pid(), None, "no live app-server");
    assert_eq!(archived.stream_history().len(), 2);
    assert_eq!(
        archived.start_turn(&Prompt::Text("x".to_owned()), &RunOptions::default()),
        Err("Codex client not initialized".to_owned())
    );
    assert_eq!(archived.unported(), Vec::<String>::new());
    archived.close().expect("close");
}
