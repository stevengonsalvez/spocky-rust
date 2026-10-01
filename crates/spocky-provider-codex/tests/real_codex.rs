//! Drives the real pinned `codex app-server` (0.159.0) against the scripted
//! local Responses stub. Expected events follow pinned Paseo's mapping of the
//! exact notification sequence Codex 0.159.0 emits for a streamed reply.

mod support;

use std::process::{Command, ExitStatus};
use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{Prompt, RunOptions};
use support::{
    DisposableRoot, Events, Reply, ResponsesStub, full_access_config, manager_full_access_config,
    stub_provider,
};

const WAIT: Duration = Duration::from_secs(90);

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
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn happy_path_turns_emit_paseo_events_in_paseo_key_order() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![
        message("msg_stub_1", &["Hello", " from stub."]),
        message("msg_stub_2", &["Second", " reply."]),
    ]);
    let root = DisposableRoot::new("happy");
    let provider = stub_provider(&root, &stub, &codex);
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
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn interrupt_cancels_the_active_turn() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![Reply::Hold]);
    let root = DisposableRoot::new("interrupt");
    let provider = stub_provider(&root, &stub, &codex);
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
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn app_server_exit_mid_turn_fails_the_turn() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![Reply::Hold]);
    let root = DisposableRoot::new("exit");
    let provider = stub_provider(&root, &stub, &codex);
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
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn turn_start_without_a_model_is_rejected_by_codex() {
    // Without the agent manager's catalog model the collaboration mode
    // settings carry no `model`; pinned Paseo sends the same params and
    // Codex 0.159.0 rejects them.
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![]);
    let root = DisposableRoot::new("no-model");
    let provider = stub_provider(&root, &stub, &codex);
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
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn dropping_an_unclosed_session_stops_its_app_server() {
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![]);
    let root = DisposableRoot::new("drop");
    let provider = stub_provider(&root, &stub, &codex);
    let session = provider
        .create_session(full_access_config(&root), None, false)
        .expect("create session");
    let pid = session.app_server_pid().expect("pid");
    assert!(support::process_alive(pid));
    drop(session);
    assert!(!support::process_alive(pid), "codex app-server leaked");
}

#[test]
#[ignore = "drives the pinned codex binary; run with --ignored"]
fn a_subscriber_can_interrupt_from_inside_turn_started() {
    // Subscribers run off the stdout reader, so one that calls back into the
    // session (here `interrupt`, which waits on a Codex response) completes.
    let codex = support::real_codex();
    let stub = ResponsesStub::start(vec![Reply::Hold]);
    let root = DisposableRoot::new("reentrant");
    let provider = stub_provider(&root, &stub, &codex);
    let session = provider
        .create_session(manager_full_access_config(&root, &provider), None, false)
        .expect("create session");
    session.runtime_info().expect("runtime info");
    let events = Events::attach(&session);
    let interrupted = std::sync::Arc::new(std::sync::Mutex::new(None));
    let reentrant = session.clone();
    let result = std::sync::Arc::clone(&interrupted);
    session.subscribe(std::sync::Arc::new(move |event: &Value| {
        if event["type"] == "turn_started" {
            *result.lock().unwrap() = Some(reentrant.interrupt());
        }
    }));
    session
        .start_turn(&Prompt::Text("Wait".to_owned()), &RunOptions::default())
        .expect("start turn");
    events.wait_for("turn_canceled", WAIT);
    assert_eq!(*interrupted.lock().unwrap(), Some(Ok(())));
    session.close().expect("close");
}

/// Child half of the watchdog tests: a no-op unless one of them re-runs this
/// binary with `SPOCKY_P3_WATCHDOG_CHILD` set to the case to play.
#[test]
fn watchdog_child() {
    match std::env::var("SPOCKY_P3_WATCHDOG_CHILD").as_deref() {
        Ok("abort") => {
            let _watchdog = support::Watchdog::arm("watchdog-child", Duration::from_millis(200));
            std::thread::sleep(Duration::from_secs(30));
        }
        Ok("disarm") => {
            drop(support::Watchdog::arm(
                "watchdog-child",
                Duration::from_millis(200),
            ));
            std::thread::sleep(Duration::from_millis(400));
        }
        Ok("cleanup") => {
            use std::os::unix::process::CommandExt;
            let root = support::DisposableRoot::new("watchdog-cleanup");
            // Stands in for an app-server: a group leader parented by this
            // process, recorded the way the launcher records Codex.
            let leader = Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .expect("spawn group leader");
            std::fs::write(
                root.join(support::APP_SERVER_PIDS),
                format!("{}\n", leader.id()),
            )
            .expect("record pid");
            eprintln!("recorded group {}", leader.id());
            let mut leader = leader;
            std::thread::spawn(move || leader.wait());
            let _watchdog = support::Watchdog::arm_for(
                "watchdog-child",
                Duration::from_millis(200),
                Some(root.path.clone()),
            );
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => {}
    }
}

/// Re-runs one test of this binary with `env` applied, bounded at 20 s,
/// returning its exit status and combined output.
fn rerun(test: &str, extra: &[&str], env: &[(&str, Option<&str>)]) -> (ExitStatus, String) {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", test, "--nocapture"])
        .args(extra)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    let mut child = command.spawn().expect("re-run test binary");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while child.try_wait().expect("wait for child").is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("re-run of {test} did not finish within 20 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Exited: the remaining output is already buffered in the pipes.
    let output = child.wait_with_output().expect("child output");
    let combined = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status, combined)
}

#[test]
fn watchdog_aborts_a_test_past_its_deadline() {
    use std::os::unix::process::ExitStatusExt;
    let (status, output) = rerun(
        "watchdog_child",
        &[],
        &[("SPOCKY_P3_WATCHDOG_CHILD", Some("abort"))],
    );
    assert_eq!(status.signal(), Some(6), "SIGABRT, got {status:?}");
    assert!(
        output.contains("real-codex test 'watchdog-child' exceeded its 200ms deadline; aborting"),
        "{output}"
    );
}

#[test]
fn a_dropped_watchdog_does_not_abort() {
    let (status, output) = rerun(
        "watchdog_child",
        &[],
        &[("SPOCKY_P3_WATCHDOG_CHILD", Some("disarm"))],
    );
    assert!(status.success(), "{status:?}: {output}");
}

#[test]
fn watchdog_stops_recorded_groups_and_deletes_the_root_before_abort() {
    use std::os::unix::process::ExitStatusExt;
    let (status, output) = rerun(
        "watchdog_child",
        &[],
        &[("SPOCKY_P3_WATCHDOG_CHILD", Some("cleanup"))],
    );
    let value_after = |prefix: &str| {
        output
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("no '{prefix}' in {output}"))
            .to_owned()
    };
    let leader: u32 = value_after("recorded group ").parse().expect("pid");
    let root = value_after("disposable root: ");
    let gone = (0..100).any(|_| {
        let alive = support::process_alive(leader);
        if alive {
            std::thread::sleep(Duration::from_millis(20));
        }
        !alive
    });
    if !gone {
        // Clean up by the exact pid the child reported.
        let _ = Command::new("/bin/kill")
            .args(["-s", "KILL", &leader.to_string()])
            .status();
    }
    assert_eq!(status.signal(), Some(6), "SIGABRT, got {status:?}");
    assert!(
        output.contains(&format!(
            "sending SIGTERM to app-server process group {leader}"
        )),
        "{output}"
    );
    assert!(gone, "recorded group leader {leader} survived the watchdog");
    assert!(
        !std::path::Path::new(&root).exists(),
        "disposable root {root} left behind"
    );
}

#[test]
fn real_codex_tests_fail_without_the_opt_in() {
    // Codex is never launched: the gate is the first thing `real_codex` checks.
    let (status, output) = rerun(
        "turn_start_without_a_model_is_rejected_by_codex",
        &["--include-ignored"],
        &[(support::REAL_CODEX_GATE, None)],
    );
    assert!(!status.success(), "{output}");
    assert!(
        output.contains("SPOCKY_REAL_CODEX=1 is required to run real-codex tests"),
        "{output}"
    );
}
