//! Differential check of the SDK stdio layer: the pinned `ClaudeAgentClient`
//! (with the real SDK) and `ClaudeClient` each talk to a fake stream-json
//! Claude Code (`stdio_fake.sh`) that plays scripted frames, including a
//! cancel in the same chunk as its request, hook callbacks and exits. The
//! events each session emits, the results of each step and the lines the
//! fake read from its stdin must be the same text. Normalized: uuids, the
//! SDK's random control request ids, and the scratch directory.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::client::{ClaudeClient, ClaudeClientOptions};
use spocky_provider_claude::local::LocalBoxFuture;
use spocky_provider_claude::session::ResolveBinary;
use spocky_session::agent_sdk::{
    AgentClient, AgentError, AgentPromptInput, SteerActiveTurnOptions, SteerResult,
};

const CHILD_SCENARIO: &str = "stdio-child.json";
const CHILD_OUT: &str = "stdio-child.out";
const FAKE: &str = include_str!("stdio_fake.sh");
const HARNESS: &str = include_str!("stdio_harness.mjs");

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/agent.js",
        "c8e8e12df50c2bb45b5d33d08bb4c070e454b5919a9ad62932ffa89337f57e69",
    ),
    (
        "../../../../node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
        "bf86ef08eff553cb8e64262ab1575c76f9a7f4a7a3b4d5dc723e4f3bc6e568af",
    ),
];

const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"sess-1","permissionMode":"default","model":"claude-opus-4-8"}"#;
const RESULT: &str = r#"{"type":"result","subtype":"success","usage":{"input_tokens":1,"cache_read_input_tokens":0,"output_tokens":1},"total_cost_usd":0,"session_id":"sess-1","uuid":"res-1"}"#;

fn assistant(text: &str) -> String {
    format!(
        r#"{{"type":"assistant","session_id":"sess-1","message":{{"id":"msg-1","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn can_use_tool(id: &str) -> String {
    format!(
        r#"{{"type":"control_request","request_id":"{id}","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"ls"}},"tool_use_id":"toolu_1","permission_suggestions":[]}}}}"#
    )
}

fn cancel(id: &str) -> String {
    format!(r#"{{"type":"control_cancel_request","request_id":"{id}"}}"#)
}

fn hook(id: &str) -> String {
    format!(
        r#"{{"type":"control_request","request_id":"{id}","request":{{"subtype":"hook_callback","callback_id":"hook_0","input":{{"hook_event_name":"PreToolUse","session_id":"sess-1","transcript_path":"/t","cwd":"/c","tool_name":"Bash","tool_input":{{"command":"ls"}},"tool_use_id":"toolu_1"}},"tool_use_id":"toolu_1"}}}}"#
    )
}

/// `(name, frames, steps JSON)`.
#[allow(clippy::too_many_lines)] // One literal per scenario.
fn scenarios() -> Vec<(&'static str, Vec<String>, &'static str)> {
    let allow = r#"[{"afterMs":500,"respond":{"behavior":"allow"}}]"#;
    let deny = r#"[{"afterMs":500,"respond":{"behavior":"deny","message":"no","interrupt":true}}]"#;
    vec![
        (
            "basic",
            vec![INIT.into(), assistant("hi"), RESULT.into()],
            "[]",
        ),
        (
            "cancel_same_chunk",
            vec![
                INIT.into(),
                assistant("working"),
                can_use_tool("fake-1"),
                cancel("fake-1"),
                "#flush".into(),
                "#sleep 0.3".into(),
                assistant("after"),
                RESULT.into(),
            ],
            "[]",
        ),
        (
            "cancel_later",
            vec![
                INIT.into(),
                can_use_tool("fake-1"),
                "#flush".into(),
                "#sleep 0.4".into(),
                cancel("fake-1"),
                "#flush".into(),
                "#sleep 0.2".into(),
                RESULT.into(),
            ],
            "[]",
        ),
        (
            "permission_allow",
            vec![
                INIT.into(),
                can_use_tool("fake-1"),
                "#flush".into(),
                "#wait-response".into(),
                assistant("done"),
                RESULT.into(),
            ],
            allow,
        ),
        (
            "permission_deny",
            vec![
                INIT.into(),
                can_use_tool("fake-1"),
                "#flush".into(),
                "#wait-response".into(),
                RESULT.into(),
            ],
            deny,
        ),
        (
            "hook_callback",
            vec![
                INIT.into(),
                hook("fake-2"),
                "#flush".into(),
                "#wait-response".into(),
                assistant("hooked"),
                RESULT.into(),
            ],
            "[]",
        ),
        (
            "steer_accepted",
            vec![
                INIT.into(),
                assistant("working"),
                "#flush".into(),
                "#wait-response".into(),
                assistant("steered"),
                RESULT.into(),
            ],
            r#"[{"afterMs":400,"steer":{"prompt":"change course"}}]"#,
        ),
        (
            "steer_wrong_turn",
            vec![
                INIT.into(),
                assistant("working"),
                "#flush".into(),
                "#sleep 0.5".into(),
                RESULT.into(),
            ],
            r#"[{"afterMs":300,"steer":{"prompt":"change course","otherTurn":true}}]"#,
        ),
        (
            "steer_clears_permission",
            vec![
                INIT.into(),
                can_use_tool("fake-1"),
                "#flush".into(),
                "#wait-response".into(),
                "#wait-response".into(),
                assistant("done"),
                RESULT.into(),
            ],
            r#"[{"afterMs":500,"steer":{"prompt":"answer instead","clear":true}}]"#,
        ),
        (
            "exit_nonzero",
            vec![
                INIT.into(),
                assistant("partial"),
                "#flush".into(),
                "#stderr boom happened".into(),
                "#sleep 0.1".into(),
                "#exit 3".into(),
            ],
            "[]",
        ),
        (
            "exit_zero_without_result",
            vec![INIT.into(), assistant("partial"), "#flush".into(), "#exit 0".into()],
            "[]",
        ),
        (
            "noise",
            vec![
                r#"{"type":"keep_alive"}"#.into(),
                r#"{"type":"transcript_mirror","entries":[]}"#.into(),
                "this is not json".into(),
                INIT.into(),
                assistant("through the noise"),
                RESULT.into(),
            ],
            "[]",
        ),
        (
            "result_error",
            vec![
                INIT.into(),
                r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-1","uuid":"res-e"}"#
                    .into(),
            ],
            "[]",
        ),
    ]
}

fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

/// Uuids become `<uuid>`; the SDK's random request ids `<id>`.
fn normalize(text: &str, scratch: &Path) -> String {
    let text = text.replace(&scratch.to_string_lossy().into_owned(), "<tmp>");
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < characters.len() {
        let slice = characters.get(index..index + 36);
        let uuid = slice.is_some_and(|slice| {
            slice.iter().enumerate().all(|(at, character)| {
                if matches!(at, 8 | 13 | 18 | 23) {
                    *character == '-'
                } else {
                    character.is_ascii_hexdigit()
                }
            })
        });
        if uuid {
            out.push_str("<uuid>");
            index += 36;
        } else {
            out.push(characters[index]);
            index += 1;
        }
    }
    mask_request_ids(&out)
}

/// `"request_id":"<random base36>"`, as the SDK writes it.
fn mask_request_ids(text: &str) -> String {
    const KEY: &str = "\"request_id\":\"";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(KEY) {
        let value_start = start + KEY.len();
        out.push_str(&rest[..value_start]);
        let after = &rest[value_start..];
        let end = after.find('"').unwrap_or(after.len());
        let value = &after[..end];
        let random = value.len() >= 6
            && value.len() <= 13
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase());
        out.push_str(if random { "<id>" } else { value });
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// The run's output: events and results in order, then the fake's stdin.
fn collect(output: &str, fake_log: &str, scratch: &Path) -> String {
    let events: Vec<&str> = output.lines().filter(|l| l.starts_with("EVENT ")).collect();
    let results: Vec<&str> = output
        .lines()
        .filter(|l| l.starts_with("RESULT "))
        .collect();
    normalize(
        &format!(
            "{}\n--- results\n{}\n--- stdin\n{}",
            events.join("\n"),
            results.join("\n"),
            fake_log
        ),
        scratch,
    )
}

fn fixed_env(scratch: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", scratch.join("home")),
        ("PATH", PathBuf::from("/usr/bin:/bin")),
        ("FAKE_FRAMES", scratch.join("frames.txt")),
        ("FAKE_LOG", scratch.join("stdin.log")),
    ]
}

fn run_pinned(node: &std::ffi::OsString, dist: &Path, file: &Path, scratch: &Path) -> String {
    let output = Command::new(timeout_binary())
        .env_clear()
        .envs(fixed_env(scratch))
        .args(["--kill-after=5", "60"])
        .arg(node)
        .args(["--input-type=module", "-e", HARNESS])
        .arg(dist)
        .arg(file)
        .arg(scratch.join("fake-claude"))
        .current_dir(scratch)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8")
}

fn run_rust(file: &Path, scratch: &Path) -> String {
    std::fs::copy(file, scratch.join(CHILD_SCENARIO)).expect("child scenario");
    let out = scratch.join(CHILD_OUT);
    let _ = std::fs::remove_file(&out);
    let output = Command::new(timeout_binary())
        .env_clear()
        .envs(fixed_env(scratch))
        .args(["--kill-after=5", "60"])
        .arg(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "child_runs_the_scenario",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(scratch)
        .output()
        .expect("run the child");
    assert!(
        output.status.success(),
        "child failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    std::fs::read_to_string(out).unwrap_or_default()
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

/// The child half: runs the scenario in its working directory against
/// `ClaudeClient` with the fake binary. Does nothing outside the differential.
#[test]
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // One scripted run; scenario millisecond literals.
fn child_runs_the_scenario() {
    let Ok(text) = std::fs::read_to_string(CHILD_SCENARIO) else {
        return;
    };
    let scenario = parse(&text).expect("JSON");
    let fake = std::env::current_dir().expect("cwd").join("fake-claude");
    let fake = fake.to_string_lossy().into_owned();
    let binary: Arc<dyn Fn() -> ResolveBinary + Send + Sync> = Arc::new(move || {
        let fake = fake.clone();
        std::rc::Rc::new(move || {
            let fake = fake.clone();
            let future: LocalBoxFuture<'static, Result<String, AgentError>> =
                Box::pin(async move { Ok(fake) });
            future
        })
    });
    let client = ClaudeClient::new(ClaudeClientOptions {
        resolve_binary: Some(binary),
        ..ClaudeClientOptions::default()
    });
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = client
            .create_session(
                scenario.get("config").cloned().unwrap_or(JsValue::Null),
                None,
                None,
            )
            .await
            .expect("a session");
        let events = Arc::clone(&lines);
        let _unsubscribe = session.subscribe(Arc::new(move |event| {
            events
                .lock()
                .expect("lines")
                .push(format!("EVENT {}", stringify(&event)));
        }));
        let prompt = scenario
            .get("prompt")
            .and_then(JsValue::as_str)
            .unwrap_or("hello")
            .to_owned();
        let started = session
            .start_turn(AgentPromptInput::Text(prompt), None)
            .await;
        let mut active_turn = String::new();
        let result = match started {
            Ok(turn_id) => {
                active_turn.clone_from(&turn_id);
                stringify(&object(vec![("turnId", JsValue::String(turn_id))]))
            }
            Err(error) => format!("ERROR {}", error.message),
        };
        lines
            .lock()
            .expect("lines")
            .push(format!("RESULT {result}"));
        for step in scenario
            .get("steps")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
        {
            let after = step
                .get("afterMs")
                .and_then(JsValue::as_f64)
                .unwrap_or(300.0);
            tokio::time::sleep(Duration::from_millis(after as u64)).await;
            if let Some(steer) = step.get("steer") {
                let options = SteerActiveTurnOptions {
                    expected_turn_id: if steer.get("otherTurn") == Some(&JsValue::Bool(true)) {
                        "not-the-active-turn".to_owned()
                    } else {
                        active_turn.clone()
                    },
                    clear_pending_permissions: steer.get("clear").and_then(JsValue::as_bool),
                    ..SteerActiveTurnOptions::default()
                };
                let text = steer
                    .get("prompt")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let outcome = match session
                    .steer_active_turn(&AgentPromptInput::Text(text), &options)
                    .expect("claude steers")
                    .await
                {
                    Ok(steered) => {
                        let status = match steered {
                            SteerResult::Accepted => "accepted",
                            SteerResult::Unavailable => "unavailable",
                        };
                        stringify(&object(vec![(
                            "status",
                            JsValue::String(status.to_owned()),
                        )]))
                    }
                    Err(error) => format!("ERROR {}", error.message),
                };
                lines
                    .lock()
                    .expect("lines")
                    .push(format!("RESULT {outcome}"));
                continue;
            }
            if let Some(response) = step.get("respond") {
                let pending = session.get_pending_permissions().unwrap_or_default();
                let Some(first) = pending.first() else {
                    lines
                        .lock()
                        .expect("lines")
                        .push("RESULT no pending permission".to_owned());
                    continue;
                };
                let id = first
                    .get("id")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let outcome = match session.respond_to_permission(&id, response.clone()).await {
                    Ok(value) => stringify(&value.unwrap_or(JsValue::Null)),
                    Err(error) => format!("ERROR {}", error.message),
                };
                lines
                    .lock()
                    .expect("lines")
                    .push(format!("RESULT {outcome}"));
            }
        }
        let wait = scenario
            .get("waitMs")
            .and_then(JsValue::as_f64)
            .unwrap_or(800.0);
        tokio::time::sleep(Duration::from_millis(wait as u64)).await;
        let _ = session.close().await;
    });
    let log = lines.lock().expect("lines").join("\n") + "\n";
    std::fs::write(CHILD_OUT, log).expect("write");
}

#[test]
#[allow(clippy::too_many_lines)] // One block per scenario.
fn stdio_matches_the_pinned_sdk() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp")
        .join(format!("spocky-stdio-diff-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("home")).expect("scratch");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let fake = scratch.join("fake-claude");
    std::fs::write(&fake, FAKE).expect("fake");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let config = format!(
        r#"{{"provider":"claude","cwd":"{}","modeId":"default","model":"claude-opus-4-8"}}"#,
        cwd.to_string_lossy()
    );
    let mut failures = Vec::new();
    for (name, frames, steps) in scenarios() {
        // The turn's own user message is emitted by a `setTimeout(0)`: the fake
        // answers after it, so the two never race.
        std::fs::write(
            scratch.join("frames.txt"),
            format!("#sleep 0.15\n{}\n", frames.join("\n")),
        )
        .expect("frames");
        let spec = format!(r#"{{"config":{config},"steps":{steps}}}"#);
        let file = scratch.join(format!("{name}.json"));
        std::fs::write(&file, &spec).expect("scenario");
        let expected_output = run_pinned(&node, &dist, &file, &scratch);
        let expected_log = std::fs::read_to_string(scratch.join("stdin.log")).unwrap_or_default();
        let expected = collect(&expected_output, &expected_log, &scratch);
        let actual_output = run_rust(&file, &scratch);
        let actual_log = std::fs::read_to_string(scratch.join("stdin.log")).unwrap_or_default();
        let actual = collect(&actual_output, &actual_log, &scratch);
        assert!(
            expected.contains("EVENT "),
            "{name}: the pinned run produced no events:\n{expected}"
        );
        if expected != actual {
            let mut node_lines = expected.lines();
            let mut rust_lines = actual.lines();
            let mut line = 0;
            let difference = loop {
                match (node_lines.next(), rust_lines.next()) {
                    (None, None) => break "same".to_owned(),
                    (node, rust) if node == rust => line += 1,
                    (node, rust) => {
                        break format!(
                            "line {line}\n  node: {}\n  rust: {}",
                            node.unwrap_or("<end>"),
                            rust.unwrap_or("<end>")
                        );
                    }
                }
            };
            failures.push(format!("{name}: {difference}"));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} scenarios differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
