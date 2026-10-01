//! Notification fixtures ported from pinned Paseo
//! `codex-app-server-agent.test.ts`. Each test names the Paseo test it ports.
//! The session is primed as Paseo's `createSession()` helper does (connected,
//! thread `test-thread`, foreground turn `test-turn`) and fed notifications
//! directly, as the Paseo tests call `handleNotification`. Expected events are
//! compared as serialized JSON, so key order is checked as well.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use spocky_provider_codex::session::{CodexSession, SessionOptions};
use spocky_provider_codex::{CodexGates, SessionConfig};

fn create_session(foreground_turn: Option<&str>) -> (CodexSession, Arc<Mutex<Vec<Value>>>) {
    let session = CodexSession::new(SessionOptions {
        config: SessionConfig {
            cwd: "/tmp/codex-question-test".to_owned(),
            mode_id: Some("auto".to_owned()),
            model: Some("gpt-5.4".to_owned()),
            ..SessionConfig::default()
        },
        spawn: Box::new(|| Err("Test session cannot spawn Codex app-server".to_owned())),
        custom_codex_config: None,
        ephemeral: false,
        gates: CodexGates {
            goals_enabled: false,
            auto_review_enabled: false,
        },
    })
    .expect("session");
    session.prime_for_notification_test("test-thread", foreground_turn);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    session.subscribe(Arc::new(move |event: &Value| {
        sink.lock().unwrap().push(event.clone());
    }));
    (session, events)
}

fn notify(session: &CodexSession, method: &str, params: &Value) {
    session.receive_notification(method, Some(params));
}

fn assert_events(events: &Arc<Mutex<Vec<Value>>>, expected: &[Value]) {
    let got: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect();
    let expected: Vec<String> = expected
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect();
    assert_eq!(got, expected);
}

fn timeline(item: &Value, turn_id: &str) -> Value {
    json!({"type": "timeline", "provider": "codex", "item": item, "turnId": turn_id})
}

// Paseo: "emits usage_updated on token usage updates and keeps usage on turn completion".
#[test]
fn usage_updated_then_turn_completed_with_usage() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "thread/tokenUsage/updated",
        &json!({"tokenUsage": {
            "model_context_window": 200_000,
            "last": {"total_tokens": 50000, "inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}
        }}),
    );
    notify(
        &session,
        "turn/completed",
        &json!({"turn": {"status": "completed", "error": null}}),
    );
    let usage = json!({
        "inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000,
        "contextWindowMaxTokens": 200_000, "contextWindowUsedTokens": 50000
    });
    assert_events(
        &events,
        &[
            json!({"type": "usage_updated", "provider": "codex", "usage": usage, "turnId": "test-turn"}),
            json!({"type": "turn_completed", "provider": "codex", "usage": usage, "turnId": "test-turn"}),
        ],
    );
}

// Paseo: "streams Codex assistant message deltas and does not replay completed text".
#[test]
fn assistant_deltas_are_not_replayed_on_completion() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "assistant-item-1", "delta": "Hel"}),
    );
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "assistant-item-1", "delta": "lo"}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"item": {"id": "assistant-item-1", "type": "agentMessage", "text": "Hello"}}),
    );
    assert_events(
        &events,
        &[
            timeline(
                &json!({"type": "assistant_message", "messageId": "assistant-item-1", "text": "Hel"}),
                "test-turn",
            ),
            timeline(
                &json!({"type": "assistant_message", "messageId": "assistant-item-1", "text": "lo"}),
                "test-turn",
            ),
        ],
    );
}

// Paseo: "emits only the missing assistant suffix when completed text extends streamed deltas".
#[test]
fn assistant_completion_emits_only_the_missing_suffix() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "assistant-item-2", "delta": "Hel"}),
    );
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "assistant-item-2", "delta": "lo"}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"item": {"id": "assistant-item-2", "type": "agentMessage", "text": "Hello!"}}),
    );
    assert_events(
        &events,
        &[
            timeline(
                &json!({"type": "assistant_message", "messageId": "assistant-item-2", "text": "Hel"}),
                "test-turn",
            ),
            timeline(
                &json!({"type": "assistant_message", "messageId": "assistant-item-2", "text": "lo"}),
                "test-turn",
            ),
            timeline(
                &json!({"type": "assistant_message", "text": "!", "messageId": "assistant-item-2"}),
                "test-turn",
            ),
        ],
    );
}

// Paseo: "emits a markdown divider when a new Codex assistant item starts after the previous one completed".
#[test]
fn second_assistant_item_starts_with_a_markdown_divider() {
    let first = "I’m in the waiting phase now. The next read is intentionally delayed so we get meaningful CI state instead of churn.";
    let second = "CI is still cooking. I’m staying on the current run rather than jumping around, because the first red job will tell us exactly whether anything else needs work.";
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "assistant-item-3", "delta": first}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"item": {"id": "assistant-item-3", "type": "agentMessage", "text": first}}),
    );
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "assistant-item-4", "delta": second}),
    );
    assert_events(
        &events,
        &[
            timeline(
                &json!({"type": "assistant_message", "messageId": "assistant-item-3", "text": first}),
                "test-turn",
            ),
            timeline(
                &json!({"type": "assistant_message", "messageId": "assistant-item-4", "text": format!("\n\n---\n\n{second}")}),
                "test-turn",
            ),
        ],
    );
}

// Paseo: "streams Codex reasoning deltas and does not replay completed reasoning".
#[test]
fn reasoning_deltas_are_not_replayed_on_completion() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/reasoning/summaryTextDelta",
        &json!({"itemId": "reasoning-item-1", "delta": "Think"}),
    );
    notify(
        &session,
        "item/reasoning/summaryTextDelta",
        &json!({"itemId": "reasoning-item-1", "delta": "ing"}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"item": {"id": "reasoning-item-1", "type": "reasoning", "summary": ["Thinking"]}}),
    );
    assert_events(
        &events,
        &[
            timeline(&json!({"type": "reasoning", "text": "Think"}), "test-turn"),
            timeline(&json!({"type": "reasoning", "text": "ing"}), "test-turn"),
        ],
    );
}

// Paseo: "emits only the missing reasoning suffix when completed reasoning extends streamed deltas".
#[test]
fn reasoning_completion_emits_only_the_missing_suffix() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/reasoning/summaryTextDelta",
        &json!({"itemId": "reasoning-item-2", "delta": "Think"}),
    );
    notify(
        &session,
        "item/reasoning/summaryTextDelta",
        &json!({"itemId": "reasoning-item-2", "delta": "ing"}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"item": {"id": "reasoning-item-2", "type": "reasoning", "summary": ["Thinking!"]}}),
    );
    assert_events(
        &events,
        &[
            timeline(&json!({"type": "reasoning", "text": "Think"}), "test-turn"),
            timeline(&json!({"type": "reasoning", "text": "ing"}), "test-turn"),
            timeline(&json!({"type": "reasoning", "text": "!"}), "test-turn"),
        ],
    );
}

// Paseo: "captures live Codex user message ids from item events".
#[test]
fn live_user_message_is_emitted_once_with_its_codex_id() {
    let (session, events) = create_session(Some("test-turn"));
    let item = json!({
        "type": "userMessage",
        "id": "codex-user-live-1",
        "content": [{"type": "text", "text": "Use the native Codex id."}]
    });
    notify(
        &session,
        "item/started",
        &json!({"threadId": "test-thread", "item": item}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"threadId": "test-thread", "item": item}),
    );
    assert_events(
        &events,
        &[timeline(
            &json!({"type": "user_message", "text": "Use the native Codex id.", "messageId": "codex-user-live-1"}),
            "test-turn",
        )],
    );
}

// Paseo: "emits Codex context compaction markers from live thread items".
#[test]
fn live_compaction_item_emits_loading_then_completed() {
    let (session, events) = create_session(Some("test-turn"));
    let item = json!({"type": "contextCompaction", "id": "compact-live"});
    notify(
        &session,
        "item/started",
        &json!({"threadId": "test-thread", "item": item}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"threadId": "test-thread", "item": item}),
    );
    assert_events(
        &events,
        &[
            timeline(
                &json!({"type": "compaction", "status": "loading"}),
                "test-turn",
            ),
            timeline(
                &json!({"type": "compaction", "status": "completed"}),
                "test-turn",
            ),
        ],
    );
}

fn compaction_summary(events: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
    events
        .lock()
        .unwrap()
        .iter()
        .map(|event| {
            if event["type"] == "timeline" {
                format!(
                    "{}:{}",
                    event["item"]["type"].as_str().unwrap(),
                    event["item"]["status"].as_str().unwrap()
                )
            } else {
                event["type"].as_str().unwrap().to_owned()
            }
        })
        .collect()
}

fn starts_compaction(session: &CodexSession, item_id: &str) {
    notify(
        session,
        "item/started",
        &json!({"threadId": "test-thread", "item": {"type": "contextCompaction", "id": item_id}}),
    );
}

fn completes_compaction(session: &CodexSession, item_id: &str) {
    notify(
        session,
        "item/completed",
        &json!({"threadId": "test-thread", "item": {"type": "contextCompaction", "id": item_id}}),
    );
}

fn complete_turn(session: &CodexSession, status: &str, error: &Value) {
    notify(
        session,
        "turn/completed",
        &json!({"threadId": "test-thread", "turn": {"id": "turn-1", "status": status, "error": error}}),
    );
}

// Paseo: "completes a pending Codex compaction when its turn ends".
#[test]
fn pending_compaction_completes_when_the_turn_ends() {
    let (session, events) = create_session(Some("codex-turn-0"));
    starts_compaction(&session, "compact-without-completion");
    complete_turn(&session, "completed", &Value::Null);
    assert_eq!(
        compaction_summary(&events),
        [
            "compaction:loading",
            "compaction:completed",
            "turn_completed"
        ]
    );
}

// Paseo: "does not complete a Codex compaction twice when its item finishes before the turn".
#[test]
fn completed_compaction_is_not_completed_again_by_the_turn() {
    let (session, events) = create_session(Some("codex-turn-0"));
    starts_compaction(&session, "compact-completed-normally");
    completes_compaction(&session, "compact-completed-normally");
    complete_turn(&session, "completed", &Value::Null);
    assert_eq!(
        compaction_summary(&events),
        [
            "compaction:loading",
            "compaction:completed",
            "turn_completed"
        ]
    );
}

// Paseo: "does not let a late compaction completion consume the current pending item".
#[test]
fn late_completion_does_not_consume_the_current_compaction() {
    let (session, events) = create_session(Some("codex-turn-0"));
    starts_compaction(&session, "current-compaction");
    completes_compaction(&session, "older-compaction");
    complete_turn(&session, "completed", &Value::Null);
    assert_eq!(
        compaction_summary(&events),
        [
            "compaction:loading",
            "compaction:completed",
            "turn_completed"
        ]
    );
}

// Paseo: "completes a pending compaction before a $status turn" (failed, interrupted).
#[test]
fn pending_compaction_completes_before_failed_and_interrupted_turns() {
    for (status, error, terminal) in [
        (
            "failed",
            json!({"message": "Compaction failed"}),
            "turn_failed",
        ),
        ("interrupted", Value::Null, "turn_canceled"),
    ] {
        let (session, events) = create_session(Some("codex-turn-0"));
        starts_compaction(&session, &format!("compact-{status}"));
        complete_turn(&session, status, &error);
        assert_eq!(
            compaction_summary(&events),
            ["compaction:loading", "compaction:completed", terminal]
        );
    }
}

// Paseo: "emits and dedupes Codex thread/compacted notifications".
#[test]
fn thread_compacted_is_deduped_against_the_completed_item() {
    let (session, events) = create_session(None);
    notify(
        &session,
        "thread/compacted",
        &json!({"threadId": "test-thread", "turnId": "legacy-compact-turn"}),
    );
    completes_compaction(&session, "legacy-compact-item");
    assert_events(
        &events,
        &[timeline(
            &json!({"type": "compaction", "status": "completed"}),
            "legacy-compact-turn",
        )],
    );
}

// Paseo: "emits consecutive Codex thread/compacted notifications".
#[test]
fn consecutive_thread_compacted_notifications_each_emit() {
    let (session, events) = create_session(None);
    for turn in ["legacy-compact-turn-1", "legacy-compact-turn-2"] {
        notify(
            &session,
            "thread/compacted",
            &json!({"threadId": "test-thread", "turnId": turn}),
        );
    }
    assert_events(
        &events,
        &[
            timeline(
                &json!({"type": "compaction", "status": "completed"}),
                "legacy-compact-turn-1",
            ),
            timeline(
                &json!({"type": "compaction", "status": "completed"}),
                "legacy-compact-turn-2",
            ),
        ],
    );
}

#[test]
fn failed_turn_uses_the_codex_error_or_the_paseo_fallback() {
    let (session, events) = create_session(Some("test-turn"));
    complete_turn(&session, "failed", &Value::Null);
    assert_events(
        &events,
        &[
            json!({"type": "turn_failed", "provider": "codex", "error": "Codex turn failed", "turnId": "test-turn"}),
        ],
    );
}

#[test]
fn turn_started_resets_the_assistant_boundary_and_is_tagged() {
    let (session, events) = create_session(Some("codex-turn-0"));
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "a", "delta": "x"}),
    );
    notify(
        &session,
        "item/completed",
        &json!({"item": {"id": "a", "type": "agentMessage", "text": "x"}}),
    );
    notify(
        &session,
        "turn/started",
        &json!({"threadId": "test-thread", "turn": {"id": "native-turn"}}),
    );
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"itemId": "b", "delta": "y"}),
    );
    let got = events.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_string(&got[1]).unwrap(),
        r#"{"type":"turn_started","provider":"codex","turnId":"codex-turn-0"}"#
    );
    assert_eq!(got[2]["item"]["text"], json!("y"));
}

#[test]
fn notifications_for_other_threads_are_reported_unported() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/agentMessage/delta",
        &json!({"threadId": "child-thread", "itemId": "c", "delta": "z"}),
    );
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(
        session.unported(),
        ["sub-agent thread notification agent_message_delta"]
    );
}

#[test]
fn tool_items_are_reported_unported_instead_of_dropped() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/started",
        &json!({"threadId": "test-thread", "item": {"type": "fileChange", "id": "patch"}}),
    );
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(session.unported(), ["item/started fileChange"]);
}

fn silent_command_item() -> Value {
    timeline(
        &json!({
            "type": "tool_call", "callId": "silent-merge", "name": "shell", "status": "completed",
            "error": null,
            "detail": {"type": "shell", "command": "gh pr merge 2030 --squash", "cwd": "/workspace/project", "exitCode": 0}
        }),
        "test-turn",
    )
}

// Paseo: "shows a successful shell command that produces no output".
#[test]
fn silent_completed_command_item_is_shown() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "item/completed",
        &json!({"threadId": "test-thread", "item": {
            "type": "commandExecution", "id": "silent-merge", "status": "completed",
            "command": "gh pr merge 2030 --squash", "cwd": "/workspace/project",
            "aggregatedOutput": null, "exitCode": 0
        }}),
    );
    assert_events(&events, &[silent_command_item()]);
    assert!(session.unported().is_empty());
}

// Paseo: "shows a silent shell command from legacy live notifications".
#[test]
fn silent_legacy_exec_command_end_is_shown() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "codex/event/exec_command_end",
        &json!({"threadId": "test-thread", "msg": {
            "type": "exec_command_end", "call_id": "silent-merge",
            "command": "gh pr merge 2030 --squash", "cwd": "/workspace/project",
            "aggregatedOutput": null, "exit_code": 0, "success": true
        }}),
    );
    assert_events(&events, &[silent_command_item()]);
}

#[test]
fn legacy_exec_end_is_authoritative_over_the_command_item() {
    let (session, events) = create_session(Some("test-turn"));
    notify(
        &session,
        "codex/event/exec_command_begin",
        &json!({"msg": {"type": "exec_command_begin", "call_id": "c1", "command": ["/bin/zsh", "-lc", "ls"], "cwd": "/w"}}),
    );
    notify(
        &session,
        "codex/event/exec_command_output_delta",
        &json!({"msg": {"type": "exec_command_output_delta", "call_id": "c1", "chunk": "YQo="}}),
    );
    notify(
        &session,
        "codex/event/exec_command_end",
        &json!({"msg": {"type": "exec_command_end", "call_id": "c1", "command": ["/bin/zsh", "-lc", "ls"], "cwd": "/w", "exit_code": 0}}),
    );
    let item = json!({"type": "commandExecution", "id": "c1", "command": "/bin/zsh -lc ls", "cwd": "/w", "status": "completed", "aggregatedOutput": "a\n", "exitCode": 0});
    notify(&session, "item/started", &json!({"item": item}));
    notify(&session, "item/completed", &json!({"item": item}));
    assert_events(
        &events,
        &[
            timeline(
                &json!({"type": "tool_call", "callId": "c1", "name": "shell", "status": "running", "error": null,
                    "detail": {"type": "shell", "command": "ls", "cwd": "/w"}}),
                "test-turn",
            ),
            timeline(
                &json!({"type": "tool_call", "callId": "c1", "name": "shell", "status": "completed", "error": null,
                    "detail": {"type": "shell", "command": "ls", "cwd": "/w", "output": "a\n", "exitCode": 0}}),
                "test-turn",
            ),
        ],
    );
}
