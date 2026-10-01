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
    CodexProvider, CodexSession, CustomProvider, ProviderRuntimeSettings, SessionConfig,
};

/// Pinned Codex binary and digest from `evidence/phase3/slice-plan.md`.
pub const PINNED_CODEX_VERSION: &str = "codex-cli 0.159.0";

/// One scripted reply to a `POST /v1/responses`.
#[derive(Clone)]
pub enum Reply {
    /// A complete assistant message streamed as the given deltas.
    Message { id: String, deltas: Vec<String> },
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

fn sse(event: &str, mut data: Value) -> String {
    data["type"] = json!(event);
    format!("event: {event}\ndata: {data}\n\n")
}

/// A disposable root with `home/`, `codex/` (`CODEX_HOME`), and `project/`.
/// Dropping it deletes exactly this directory.
pub struct DisposableRoot {
    pub path: PathBuf,
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
        Self { path }
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

/// Provider wired the way the slice harness configures the daemon: an
/// `extends: "codex"` profile whose env points Codex at the stub.
pub fn stub_provider(root: &DisposableRoot, stub: &ResponsesStub) -> CodexProvider {
    let home = path_string(&root.join("home"));
    let base_env: Vec<(OsString, OsString)> = vec![
        (
            OsString::from("PATH"),
            std::env::var_os("PATH").unwrap_or_default(),
        ),
        (OsString::from("HOME"), OsString::from(&home)),
        (OsString::from("USERPROFILE"), OsString::from(&home)),
    ];
    let env = [
        ("CODEX_HOME".to_owned(), path_string(&root.join("codex"))),
        ("OPENAI_API_KEY".to_owned(), "test-key".to_owned()),
        ("OPENAI_BASE_URL".to_owned(), stub.base_url()),
    ]
    .into_iter()
    .collect();
    CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: None,
            env: Some(env),
        }),
        Some(CustomProvider {
            id: "codex-stub".to_owned(),
            label: "Codex Stub".to_owned(),
            extends: "codex".to_owned(),
        }),
        base_env,
    )
}

pub fn full_access_config(root: &DisposableRoot) -> SessionConfig {
    SessionConfig {
        cwd: root.project(),
        mode_id: Some("full-access".to_owned()),
        ..SessionConfig::default()
    }
}

/// The config Paseo's agent manager builds for `run --mode full-access`
/// without `--model`: `normalizeConfig` fills `model` from the provider
/// catalog (`resolveDefaultModelId`) before the session is created.
pub fn manager_full_access_config(
    root: &DisposableRoot,
    provider: &CodexProvider,
) -> SessionConfig {
    let catalog = provider.fetch_catalog().expect("codex catalog");
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
