//! G2 provider behavior against the real pinned `codex app-server` (0.159.0):
//! `--mode auto` with a scripted shell call that needs approval, answered
//! allow, deny, deny with interrupt, or left pending while the turn is
//! interrupted, plus an `apply_patch` file change outside the writable roots.
//! Expected events follow pinned Paseo's approval and `commandExecution`
//! mapping; the wire decision (`decline` or `cancel`) is observed through what
//! Codex does next.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{CodexSession, Prompt, RunOptions};
use support::{DisposableRoot, Events, Reply, ResponsesStub, manager_auto_config, stub_provider};

const WAIT: Duration = Duration::from_secs(90);

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

fn escalated_echo() -> Reply {
    Reply::FunctionCall {
        call_id: "call_echo".to_owned(),
        name: "exec_command".to_owned(),
        arguments: json!({
            "cmd": "echo hi",
            "sandbox_permissions": "require_escalated",
            "justification": "Run echo hi?"
        }),
    }
}

fn done() -> Reply {
    Reply::Message {
        id: "msg_done".to_owned(),
        deltas: vec!["Done.".to_owned()],
    }
}

/// Fields drop in order: the session stops Codex before the root is deleted.
struct Turn {
    session: CodexSession,
    events: Events,
    thread_id: String,
    stub: ResponsesStub,
    root: DisposableRoot,
}

fn start_turn(label: &str, replies: Vec<Reply>, codex: &str) -> Turn {
    let stub = ResponsesStub::start(replies);
    let root = DisposableRoot::new(label);
    let provider = stub_provider(&root, &stub, codex);
    let session = provider
        .create_session(manager_auto_config(&root, &provider), None, false)
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    let thread_id = session.id().expect("thread");
    session
        .start_turn(&Prompt::Text("Run echo".to_owned()), &RunOptions::default())
        .expect("start turn");
    events.wait_for("permission_requested", WAIT);
    Turn {
        session,
        events,
        thread_id,
        stub,
        root,
    }
}

fn expected_request(turn: &Turn, request: &Value) -> Value {
    let cwd = turn.root.project();
    let metadata = &request["metadata"];
    assert_eq!(metadata["itemId"], json!("call_echo"));
    assert_eq!(metadata["threadId"], json!(turn.thread_id));
    json!({
        "id": "permission-call_echo",
        "provider": "codex",
        "name": "CodexBash",
        "kind": "tool",
        "title": "Run command: /bin/zsh -lc 'echo hi'",
        "description": "Run echo hi?",
        "input": {"command": "/bin/zsh -lc 'echo hi'", "cwd": cwd},
        "detail": {"type": "shell", "command": "echo hi", "cwd": cwd},
        "metadata": {"itemId": "call_echo", "threadId": turn.thread_id, "turnId": metadata["turnId"]},
    })
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn allowed_command_runs_and_matches_paseo_events() {
    let codex = support::real_codex();
    let turn = start_turn("allow", vec![escalated_echo(), done()], &codex);
    let pending = turn.session.pending_permissions();
    assert_eq!(pending.len(), 1, "permit ls shows the request");
    let request = pending[0].clone();
    assert_eq!(
        compact(&request),
        compact(&expected_request(&turn, &request))
    );

    turn.session
        .respond_to_permission("permission-call_echo", &json!({"behavior": "allow"}))
        .expect("allow");
    turn.events.wait_for("turn_completed", WAIT);
    assert!(turn.session.pending_permissions().is_empty());

    let cwd = turn.root.project();
    let got = turn.events.snapshot();
    let types: Vec<String> = got
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
        types,
        [
            "thread_started:",
            "turn_started:",
            "timeline:user_message",
            "timeline:tool_call",
            "permission_requested:",
            "permission_resolved:",
            "timeline:tool_call",
            "usage_updated:",
            "timeline:assistant_message",
            "usage_updated:",
            "turn_completed:",
        ]
    );
    assert_eq!(
        compact(&got[3]),
        compact(&json!({"type": "timeline", "provider": "codex", "item": {
            "type": "tool_call", "callId": "call_echo", "name": "shell", "status": "running", "error": null,
            "detail": {"type": "shell", "command": "echo hi", "cwd": cwd}
        }, "turnId": "codex-turn-0"}))
    );
    assert_eq!(
        compact(&got[4]),
        compact(
            &json!({"type": "permission_requested", "provider": "codex", "request": request, "turnId": "codex-turn-0"})
        )
    );
    assert_eq!(
        compact(&got[5]),
        compact(
            &json!({"type": "permission_resolved", "provider": "codex", "requestId": "permission-call_echo",
            "resolution": {"behavior": "allow"}, "turnId": "codex-turn-0"})
        )
    );
    assert_eq!(
        compact(&got[6]),
        compact(&json!({"type": "timeline", "provider": "codex", "item": {
            "type": "tool_call", "callId": "call_echo", "name": "shell", "status": "completed", "error": null,
            "detail": {"type": "shell", "command": "echo hi", "cwd": cwd, "output": "hi\n", "exitCode": 0}
        }, "turnId": "codex-turn-0"}))
    );
    assert_eq!(turn.session.unported(), Vec::<String>::new());
    turn.session.close().expect("close");
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn denied_command_emits_the_failed_tool_call_and_declines() {
    let codex = support::real_codex();
    let turn = start_turn("deny", vec![escalated_echo(), done()], &codex);
    let request = turn.session.pending_permissions()[0].clone();
    turn.session
        .respond_to_permission(
            "permission-call_echo",
            &json!({"behavior": "deny", "message": "Not now"}),
        )
        .expect("deny");
    turn.events.wait_for("turn_completed", WAIT);
    let got = turn.events.snapshot();
    let resolved = got
        .iter()
        .position(|event| event["type"] == "permission_resolved")
        .expect("permission_resolved");
    assert_eq!(
        compact(&got[resolved - 1]),
        compact(&json!({"type": "timeline", "provider": "codex", "item": {
            "type": "tool_call", "callId": "permission-call_echo", "name": "shell", "status": "failed",
            "error": {"message": "Not now"}, "detail": request["detail"],
            "metadata": {"permissionRequestId": "permission-call_echo", "denied": true}
        }, "turnId": "codex-turn-0"}))
    );
    assert_eq!(
        compact(&got[resolved]),
        compact(
            &json!({"type": "permission_resolved", "provider": "codex", "requestId": "permission-call_echo",
            "resolution": {"behavior": "deny", "message": "Not now"}, "turnId": "codex-turn-0"})
        )
    );
    assert_eq!(
        turn.session
            .respond_to_permission("permission-call_echo", &json!({"behavior": "allow"})),
        Err(
            "No pending Codex app-server permission request with id 'permission-call_echo'"
                .to_owned()
        )
    );
    // Paseo answers `decline`: Codex hands the rejection to the model and
    // continues the turn with a second model request.
    let requests = turn.stub.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].body.to_string().contains("rejected by user"),
        "decline reaches the model as a rejection"
    );
    assert_eq!(turn.session.unported(), Vec::<String>::new());
    turn.session.close().expect("close");
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn interrupting_a_turn_waiting_on_approval_cancels_it() {
    let codex = support::real_codex();
    let turn = start_turn("cancel", vec![escalated_echo(), done()], &codex);
    turn.session.interrupt().expect("interrupt");
    let canceled = turn.events.wait_for("turn_canceled", WAIT);
    assert_eq!(
        compact(&canceled),
        compact(
            &json!({"type": "turn_canceled", "provider": "codex", "reason": "interrupted", "turnId": "codex-turn-0"})
        )
    );
    // Paseo leaves the command approval in its pending map after an interrupt.
    assert_eq!(turn.session.pending_permissions().len(), 1);
    assert_eq!(turn.session.unported(), Vec::<String>::new());
    turn.session.close().expect("close");
    assert!(turn.session.pending_permissions().is_empty());
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn deny_with_interrupt_sends_cancel_and_ends_the_turn() {
    let codex = support::real_codex();
    let turn = start_turn("cancel-wire", vec![escalated_echo(), done()], &codex);
    turn.session
        .respond_to_permission(
            "permission-call_echo",
            &json!({"behavior": "deny", "message": "Stop", "interrupt": true}),
        )
        .expect("deny with interrupt");
    let canceled = turn.events.wait_for("turn_canceled", WAIT);
    assert_eq!(canceled["reason"], json!("interrupted"));
    // Paseo answers `cancel`: Codex interrupts the turn instead of telling the
    // model, so no second model request is made.
    assert_eq!(turn.stub.requests().len(), 1);
    assert!(
        turn.events
            .snapshot()
            .iter()
            .all(|event| event["type"] != "turn_completed")
    );
    turn.session.close().expect("close");
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn file_change_approval_matches_paseo_and_applies_on_allow() {
    let codex = support::real_codex();
    let root = DisposableRoot::new("patch");
    // Outside the project and every writable root, so Codex asks first.
    let target = root.join("outside.txt");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+hi\n*** End Patch\n",
        target.display()
    );
    let stub = ResponsesStub::start(vec![
        Reply::CustomToolCall {
            call_id: "call_patch".to_owned(),
            name: "apply_patch".to_owned(),
            input: patch,
        },
        done(),
    ]);
    let provider = stub_provider(&root, &stub, &codex);
    let session = provider
        .create_session(manager_auto_config(&root, &provider), None, false)
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    let thread_id = session.id().expect("thread");
    session
        .start_turn(&Prompt::Text("Write it".to_owned()), &RunOptions::default())
        .expect("start turn");
    let requested = events.wait_for("permission_requested", WAIT);
    let request = &requested["request"];
    assert_eq!(
        compact(request),
        compact(&json!({
            "id": "permission-call_patch",
            "provider": "codex",
            "name": "CodexFileChange",
            "kind": "tool",
            "title": "Apply file changes",
            "detail": {"type": "unknown", "input": {"reason": null}, "output": null},
            "metadata": {"itemId": "call_patch", "threadId": thread_id, "turnId": request["metadata"]["turnId"]},
        }))
    );
    session
        .respond_to_permission("permission-call_patch", &json!({"behavior": "allow"}))
        .expect("allow");
    events.wait_for("turn_completed", WAIT);
    assert_eq!(
        std::fs::read_to_string(&target).expect("patched file"),
        "hi\n"
    );
    // The fileChange item mapping is a recorded gap, not a silent divergence.
    assert_eq!(
        session.unported(),
        ["item/started fileChange", "item/completed fileChange"]
    );
    session.close().expect("close");
}
