//! Shared support for tests that drive the real pinned `codex` binary: a
//! scripted local `/v1/responses` SSE stub, a disposable root, and a stream
//! event collector. No fake Codex: the app-server is the real binary, only
//! the model endpoint is scripted.

// Each test binary uses a different subset of this module.
#![allow(dead_code)]

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_provider_codex::{
    CodexProvider, CodexSession, CustomProvider, Prompt, ProviderCommand, ProviderRuntimeSettings,
    RunOptions, SessionConfig,
};

/// Pinned Codex binary and digest from `evidence/phase3/slice-plan.md`.
pub const PINNED_CODEX_VERSION: &str = "codex-cli 0.159.0";
pub const PINNED_CODEX_PATH: &str = "/usr/local/Caskroom/codex/0.159.0/bin/codex";
pub const PINNED_CODEX_SHA256: &str =
    "1ad71e5ed117114f9d04cdd8d5dd411515b5ab7ebc725b8ca2f484695d71c838";

/// Explicit opt-in for real-Codex tests, on top of `#[ignore]`: running
/// them with `--include-ignored` but without `SPOCKY_REAL_CODEX=1` fails
/// instead of passing on an unprepared machine.
pub const REAL_CODEX_GATE: &str = "SPOCKY_REAL_CODEX";

/// The pinned Codex binary, verified by path, SHA-256, and version. Panics
/// when it is missing or different, so a real-Codex test cannot pass on
/// another binary.
pub fn real_codex() -> String {
    static VERIFIED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    assert_eq!(
        std::env::var_os(REAL_CODEX_GATE).as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "{REAL_CODEX_GATE}=1 is required to run real-codex tests; they launch the pinned \
         {PINNED_CODEX_VERSION} binary"
    );
    VERIFIED.get_or_init(|| {
        let digest = std::process::Command::new("shasum")
            .args(["-a", "256", PINNED_CODEX_PATH])
            .output()
            .expect("shasum of the pinned codex");
        let digest = String::from_utf8_lossy(&digest.stdout);
        assert_eq!(
            digest.split_whitespace().next(),
            Some(PINNED_CODEX_SHA256),
            "pinned codex digest mismatch at {PINNED_CODEX_PATH}"
        );
        assert_eq!(sandboxed_version().trim(), PINNED_CODEX_VERSION);
    });
    PINNED_CODEX_PATH.to_owned()
}

/// Bound on `codex --version`.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `codex --version` under the loopback-only seatbelt with the same hermetic
/// env as a real session (disposable `HOME` and `CODEX_HOME`, proxies aimed at
/// the egress guard), killed by its own pid when it overruns
/// `VERSION_PROBE_TIMEOUT`, then checked for egress.
fn sandboxed_version() -> String {
    let root = DisposableRoot::new("version-probe");
    let mut child = std::process::Command::new("/usr/bin/sandbox-exec")
        .args(["-p", LOOPBACK_ONLY_PROFILE, PINNED_CODEX_PATH, "--version"])
        .env_clear()
        .envs(hermetic_env(&root))
        .env("CODEX_HOME", root.join("codex"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn pinned codex --version");
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    while child
        .try_wait()
        .expect("wait for codex --version")
        .is_none()
    {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned codex --version exceeded {VERSION_PROBE_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("version stdout")
        .read_to_string(&mut stdout)
        .expect("read codex --version");
    assert_no_egress();
    stdout
}

/// Seatbelt profile for Codex in tests: every outbound connection except
/// loopback and Unix sockets is denied.
const LOOPBACK_ONLY_PROFILE: &str = "(version 1) (allow default) (deny network-outbound) \
(allow network-outbound (remote ip \"localhost:*\")) (allow network-outbound (remote unix-socket))";

/// Records every connection to the HTTP proxy all real-Codex children are
/// pointed at. Anything that reaches it tried to leave loopback.
struct EgressGuard {
    url: String,
    attempts: Arc<Mutex<Vec<String>>>,
}

fn egress_guard() -> &'static EgressGuard {
    static GUARD: std::sync::OnceLock<EgressGuard> = std::sync::OnceLock::new();
    GUARD.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind egress guard");
        let port = listener.local_addr().expect("guard addr").port();
        assert!(port != 6767 && port != 6768);
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&attempts);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut first_line = String::new();
                let mut reader = BufReader::new(&stream);
                let _ = reader.read_line(&mut first_line);
                log.lock().unwrap().push(first_line.trim().to_owned());
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

/// Fails the test if any Codex child tried to reach a non-loopback host.
pub fn assert_no_egress() {
    let attempts = egress_guard().attempts.lock().unwrap().clone();
    assert!(
        attempts.is_empty(),
        "non-loopback egress attempted: {attempts:?}"
    );
}

/// One scripted reply to a `POST /v1/responses`.
#[derive(Clone)]
pub enum Reply {
    /// A complete assistant message streamed as the given deltas.
    Message { id: String, deltas: Vec<String> },
    /// A completed function call the model asks Codex to run.
    FunctionCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    /// A completed freeform (custom) tool call such as `apply_patch`.
    CustomToolCall {
        call_id: String,
        name: String,
        input: String,
    },
    /// `response.created`, then the stream stays open until the client
    /// disconnects or the stub stops.
    Hold,
    /// `HTTP/1.1 500 Internal Server Error` with this JSON body, as the G4
    /// slice stub answers an upstream failure.
    ServerError { body: Value },
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub body: Value,
}

/// Scripted Responses API stub on a random loopback port.
pub struct ResponsesStub {
    pub port: u16,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
}

impl ResponsesStub {
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().expect("stub addr").port();
        assert!(
            port != 6767 && port != 6768,
            "stub must never use 6767 or 6768"
        );
        listener.set_nonblocking(true).expect("nonblocking stub");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            let stop = Arc::clone(&stop);
                            let replies = Arc::clone(&replies);
                            thread::spawn(move || serve(stream, &requests, &stop, &replies));
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

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ResponsesStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if !thread::panicking() {
            assert_no_egress();
        }
    }
}

/// Reads one HTTP request (request line, headers, `content-length` body).
fn read_request(stream: &TcpStream) -> Option<RecordedRequest> {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stub stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut content_length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).ok();
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Some(RecordedRequest { method, path, body })
}

fn serve(
    stream: TcpStream,
    requests: &Mutex<Vec<RecordedRequest>>,
    stop: &AtomicBool,
    replies: &Mutex<std::vec::IntoIter<Reply>>,
) {
    stream.set_nonblocking(false).ok();
    let Some(request) = read_request(&stream) else {
        return;
    };
    let (method, path) = (request.method.clone(), request.path.clone());
    requests.lock().unwrap().push(request);
    let mut stream = stream;
    if method != "POST" || !path.ends_with("/responses") {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        return;
    }
    let reply = replies.lock().unwrap().next();
    if let Some(Reply::ServerError { body }) = &reply {
        write_server_error(&mut stream, body);
        return;
    }
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
    );
    let response_id = "resp_stub";
    let _ = stream
        .write_all(sse("response.created", json!({"response": {"id": response_id}})).as_bytes());
    match reply {
        Some(Reply::Message { id, deltas }) => {
            let text: String = deltas.concat();
            let mut events = sse(
                "response.output_item.added",
                json!({"output_index": 0, "item": {"type": "message", "id": id, "role": "assistant", "status": "in_progress", "content": []}}),
            );
            for delta in &deltas {
                events.push_str(&sse(
                    "response.output_text.delta",
                    json!({"output_index": 0, "item_id": id, "content_index": 0, "delta": delta}),
                ));
            }
            events.push_str(&sse(
                "response.output_item.done",
                json!({"output_index": 0, "item": {"type": "message", "id": id, "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": text, "annotations": []}]}}),
            ));
            events.push_str(&sse(
                "response.completed",
                json!({"response": {"id": response_id, "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 4, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 14}}}),
            ));
            let _ = stream.write_all(events.as_bytes());
        }
        Some(Reply::FunctionCall {
            call_id,
            name,
            arguments,
        }) => {
            let item = json!({
                "type": "function_call", "id": format!("fc_{call_id}"), "call_id": call_id,
                "name": name, "arguments": arguments.to_string(), "status": "completed"
            });
            let _ = stream.write_all(tool_call_events(&item, "arguments", response_id).as_bytes());
        }
        Some(Reply::CustomToolCall {
            call_id,
            name,
            input,
        }) => {
            let item = json!({
                "type": "custom_tool_call", "id": format!("ctc_{call_id}"), "call_id": call_id,
                "name": name, "input": input, "status": "completed"
            });
            let _ = stream.write_all(tool_call_events(&item, "input", response_id).as_bytes());
        }
        Some(Reply::ServerError { .. }) => unreachable!("answered before the SSE head"),
        Some(Reply::Hold) | None => {
            while !stop.load(Ordering::SeqCst) {
                if stream.write_all(b": keepalive\n\n").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// `HTTP/1.1 500` with a JSON body, in place of the SSE stream.
fn write_server_error(stream: &mut TcpStream, body: &Value) {
    let body = body.to_string();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
}

/// One completed tool call item: added (in progress, `payload` empty), done,
/// then `response.completed`.
fn tool_call_events(item: &Value, payload: &str, response_id: &str) -> String {
    let mut added = item.clone();
    added["status"] = json!("in_progress");
    added[payload] = json!("");
    let mut events = sse(
        "response.output_item.added",
        json!({"output_index": 0, "item": added}),
    );
    events.push_str(&sse(
        "response.output_item.done",
        json!({"output_index": 0, "item": item}),
    ));
    events.push_str(&sse(
        "response.completed",
        json!({"response": {"id": response_id, "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 4, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 14}}}),
    ));
    events
}

fn sse(event: &str, mut data: Value) -> String {
    data["type"] = json!(event);
    format!("event: {event}\ndata: {data}\n\n")
}

/// Whole-test bound for real-Codex tests. Each event wait has its own
/// timeout, but session calls carry Paseo's request timeouts (up to 14 days),
/// so a wedged Codex would otherwise hang the run.
pub const REAL_CODEX_TEST_DEADLINE: Duration = Duration::from_secs(300);

/// File in a disposable root where the launcher records each app-server pid
/// (also its process group id) before it execs Codex.
pub const APP_SERVER_PIDS: &str = "app-server.pids";

/// File in a disposable root where the launcher records the argv of every
/// Codex launch (`--version` probes and `app-server` spawns), in order.
pub const CODEX_ARGV_LOG: &str = "codex-argv.log";

/// Aborts the test process if it is still alive at the deadline. A blocked
/// test thread cannot be stopped any other way, and abort skips `Drop`, so
/// first it stops the app-servers recorded under `root` and deletes `root`.
/// Dropping it disarms it.
pub struct Watchdog {
    done: Arc<(Mutex<bool>, Condvar)>,
}

impl Watchdog {
    pub fn arm(label: &str, limit: Duration) -> Self {
        Self::arm_for(label, limit, None)
    }

    pub fn arm_for(label: &str, limit: Duration, root: Option<PathBuf>) -> Self {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let shared = Arc::clone(&done);
        let label = label.to_owned();
        thread::spawn(move || {
            let (flag, changed) = &*shared;
            let (finished, _) = changed
                .wait_timeout_while(flag.lock().unwrap(), limit, |finished| !*finished)
                .unwrap();
            if !*finished {
                eprintln!("real-codex test '{label}' exceeded its {limit:?} deadline; aborting");
                if let Some(root) = root {
                    eprintln!("disposable root: {}", root.display());
                    stop_recorded_app_servers(&root);
                    let _ = std::fs::remove_dir_all(&root);
                }
                std::process::abort();
            }
        });
        Self { done }
    }
}

/// TERM, then after 2 s KILL, to each process group recorded in `root`
/// whose leader is still a child of this test process. The parent check
/// skips a pid that exited and was reused.
fn stop_recorded_app_servers(root: &Path) {
    let recorded = std::fs::read_to_string(root.join(APP_SERVER_PIDS)).unwrap_or_default();
    let ours = |pid: &u32| {
        std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout).trim() == std::process::id().to_string()
            })
    };
    let groups: Vec<u32> = recorded
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .filter(ours)
        .collect();
    for signal in ["TERM", "KILL"] {
        for pid in groups.iter().filter(|pid| ours(pid)) {
            eprintln!("sending SIG{signal} to app-server process group {pid}");
            let _ = std::process::Command::new("/bin/kill")
                .args(["-s", signal, "--", &format!("-{pid}")])
                .status();
        }
        if signal == "TERM" {
            thread::sleep(Duration::from_secs(2));
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let (flag, changed) = &*self.done;
        *flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

/// A disposable root with `home/`, `codex/` (`CODEX_HOME`), and `project/`.
/// Dropping it deletes exactly this directory. Every real-Codex test owns
/// one, so it also arms the test's `REAL_CODEX_TEST_DEADLINE` watchdog.
pub struct DisposableRoot {
    pub path: PathBuf,
    _watchdog: Watchdog,
}

impl DisposableRoot {
    pub fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-p3-codex-{label}-{}-{nanos}",
            std::process::id()
        ));
        for child in ["home", "codex", "project"] {
            std::fs::create_dir_all(path.join(child)).expect("create disposable root");
        }
        let path = path.canonicalize().expect("canonical disposable root");
        Self {
            _watchdog: Watchdog::arm_for(label, REAL_CODEX_TEST_DEADLINE, Some(path.clone())),
            path,
        }
    }

    pub fn join(&self, child: &str) -> PathBuf {
        self.path.join(child)
    }

    pub fn project(&self) -> String {
        path_string(&self.join("project"))
    }
}

impl Drop for DisposableRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `CODEX_HOME/config.toml` for hermetic runs: the stub is the default model
/// provider (so `model/list`, `config/read`, and background calls never use
/// the built-in `openai` provider), and analytics, update checks, apps, and
/// plugins, which otherwise reach chatgpt.com and github.com, are off.
fn write_hermetic_config(root: &DisposableRoot, stub: &ResponsesStub, fail_fast: bool) {
    // Codex retries a 5xx or a dropped stream up to five times with backoff,
    // which takes minutes. A failure test turns that off so the first
    // upstream failure ends the turn.
    let retries = if fail_fast {
        "request_max_retries = 0\nstream_max_retries = 0\n"
    } else {
        ""
    };
    let config = format!(
        "model_provider = \"codex-stub\"\ncheck_for_update_on_startup = false\n\n\
[model_providers.codex-stub]\nname = \"Codex Stub\"\nbase_url = \"{}/v1\"\n\
wire_api = \"responses\"\nenv_key = \"OPENAI_API_KEY\"\nrequires_openai_auth = false\n{retries}\n\
[analytics]\nenabled = false\n\n\
[features]\napps = false\nplugins = false\nremote_plugin = false\nplugin_sharing = false\ntool_suggest = false\n",
        stub.base_url()
    );
    std::fs::write(root.join("codex").join("config.toml"), config).expect("write config.toml");
}

/// Directory the launcher records `app-server` stdio under when set: each
/// launch writes `<dir>/<root dir name>/<pid>/{in,out}.jsonl`, the bytes the
/// client sent and the bytes Codex answered. `record_fixture.py` turns those
/// recordings into the replay fixtures.
pub const RECORD_DIR_ENV: &str = "SPOCKY_RECORD_DIR";

/// The launcher lines that tee an `app-server` launch's stdio into
/// `RECORD_DIR_ENV`, or nothing when it is unset. Only the launcher script
/// changes: Codex still runs the same command under the same seatbelt.
fn recording_lines(root: &DisposableRoot, codex: &str) -> String {
    let Some(dir) = std::env::var_os(RECORD_DIR_ENV) else {
        return String::new();
    };
    let name = root.path.file_name().expect("root dir name");
    let dir = PathBuf::from(dir).join(name);
    format!(
        "if [ \"$1\" = app-server ]; then d='{}'/$$; mkdir -p \"$d\"; tee \"$d/in.jsonl\" | /usr/bin/sandbox-exec -p '{LOOPBACK_ONLY_PROFILE}' '{codex}' \"$@\" | tee \"$d/out.jsonl\"; exit; fi\n",
        dir.display()
    )
}

/// A launcher that runs `codex` under the loopback-only seatbelt profile.
fn loopback_only_launcher(root: &DisposableRoot, codex: &str) -> String {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let launcher = bin.join("codex");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\necho \"$*\" >> '{}'\n{}exec /usr/bin/sandbox-exec -p '{LOOPBACK_ONLY_PROFILE}' '{codex}' \"$@\"\n",
            root.join(APP_SERVER_PIDS).display(),
            root.join(CODEX_ARGV_LOG).display(),
            recording_lines(root, codex)
        ),
    )
    .expect("write launcher");
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&launcher)
        .status()
        .expect("chmod launcher");
    path_string(&launcher)
}

/// Provider wired the way the slice harness configures the daemon: an
/// `extends: "codex"` profile whose env points Codex at the stub, launching
/// the pinned binary by absolute path. Codex runs hermetically: config file
/// above, loopback-only seatbelt, and every proxy variable aimed at the
/// egress guard.
pub fn stub_provider(root: &DisposableRoot, stub: &ResponsesStub, codex: &str) -> CodexProvider {
    hermetic_provider(root, stub, codex, false)
}

/// [`stub_provider`] with Codex's upstream retries turned off, so a scripted
/// failure (a 500) ends the turn at once.
pub fn failing_stub_provider(
    root: &DisposableRoot,
    stub: &ResponsesStub,
    codex: &str,
) -> CodexProvider {
    hermetic_provider(root, stub, codex, true)
}

fn hermetic_provider(
    root: &DisposableRoot,
    stub: &ResponsesStub,
    codex: &str,
    fail_fast: bool,
) -> CodexProvider {
    write_hermetic_config(root, stub, fail_fast);
    let launcher = loopback_only_launcher(root, codex);
    let env = [
        ("CODEX_HOME".to_owned(), path_string(&root.join("codex"))),
        ("OPENAI_API_KEY".to_owned(), "test-key".to_owned()),
        ("OPENAI_BASE_URL".to_owned(), stub.base_url()),
    ]
    .into_iter()
    .collect();
    CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: vec![launcher],
            }),
            env: Some(env),
        }),
        Some(CustomProvider {
            id: "codex-stub".to_owned(),
            label: "Codex Stub".to_owned(),
            extends: "codex".to_owned(),
        }),
        hermetic_env(root),
    )
}

/// Pinned Node from `evidence/phase3/gate-g1.md`.
const PINNED_NODE_SHA256: &str = "1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931";

/// Bound on the pinned client's G1 sequence.
const PINNED_SEQUENCE_TIMEOUT: Duration = Duration::from_secs(120);

/// The pinned Node binary (`SPOCKY_PINNED_NODE`, digest-checked) and the
/// pinned `codex-app-server-agent.js` under `SPOCKY_PASEO_DIST`.
pub struct PinnedPaseo {
    pub node: PathBuf,
    pub module: PathBuf,
}

/// [`PinnedPaseo`] from the environment. Panics when either variable is
/// unset or the node digest differs, so a pinned differential cannot pass
/// without the pinned build.
pub fn pinned_paseo() -> PinnedPaseo {
    let var = |name: &str| {
        std::env::var_os(name)
            .unwrap_or_else(|| panic!("{name} is required for the pinned differential"))
    };
    let node = PathBuf::from(var("SPOCKY_PINNED_NODE"));
    let digest = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(&node)
        .output()
        .expect("shasum of the pinned node");
    assert_eq!(
        String::from_utf8_lossy(&digest.stdout)
            .split_whitespace()
            .next(),
        Some(PINNED_NODE_SHA256),
        "pinned node digest mismatch"
    );
    let dist = PathBuf::from(var("SPOCKY_PASEO_DIST"));
    let module = ["server/server", "server"]
        .iter()
        .map(|dir| {
            dist.join(dir)
                .join("agent/providers/codex-app-server-agent.js")
        })
        .find(|module| module.is_file())
        .expect("pinned codex-app-server-agent.js under SPOCKY_PASEO_DIST");
    PinnedPaseo { node, module }
}

/// Runs `tests/support/pinned_g1_probes.mjs`: the pinned Paseo
/// `CodexAppServerAgentClient` (from `SPOCKY_PASEO_DIST`, run by the
/// `SPOCKY_PINNED_NODE` binary) through the G1 launch sequence with the same
/// launcher, env, and stub as [`stub_provider`], so every Codex it starts
/// runs under the loopback-only seatbelt. Node itself is not wrapped: a
/// seatbelt cannot apply another one, so the launcher's would fail. It gets
/// the hermetic env with proxies aimed at the egress guard, is bounded by
/// `PINNED_SEQUENCE_TIMEOUT` and killed by its own pid on overrun, and the
/// guard is checked afterwards. Phase markers and launches land in the
/// root's [`CODEX_ARGV_LOG`].
pub fn run_pinned_g1_sequence(root: &DisposableRoot, stub: &ResponsesStub, model: &str) {
    let PinnedPaseo { node, module } = pinned_paseo();
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/pinned_g1_probes.mjs");
    let mut child = std::process::Command::new(&node)
        .arg(&driver)
        .arg(&module)
        .arg(root.join("bin/codex"))
        .arg(root.join(CODEX_ARGV_LOG))
        .arg(root.project())
        .arg(model)
        .env_clear()
        .envs(hermetic_env(root))
        .env("CODEX_HOME", root.join("codex"))
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", stub.base_url())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn pinned node");
    let deadline = Instant::now() + PINNED_SEQUENCE_TIMEOUT;
    while child.try_wait().expect("wait for pinned node").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned G1 sequence exceeded {PINNED_SEQUENCE_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("pinned node output");
    assert!(
        output.status.success(),
        "pinned G1 sequence failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_no_egress();
}

/// `PATH`, a disposable `HOME`, and every proxy variable aimed at the egress
/// guard, with loopback exempt.
fn hermetic_env(root: &DisposableRoot) -> Vec<(OsString, OsString)> {
    let home = path_string(&root.join("home"));
    let guard = egress_guard().url.clone();
    let mut base_env: Vec<(OsString, OsString)> = vec![
        (
            OsString::from("PATH"),
            std::env::var_os("PATH").unwrap_or_default(),
        ),
        (OsString::from("HOME"), OsString::from(&home)),
        (OsString::from("USERPROFILE"), OsString::from(&home)),
        (
            OsString::from("NO_PROXY"),
            OsString::from("127.0.0.1,localhost"),
        ),
        (
            OsString::from("no_proxy"),
            OsString::from("127.0.0.1,localhost"),
        ),
    ];
    for key in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ] {
        base_env.push((OsString::from(key), OsString::from(&guard)));
    }
    base_env
}

/// `run --mode auto`: on-request approvals in a workspace-write sandbox.
pub fn manager_auto_config(root: &DisposableRoot, provider: &CodexProvider) -> SessionConfig {
    SessionConfig {
        mode_id: Some("auto".to_owned()),
        ..manager_full_access_config(root, provider)
    }
}

pub fn full_access_config(root: &DisposableRoot) -> SessionConfig {
    SessionConfig {
        cwd: root.project(),
        mode_id: Some("full-access".to_owned()),
        ..SessionConfig::default()
    }
}

/// Bound on the catalog fetch that resolves the manager's default model.
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(90);

/// The config Paseo's agent manager builds for `run --mode full-access`
/// without `--model`: `normalizeConfig` fills `model` from the provider
/// catalog (`resolveDefaultModelId`) before the session is created.
pub fn manager_full_access_config(
    root: &DisposableRoot,
    provider: &CodexProvider,
) -> SessionConfig {
    let catalog = provider
        .fetch_catalog(Some(Instant::now() + CATALOG_TIMEOUT))
        .expect("codex catalog");
    let model = spocky_provider_codex::catalog::default_model_id(&catalog).expect("default model");
    SessionConfig {
        model: Some(model),
        ..full_access_config(root)
    }
}

/// Collects stream events and lets a test wait for one.
#[derive(Clone, Default)]
pub struct Events {
    inner: Arc<(Mutex<Vec<Value>>, Condvar)>,
}

impl Events {
    pub fn attach(session: &CodexSession) -> Self {
        let events = Self::default();
        let sink = events.clone();
        session.subscribe(Arc::new(move |event: &Value| {
            let (list, ready) = &*sink.inner;
            list.lock().unwrap().push(event.clone());
            ready.notify_all();
        }));
        events
    }

    pub fn snapshot(&self) -> Vec<Value> {
        self.inner.0.lock().unwrap().clone()
    }

    /// Waits up to `timeout` until `count` events of the given type arrived.
    pub fn wait_for_count(&self, event_type: &str, count: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let (list, ready) = &*self.inner;
        let mut events = list.lock().unwrap();
        loop {
            if events
                .iter()
                .filter(|event| event["type"] == event_type)
                .count()
                >= count
            {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {count} {event_type}; got {:#?}",
                *events
            );
            events = ready.wait_timeout(events, deadline - now).unwrap().0;
        }
    }

    /// Waits up to `timeout` for an event of the given type.
    pub fn wait_for(&self, event_type: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        let (list, ready) = &*self.inner;
        let mut events = list.lock().unwrap();
        loop {
            if let Some(found) = events.iter().find(|event| event["type"] == event_type) {
                return found.clone();
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {event_type}; got {:#?}",
                *events
            );
            events = ready.wait_timeout(events, deadline - now).unwrap().0;
        }
    }
}

/// True while a process with this pid exists.
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The command both clients launch as `codex`: the replay of `scenario` from
/// `tests/fixtures/<fixture>`, logging what the client sends to `log`.
fn replay_argv(
    pinned: &PinnedPaseo,
    fixture: &str,
    scenario: &str,
    root: &DisposableRoot,
    log: &Path,
) -> Vec<String> {
    [
        pinned.node.clone(),
        crate_path("tests/support/codex_replay.mjs"),
        crate_path(&format!("tests/fixtures/{fixture}")),
        PathBuf::from(scenario),
        root.path.clone(),
        log.to_path_buf(),
    ]
    .iter()
    .map(|part| part.to_string_lossy().into_owned())
    .collect()
}

fn wait_for_any(events: &Events, types: &[&str]) -> Value {
    let deadline = Instant::now() + REPLAY_WAIT;
    loop {
        if let Some(found) = events
            .snapshot()
            .into_iter()
            .find(|event| types.iter().any(|kind| event["type"] == *kind))
        {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {types:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn rust_run(argv: Vec<String>, root: &DisposableRoot, prompt: &str, action: &str) -> String {
    let base_env = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("HOME".into(), root.join("home").into_os_string()),
    ];
    let provider = CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace { argv }),
            env: None,
        }),
        None,
        base_env,
    );
    let session = provider
        .create_session(
            SessionConfig {
                cwd: root.project(),
                mode_id: Some("auto".to_owned()),
                model: Some(REPLAY_MODEL.to_owned()),
                ..SessionConfig::default()
            },
            None,
            false,
        )
        .expect("create session");
    let events = Events::attach(&session);
    session.runtime_info().expect("runtime info");
    session
        .start_turn(&Prompt::Text(prompt.to_owned()), &RunOptions::default())
        .expect("start turn");
    if action == "none" {
        // No approval: the turn runs to its terminal event on its own.
        wait_for_any(&events, &["turn_completed", "turn_canceled", "turn_failed"]);
        session.close().expect("close");
        return json!({"events": events.snapshot(), "pendingBefore": [], "pendingAfter": []})
            .to_string();
    }
    let requested = wait_for_any(&events, &["permission_requested"]);
    let before = session.pending_permissions();
    let id = requested["request"]["id"].as_str().expect("request id");
    match action {
        "allow" => session.respond_to_permission(id, &json!({"behavior": "allow"})),
        "deny" => {
            session.respond_to_permission(id, &json!({"behavior": "deny", "message": "Not now"}))
        }
        "deny_interrupt" => session.respond_to_permission(
            id,
            &json!({"behavior": "deny", "message": "Stop", "interrupt": true}),
        ),
        "interrupt" => session.interrupt(),
        other => panic!("unknown action {other}"),
    }
    .expect("action");
    wait_for_any(&events, &["turn_completed", "turn_canceled", "turn_failed"]);
    let after = session.pending_permissions();
    session.close().expect("close");
    json!({"events": events.snapshot(), "pendingBefore": before, "pendingAfter": after}).to_string()
}

fn pinned_run(
    pinned: &PinnedPaseo,
    argv: &[String],
    root: &DisposableRoot,
    prompt: &str,
    action: &str,
) -> String {
    let mut child = Command::new(&pinned.node)
        .arg(crate_path("tests/support/pinned_g2_session.mjs"))
        .arg(&pinned.module)
        .arg(serde_json::to_string(argv).unwrap())
        .arg(root.project())
        .arg(REPLAY_MODEL)
        .arg(prompt)
        .arg(action)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pinned node");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("wait for pinned node").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned G2 run exceeded 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("pinned output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "pinned G2 run failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout.trim().to_owned()
}

/// `Ok` when `sums` (the text of the digest file `sums_name`) lists `name`
/// with the digest `actual`.
pub fn check_digest(sums_name: &str, sums: &str, name: &str, actual: &str) -> Result<(), String> {
    let listed = sums
        .lines()
        .find_map(|line| {
            let (digest, listed_name) = line.split_once("  ")?;
            (listed_name == name).then_some(digest)
        })
        .ok_or_else(|| format!("{name} is not listed in tests/fixtures/{sums_name}"))?;
    if listed == actual {
        Ok(())
    } else {
        Err(format!("{name} differs from its {sums_name} digest"))
    }
}

/// [`check_digest`] against `SHA256SUMS`, the replay fixtures' digests.
pub fn check_fixture_digest(sums: &str, fixture: &str, actual: &str) -> Result<(), String> {
    check_digest("SHA256SUMS", sums, fixture, actual)
}

/// SHA-256 of `path`, from `shasum -a 256`.
fn sha256_of(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("shasum");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("shasum digest")
        .to_owned()
}

/// Panics unless `tests/fixtures/<fixture>` has the digest `SHA256SUMS`
/// lists, so a replay cannot run on a fixture that changed unnoticed.
pub fn assert_fixture_digest(fixture: &str) {
    let sums = std::fs::read_to_string(crate_path("tests/fixtures/SHA256SUMS"))
        .expect("tests/fixtures/SHA256SUMS");
    let actual = sha256_of(&crate_path(&format!("tests/fixtures/{fixture}")));
    if let Err(problem) = check_fixture_digest(&sums, fixture, &actual) {
        panic!("{problem}");
    }
}

impl PinnedPaseo {
    /// The pinned `agent/providers` directory.
    pub fn providers_dir(&self) -> &Path {
        self.module.parent().expect("agent providers dir")
    }

    /// `relative` (under [`Self::providers_dir`]) after checking its SHA-256
    /// against `tests/fixtures/ORACLE_SHA256SUMS`. The corpus differentials
    /// copy these files and run the copies as their oracle, so a changed or
    /// unlisted file must stop the test before anything is copied.
    pub fn verified_oracle(&self, relative: &str) -> PathBuf {
        let sums = std::fs::read_to_string(crate_path("tests/fixtures/ORACLE_SHA256SUMS"))
            .expect("tests/fixtures/ORACLE_SHA256SUMS");
        let path = self.providers_dir().join(relative);
        if let Err(problem) = check_digest("ORACLE_SHA256SUMS", &sums, relative, &sha256_of(&path))
        {
            panic!("{problem}");
        }
        path
    }
}

/// Whole-run bound for waiting on a terminal session event in a replay.
const REPLAY_WAIT: Duration = Duration::from_secs(30);
/// Model every recorded session was started with.
const REPLAY_MODEL: &str = "gpt-6-astra";

/// Replays one recorded `codex app-server` session (`scenario` of
/// `tests/fixtures/<fixture>`) to the Rust provider and to the pinned
/// `CodexAppServerAgentClient`, runs one turn on each (`action` is an
/// approval action, or `none` to let the turn end by itself), and requires
/// identical session events, pending permissions before and after, and
/// identical client-to-Codex JSON lines. Nothing is normalized: both sides
/// read the same replayed bytes.
pub fn replay_differential(fixture: &str, scenario: &str, prompt: &str, action: &str) {
    assert_fixture_digest(fixture);
    let pinned = pinned_paseo();
    let root = DisposableRoot::new(&format!(
        "{}-{scenario}",
        fixture.trim_end_matches(".json").replace('_', "-")
    ));
    let rust_log = root.join("rust-client.jsonl");
    let pinned_log = root.join("pinned-client.jsonl");
    let rust = rust_run(
        replay_argv(&pinned, fixture, scenario, &root, &rust_log),
        &root,
        prompt,
        action,
    );
    let argv = replay_argv(&pinned, fixture, scenario, &root, &pinned_log);
    let paseo = pinned_run(&pinned, &argv, &root, prompt, action);
    // The session output is compared as text, with no parse and re-serialize
    // in between: both sides emit `{"events":..,"pendingBefore":..,"pendingAfter":..}`
    // with keys in emission order, so any difference in key order, number
    // text, or string escaping between the Rust provider's output and the
    // pinned client's own `JSON.stringify` shows up here.
    if rust != paseo {
        let part = |text: &str, name: &str| {
            serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|value| value.get(name).map(Value::to_string))
        };
        let differing = ["events", "pendingBefore", "pendingAfter"]
            .into_iter()
            .find(|name| part(&rust, name) != part(&paseo, name))
            .unwrap_or("serialization");
        panic!("{scenario}: {differing} differ\n rust:   {rust}\n pinned: {paseo}");
    }
    let read = |log: &Path| std::fs::read_to_string(log).expect("client log");
    let (ours, theirs) = (read(&rust_log), read(&pinned_log));
    for (index, (rust_line, pinned_line)) in ours.lines().zip(theirs.lines()).enumerate() {
        assert_eq!(
            rust_line, pinned_line,
            "{scenario}: client line {index} differs"
        );
    }
    assert_eq!(
        ours.lines().count(),
        theirs.lines().count(),
        "{scenario}: client line count"
    );
}
