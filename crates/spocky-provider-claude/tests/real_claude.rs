//! Drives the real Claude Code binary through `ClaudeClient` against a
//! scripted Anthropic Messages stub on loopback.
//!
//! Every launch goes through a launcher that runs the binary under a
//! loopback-only seatbelt profile, with a disposable `HOME` and
//! `CLAUDE_CONFIG_DIR` and the proxies aimed at an egress guard. The test
//! fails if anything reached the guard. It is `#[ignore]`d and needs
//! `SPOCKY_REAL_CLAUDE=1` on top, so an unprepared machine cannot pass it by
//! accident. Every wait is bounded and a watchdog aborts the process (after
//! stopping the launched pids) when the whole test overruns.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_provider_claude::client::{ClaudeClient, ClaudeClientOptions};
use spocky_provider_claude::local::LocalBoxFuture;
use spocky_provider_claude::session::ResolveBinary;
use spocky_session::agent_sdk::{AgentClient, AgentError, AgentPromptInput};

const GATE: &str = "SPOCKY_REAL_CLAUDE";
const CLAUDE_PATH: &str = "/usr/local/bin/claude";
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const DEADLINE: Duration = Duration::from_secs(300);
const TURN_WAIT: Duration = Duration::from_secs(90);
const VERSION_WAIT: Duration = Duration::from_secs(30);

/// Every outbound connection except loopback and Unix sockets is denied.
const LOOPBACK_ONLY_PROFILE: &str = "(version 1) (allow default) (deny network-outbound) \
(allow network-outbound (remote ip \"localhost:*\")) (allow network-outbound (remote unix-socket))";

/// Records every connection to the proxy the child is pointed at; anything
/// that reaches it tried to leave loopback.
struct EgressGuard {
    url: String,
    attempts: Arc<Mutex<Vec<String>>>,
}

fn egress_guard() -> &'static EgressGuard {
    static GUARD: OnceLock<EgressGuard> = OnceLock::new();
    GUARD.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind egress guard");
        let port = listener.local_addr().expect("guard address").port();
        assert!(port != 6767 && port != 6768);
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&attempts);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut first_line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut first_line);
                log.lock()
                    .expect("guard log")
                    .push(first_line.trim().to_owned());
                let _ = (&stream).write_all(
                    b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
            }
        });
        EgressGuard {
            url: format!("http://127.0.0.1:{port}"),
            attempts,
        }
    })
}

/// Fails the test if the child tried to reach a non-loopback host.
fn assert_no_egress() {
    let attempts = egress_guard().attempts.lock().expect("guard log").clone();
    assert!(
        attempts.is_empty(),
        "non-loopback egress attempted: {attempts:?}"
    );
}

/// A disposable directory removed on drop.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(label: &str) -> Self {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .expect("temp dir")
            .join(format!("spocky-real-claude-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        for child in ["home", "config", "project"] {
            std::fs::create_dir_all(path.join(child)).expect("root");
        }
        Self { path }
    }

    fn join(&self, child: &str) -> PathBuf {
        self.path.join(child)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Stops the pids the launcher recorded, then aborts the process, once the
/// deadline passes. Dropping it disarms it.
struct Watchdog {
    done: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(pids: PathBuf) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        thread::spawn(move || {
            let end = Instant::now() + DEADLINE;
            while Instant::now() < end {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            for pid in std::fs::read_to_string(&pids)
                .unwrap_or_default()
                .lines()
                .filter(|line| line.chars().all(|c| c.is_ascii_digit()) && !line.is_empty())
            {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-TERM", pid])
                    .status();
            }
            eprintln!("real claude test overran {DEADLINE:?}");
            std::process::abort();
        });
        Self { done }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// The Messages API stub: every `POST /v1/messages` streams one text reply.
struct MessagesStub {
    port: u16,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    stop: Arc<AtomicBool>,
}

impl MessagesStub {
    fn start(reply: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().expect("stub address").port();
        assert!(port != 6767 && port != 6768);
        listener.set_nonblocking(true).expect("nonblocking stub");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            thread::spawn(move || serve(&stream, &requests, reply));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(10)),
                    }
                }
            });
        }
        Self {
            port,
            requests,
            stop,
        }
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for MessagesStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// One request: method and path are recorded, the body is drained.
fn serve(stream: &TcpStream, requests: &Mutex<Vec<(String, String)>>, reply: &str) {
    stream.set_nonblocking(false).ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok();
    requests
        .lock()
        .expect("requests")
        .push((method.clone(), path.clone()));
    let mut out = stream;
    let is_message =
        method == "POST" && path.starts_with("/v1/messages") && !path.contains("count_tokens");
    if !is_message {
        let _ = out.write_all(
            b"HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
        );
        return;
    }
    let event =
        |kind: &str, data: &str| format!("event: {kind}\ndata: {{\"type\":\"{kind}\"{data}}}\n\n");
    let mut stream_body = String::new();
    stream_body.push_str(&event(
        "message_start",
        ",\"message\":{\"id\":\"msg_stub\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}",
    ));
    stream_body.push_str(&event(
        "content_block_start",
        ",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}",
    ));
    stream_body.push_str(&event(
        "content_block_delta",
        &format!(",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{reply}\"}}"),
    ));
    stream_body.push_str(&event("content_block_stop", ",\"index\":0"));
    stream_body.push_str(&event(
        "message_delta",
        ",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}",
    ));
    stream_body.push_str(&event("message_stop", ""));
    let _ = out.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
    );
    let _ = out.write_all(stream_body.as_bytes());
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// The environment of every launch: nothing but these.
fn hermetic_env(root: &Root, stub_port: Option<u16>) -> JsObject {
    let guard = egress_guard().url.clone();
    let mut env = JsObject::new();
    env.insert("HOME", text(&root.join("home").to_string_lossy()));
    env.insert("PATH", text("/usr/bin:/bin"));
    env.insert(
        "CLAUDE_CONFIG_DIR",
        text(&root.join("config").to_string_lossy()),
    );
    env.insert("ANTHROPIC_API_KEY", text("sk-ant-test"));
    if let Some(port) = stub_port {
        env.insert(
            "ANTHROPIC_BASE_URL",
            text(&format!("http://127.0.0.1:{port}")),
        );
    }
    for (key, value) in [
        ("DISABLE_AUTOUPDATER", "1"),
        ("DISABLE_TELEMETRY", "1"),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ("NO_PROXY", "127.0.0.1,localhost"),
    ] {
        env.insert(key, text(value));
    }
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        env.insert(key, text(&guard));
    }
    env
}

/// Writes the launcher: records its pid, then execs the binary under the
/// loopback-only seatbelt.
fn launcher(root: &Root) -> PathBuf {
    let path = root.join("launch.sh");
    let script = format!(
        "#!/bin/sh\necho $$ >> '{}'\nexec {SANDBOX_EXEC} -p '{LOOPBACK_ONLY_PROFILE}' '{CLAUDE_PATH}' \"$@\"\n",
        root.join("launch.pids").display()
    );
    std::fs::write(&path, script).expect("launcher");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// `claude --version` under the same seatbelt and env, bounded.
fn sandboxed_version(root: &Root) -> String {
    let mut command = std::process::Command::new(SANDBOX_EXEC);
    command
        .args(["-p", LOOPBACK_ONLY_PROFILE, CLAUDE_PATH, "--version"])
        .env_clear();
    for (key, value) in hermetic_env(root, None).iter() {
        if let Some(value) = value.as_str() {
            command.env(key, value);
        }
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn claude --version");
    let deadline = Instant::now() + VERSION_WAIT;
    while child.try_wait().expect("wait").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("claude --version exceeded {VERSION_WAIT:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("read version");
    out
}

fn wait_for(events: &Mutex<Vec<String>>, needle: &str, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if events
            .lock()
            .expect("events")
            .iter()
            .any(|event| event.contains(needle))
        {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
#[ignore = "drives the real claude binary; run with --ignored and SPOCKY_REAL_CLAUDE=1"]
fn a_turn_runs_against_the_loopback_stub() {
    assert_eq!(
        std::env::var_os(GATE).as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "{GATE}=1 is required to run the real claude test"
    );
    assert!(Path::new(CLAUDE_PATH).is_file(), "{CLAUDE_PATH} is missing");
    let root = Root::new("turn");
    let _watchdog = Watchdog::arm(root.join("launch.pids"));
    let version = sandboxed_version(&root);
    assert!(
        version.contains("Claude Code"),
        "unexpected claude --version: {version:?}"
    );
    let stub = MessagesStub::start("stub answer");
    let env = Arc::new(hermetic_env(&root, Some(stub.port)));
    let launch = launcher(&root).to_string_lossy().into_owned();
    let binary: Arc<dyn Fn() -> ResolveBinary + Send + Sync> = Arc::new(move || {
        let launch = launch.clone();
        Rc::new(move || {
            let launch = launch.clone();
            let future: LocalBoxFuture<'static, Result<String, AgentError>> =
                Box::pin(async move { Ok(launch) });
            future
        })
    });
    let client = ClaudeClient::new(ClaudeClientOptions {
        resolve_binary: Some(binary),
        process_env: Some(Arc::new(move || (*env).clone())),
        ..ClaudeClientOptions::default()
    });
    let mut config = JsObject::new();
    config.insert("provider", text("claude"));
    config.insert("cwd", text(&root.join("project").to_string_lossy()));
    config.insert("modeId", text("default"));
    config.insert("model", text("claude-opus-4-8"));
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = client
            .create_session(JsValue::Object(config), None, None)
            .await
            .expect("a session");
        let log = Arc::clone(&events);
        let _unsubscribe = session.subscribe(Arc::new(move |event| {
            log.lock().expect("events").push(stringify(&event));
        }));
        session
            .start_turn(AgentPromptInput::Text("say hi".to_owned()), None)
            .await
            .expect("a turn");
        let finished = wait_for(&events, "\"type\":\"turn_completed\"", TURN_WAIT);
        let _ = session.close().await;
        assert!(
            finished,
            "no turn_completed within {TURN_WAIT:?}: {:?}",
            events.lock().expect("events")
        );
    });
    assert_no_egress();
    let seen = events.lock().expect("events").clone();
    let position = |needle: &str| seen.iter().position(|event| event.contains(needle));
    let started = position("\"type\":\"thread_started\"").expect("thread_started");
    let answer = position("\"text\":\"stub answer\"").expect("the stub's reply as a message");
    let completed = position("\"type\":\"turn_completed\"").expect("turn_completed");
    assert!(started < answer && answer < completed, "{seen:?}");
    assert!(
        seen.iter()
            .all(|event| !event.contains("\"type\":\"turn_failed\"")),
        "{seen:?}"
    );
    let requests = stub.requests();
    assert!(
        requests
            .iter()
            .any(|(method, path)| method == "POST" && path.starts_with("/v1/messages")),
        "the stub saw no messages request: {requests:?}"
    );
}
