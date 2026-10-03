//! Spawn differential: the pinned `ClaudeAgentClient` (with the real SDK) and
//! this crate's `ClaudeClient` each launch a recorder in place of the Claude
//! binary, and the argv, working directory, environment and first stdin lines
//! the recorder sees must be the same text. Normalized: the scratch directory
//! (`<tmp>`), UUIDs (`<uuid>`) and control request ids (`<id>`), each pinned by
//! `normalization_covers_every_generated_value`. Both sides see the same
//! environment, `NoDefaultCurrentDirectoryInExePath=1` included: the pinned SDK
//! sets it in `process.env` when imported and `process_env()` carries it. Only
//! one line is dropped, from the pinned record: macOS adds
//! `__CF_USER_TEXT_ENCODING` to a process started with a cleared environment
//! (a CoreFoundation artifact, not adapter behavior).

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::client::{ClaudeClient, ClaudeClientOptions};
use spocky_provider_claude::local::LocalBoxFuture;
use spocky_provider_claude::session::ResolveBinary;
use spocky_session::agent_sdk::{AgentClient, AgentPromptInput};

/// The child run finds its scenario here, in its working directory, and
/// writes what it observed to `CHILD_OUT`: files, not environment variables,
/// so the child's environment is the pinned run's.
const CHILD_SCENARIO: &str = "spawn-child.json";
const CHILD_OUT: &str = "spawn-child.out";

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/agent.js",
        "c8e8e12df50c2bb45b5d33d08bb4c070e454b5919a9ad62932ffa89337f57e69",
    ),
    (
        "../../../../node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
        "bf86ef08eff553cb8e64262ab1575c76f9a7f4a7a3b4d5dc723e4f3bc6e568af",
    ),
    (
        "server/agent/providers/claude/options.js",
        "3f63e785c60e02b8e094487807e34143c4dd8c331b0c5d58b33e09dbf0ce1791",
    ),
];

const RECORDER: &str = include_str!("spawn_recorder.sh");
const HARNESS: &str = include_str!("spawn_harness.mjs");

/// The `timeout` binary as an absolute path, found on the parent's `PATH`.
fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

/// The environment both builds start from: nothing but these.
fn fixed_env(scratch: &Path) -> Vec<(OsString, OsString)> {
    let home = scratch.join("home");
    vec![
        ("HOME".into(), home.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LANG".into(), "C".into()),
        ("RECORD_FILE".into(), scratch.join("record.txt").into()),
        ("RECORD_STDIN".into(), scratch.join("stdin.txt").into()),
    ]
}

/// `text` with the scratch directory, uuids and request ids replaced.
fn normalize(text: &str, scratch: &Path) -> String {
    let text = text.replace(&scratch.to_string_lossy().into_owned(), "<tmp>");
    let mut out = String::new();
    let characters: Vec<char> = text.chars().collect();
    let shape = [8, 4, 4, 4, 12];
    let mut index = 0;
    while index < characters.len() {
        let mut cursor = index;
        let matched = shape.iter().enumerate().all(|(group, length)| {
            if group > 0 {
                if characters.get(cursor) != Some(&'-') {
                    return false;
                }
                cursor += 1;
            }
            let hex = (0..*length).all(|offset| {
                characters
                    .get(cursor + offset)
                    .is_some_and(char::is_ascii_hexdigit)
            });
            cursor += length;
            hex
        });
        if matched {
            out.push_str("<uuid>");
            index = cursor;
        } else {
            out.push(characters[index]);
            index += 1;
        }
    }
    normalize_request_ids(&out)
}

/// `"request_id":"…"` becomes `"request_id":"<id>"`.
fn normalize_request_ids(text: &str) -> String {
    const KEY: &str = "\"request_id\":\"";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(KEY) {
        let value_start = start + KEY.len();
        out.push_str(&rest[..value_start]);
        let Some(end) = rest[value_start..].find('"') else {
            rest = &rest[value_start..];
            break;
        };
        out.push_str("<id>");
        rest = &rest[value_start + end..];
    }
    out.push_str(rest);
    out
}

/// What the recorder wrote, normalized; the files are removed afterwards.
fn read_record(scratch: &Path) -> String {
    let record = std::fs::read_to_string(scratch.join("record.txt")).unwrap_or_default();
    let stdin = std::fs::read_to_string(scratch.join("stdin.txt")).unwrap_or_default();
    let _ = std::fs::remove_file(scratch.join("record.txt"));
    let _ = std::fs::remove_file(scratch.join("stdin.txt"));
    normalize(&format!("{record}STDIN-BEGIN\n{stdin}STDIN-END\n"), scratch)
}

fn run_pinned(node: &OsString, dist: &Path, scenario_file: &Path, scratch: &Path) -> String {
    let recorder = scratch.join("recorder.sh");
    let output = Command::new(timeout_binary())
        .env_clear()
        .envs(fixed_env(scratch))
        .args(["--kill-after=5", "60"])
        .arg(node)
        .args(["--input-type=module", "-e", HARNESS])
        .arg(dist)
        .arg(scenario_file)
        .arg(recorder)
        .current_dir(scratch)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let observed = observation(scenario_file);
    assert!(
        observed || !stdout.contains("ERROR"),
        "pinned run failed: {stdout}"
    );
    let observation = if observed {
        normalize(&stdout, scratch)
    } else {
        String::new()
    };
    let record = read_record(scratch)
        .lines()
        .filter(|line| !line.starts_with("__CF_USER_TEXT_ENCODING="))
        .fold(String::new(), |mut text, line| {
            text.push_str(line);
            text.push('\n');
            text
        });
    format!("{record}{observation}")
}

/// Whether the scenario also compares what the session reported.
fn observation(scenario_file: &Path) -> bool {
    std::fs::read_to_string(scenario_file)
        .ok()
        .and_then(|text| parse(&text).ok())
        .is_some_and(|scenario| scenario.get("observe") == Some(&JsValue::Bool(true)))
}

fn run_rust(scenario_file: &Path, scratch: &Path) -> String {
    std::fs::copy(scenario_file, scratch.join(CHILD_SCENARIO)).expect("child scenario");
    let _ = std::fs::remove_file(scratch.join(CHILD_OUT));
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
    let observed = std::fs::read_to_string(scratch.join(CHILD_OUT)).unwrap_or_default();
    format!("{}{}", read_record(scratch), normalize(&observed, scratch))
}

/// The child half: runs one scenario against `ClaudeClient` with the recorder
/// as the binary. Does nothing outside the differential.
#[test]
fn child_runs_the_scenario() {
    let Ok(text) = std::fs::read_to_string(CHILD_SCENARIO) else {
        return;
    };
    let scenario = parse(&text).expect("JSON");
    let recorder = PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .parent()
        .expect("scratch")
        .join("recorder.sh");
    let recorder = recorder.to_string_lossy().into_owned();
    let binary: Arc<dyn Fn() -> ResolveBinary + Send + Sync> = Arc::new(move || {
        let recorder = recorder.clone();
        Rc::new(move || {
            let recorder = recorder.clone();
            let future: LocalBoxFuture<'static, Result<String, _>> =
                Box::pin(async move { Ok(recorder) });
            future
        })
    });
    let client = ClaudeClient::new(ClaudeClientOptions {
        resolve_binary: Some(binary),
        ..ClaudeClientOptions::default()
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = match scenario.get("resume") {
            Some(handle) => {
                client
                    .resume_session(
                        handle.clone(),
                        scenario.get("overrides").cloned(),
                        None,
                        None,
                    )
                    .await
            }
            None => {
                client
                    .create_session(
                        scenario.get("config").cloned().unwrap_or(JsValue::Null),
                        None,
                        None,
                    )
                    .await
            }
        }
        .expect("a session");
        let observed: Arc<Mutex<Vec<String>>> = Arc::default();
        let events = Arc::clone(&observed);
        let _unsubscribe = session.subscribe(Arc::new(move |event| {
            events
                .lock()
                .expect("observed")
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
        observed.lock().expect("observed").push(match started {
            Ok(turn_id) => {
                let mut result = JsObject::new();
                result.insert("turnId", JsValue::String(turn_id));
                format!("RESULT {}", stringify(&JsValue::Object(result)))
            }
            Err(error) => format!("RESULT ERROR {}", error.message),
        });
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = session.close().await;
        if scenario.get("observe") == Some(&JsValue::Bool(true)) {
            let text = observed.lock().expect("observed").join("\n") + "\n";
            std::fs::write(CHILD_OUT, text).expect("write");
        }
    });
}

fn scenarios() -> Vec<(&'static str, String)> {
    let base = |extra: &str| {
        format!(
            r#"{{"provider":"claude","cwd":"<cwd>","modeId":"default","model":"claude-opus-4-8"{extra}}}"#
        )
    };
    let one = |config: String| format!(r#"{{"config":{config}}}"#);
    vec![
        ("default", one(base(""))),
        (
            "plan_thinking",
            one(base(r#","thinkingOptionId":"high""#)
                .replace(r#""modeId":"default""#, r#""modeId":"plan""#)),
        ),
        (
            "provider_options",
            one(base(
                r#","providerOptions":{"allowedTools":["Read"],"disallowedTools":["Bash"],"additionalDirectories":["/tmp/extra"],"extraArgs":{"foo":"bar","flag":null}}"#,
            )),
        ),
        (
            "bypass_permissions",
            one(base("").replace(r#""modeId":"default""#, r#""modeId":"bypassPermissions""#)),
        ),
        (
            "mcp_servers",
            one(base(
                r#","systemPrompt":"be brief","mcpServers":{"files":{"type":"stdio","command":"node","args":["srv.js"],"env":{"A":"1"}}}"#,
            )),
        ),
        ("ultracode", one(base(r#","thinkingOptionId":"ultracode""#))),
        (
            "thinking_disabled",
            one(base(r#","thinkingOptionId":"disabled""#)),
        ),
        (
            "fast_mode",
            one(base(r#","featureValues":{"fast_mode":true}"#)),
        ),
        (
            "no_model_accept_edits",
            one(base("")
                .replace(r#","model":"claude-opus-4-8""#, "")
                .replace(r#""modeId":"default""#, r#""modeId":"acceptEdits""#)),
        ),
        (
            "prompts",
            one(base(
                r#","systemPrompt":"be brief","daemonAppendSystemPrompt":"daemon rules""#,
            )),
        ),
        (
            "sandbox_settings",
            one(base(
                r#","providerOptions":{"sandbox":{"enabled":true,"network":{"allowedDomains":["a.com"],"httpProxyPort":8080},"filesystem":{"denyRead":["/etc"]},"ripgrep":{"command":"rg","args":["-n"]}},"settings":{"permissions":{"allow":["Read"],"deny":["Write"]},"sandbox":{"enabled":true}}}"#,
            )),
        ),
        (
            "tool_policy",
            one(base(
                r#","providerOptions":{"allowedTools":["Read"]},"toolPolicy":{"preapproved":[{"server":"files","tool":"read"},{"server":"files","tool":"read"}]}"#,
            )),
        ),
        (
            "missing_cwd",
            format!(
                r#"{{"observe":true,"config":{}}}"#,
                base("").replace("<cwd>", "<cwd>/missing")
            ),
        ),
        (
            "resume",
            format!(
                r#"{{"resume":{{"provider":"claude","sessionId":"sess-abc","nativeHandle":"sess-abc","metadata":{}}}}}"#,
                base("")
            ),
        ),
    ]
}

#[test]
fn spawns_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = std::env::temp_dir().join(format!("spocky-spawn-diff-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("home")).expect("scratch");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let recorder = scratch.join("recorder.sh");
    std::fs::write(&recorder, RECORDER).expect("recorder");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let mut failures = Vec::new();
    for (name, text) in scenarios() {
        let text = text.replace("<cwd>", &cwd.to_string_lossy());
        let file = scratch.join(format!("{name}.json"));
        std::fs::write(&file, &text).expect("scenario file");
        let expected = run_pinned(&node, &dist, &file, &scratch);
        let actual = run_rust(&file, &scratch);
        if let Some(dump) = std::env::var_os("SPOCKY_SPAWN_DUMP") {
            let dump = PathBuf::from(dump);
            std::fs::write(dump.join(format!("node-{name}.txt")), &expected).expect("dump");
            std::fs::write(dump.join(format!("rust-{name}.txt")), &actual).expect("dump");
        }
        assert!(
            observation(&file) || (expected.contains("ARGC") && expected.contains("ENV-END")),
            "{name}: the pinned run recorded nothing"
        );
        if actual != expected {
            let difference = expected
                .lines()
                .zip(actual.lines())
                .enumerate()
                .find(|(_, (node, rust))| node != rust)
                .map_or_else(
                    || "different length".to_owned(),
                    |(line, (node, rust))| format!("line {line}\n  node: {node}\n  rust: {rust}"),
                );
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

#[test]
fn normalization_covers_every_generated_value() {
    let scratch = Path::new("/var/scratch/x");
    let text = normalize(
        r#"cwd /var/scratch/x/cwd {"request_id":"abc123","uuid":"123e4567-e89b-12d3-a456-426614174000"} keep-this"#,
        scratch,
    );
    assert_eq!(
        text,
        r#"cwd <tmp>/cwd {"request_id":"<id>","uuid":"<uuid>"} keep-this"#
    );
}
