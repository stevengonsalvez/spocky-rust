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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_provider_codex::{
    CodexProvider, CodexSession, CustomProvider, ProviderCommand, ProviderRuntimeSettings,
    SessionConfig,
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

fn serve(
    stream: TcpStream,
    requests: &Mutex<Vec<RecordedRequest>>,
    stop: &AtomicBool,
    replies: &Mutex<std::vec::IntoIter<Reply>>,
) {
    stream.set_nonblocking(false).ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone stub stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
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
    requests.lock().unwrap().push(RecordedRequest {
        method: method.clone(),
        path: path.clone(),
        body,
    });
    let mut stream = stream;
    if method != "POST" || !path.ends_with("/responses") {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        return;
    }
    let reply = replies.lock().unwrap().next();
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
fn write_hermetic_config(root: &DisposableRoot, stub: &ResponsesStub) {
    let config = format!(
        "model_provider = \"codex-stub\"\ncheck_for_update_on_startup = false\n\n\
[model_providers.codex-stub]\nname = \"Codex Stub\"\nbase_url = \"{}/v1\"\n\
wire_api = \"responses\"\nenv_key = \"OPENAI_API_KEY\"\nrequires_openai_auth = false\n\n\
[analytics]\nenabled = false\n\n\
[features]\napps = false\nplugins = false\nremote_plugin = false\nplugin_sharing = false\ntool_suggest = false\n",
        stub.base_url()
    );
    std::fs::write(root.join("codex").join("config.toml"), config).expect("write config.toml");
}

/// A launcher that runs `codex` under the loopback-only seatbelt profile.
fn loopback_only_launcher(root: &DisposableRoot, codex: &str) -> String {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let launcher = bin.join("codex");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nexec /usr/bin/sandbox-exec -p '{LOOPBACK_ONLY_PROFILE}' '{codex}' \"$@\"\n",
            root.join(APP_SERVER_PIDS).display()
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
    write_hermetic_config(root, stub);
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
