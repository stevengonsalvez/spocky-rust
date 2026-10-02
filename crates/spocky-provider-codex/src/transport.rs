//! Newline-delimited JSON-RPC client for a spawned `codex app-server` child.
//!
//! Port of pinned Paseo `codex/app-server-transport.ts` (`CodexAppServerClient`).
//! Shutdown signals the child's process group instead of Paseo's
//! `utils/tree-kill.ts` descendant walk (see `signal_process_group`). Requests carry
//! increasing numeric ids starting at 1, responses resolve the matching pending
//! request, server-initiated requests go to registered handlers (unhandled
//! methods answer `{}`), and notifications go to one notification handler in
//! stdout order. An unexpected exit rejects every pending request with
//! `Codex app-server exited ...` plus the last 8192 stderr characters.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use spocky_contracts::js::truthy;
use spocky_contracts::js_value::{self, JsValue};
use spocky_contracts::text::js_trim;

/// Paseo `DEFAULT_TIMEOUT_MS`: 14 days.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(14 * 24 * 60 * 60 * 1000);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(2_000);
const FORCE_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1_000);
const STDERR_BUFFER_LIMIT: usize = 8192;
// ponytail: bounded wait for stderr EOF after exit; Node reads whatever stderr
// arrived before its 'exit' event, a grandchild holding the pipe never blocks us.
const STDERR_DRAIN_AFTER_EXIT: Duration = Duration::from_millis(250);

pub const CLIENT_CLOSED_MESSAGE: &str = "Codex app-server client is closed";

/// Rejection of a request: either a JSON-RPC error object from Codex
/// (`CodexAppServerRpcError`) or a transport failure with a plain message.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientError {
    pub message: String,
    /// Present only for JSON-RPC error responses. `code` and `data` keep the
    /// missing versus present distinction of the raw error object.
    pub rpc: Option<Box<RpcErrorFields>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcErrorFields {
    pub code: Option<Value>,
    pub data: Option<Value>,
}

impl ClientError {
    #[must_use]
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            rpc: None,
        }
    }

    /// Paseo compares `error.code === <number>` on `CodexAppServerRpcError`.
    #[must_use]
    pub fn rpc_code_is(&self, code: i64) -> bool {
        self.rpc
            .as_ref()
            .and_then(|rpc| rpc.code.as_ref())
            .is_some_and(|value| {
                value.as_i64() == Some(code)
                    || value.as_f64().is_some_and(|float| {
                        float.fract() == 0.0 && format!("{float:.0}") == code.to_string()
                    })
            })
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

/// Completes one server-initiated request. A responder dropped without an
/// answer replies with [`DROPPED_REQUEST_MESSAGE`] so Codex never waits on a
/// request no one can answer any more.
pub struct Responder {
    shared: Arc<Shared>,
    id: Option<Value>,
}

/// Error reply for a server request whose responder was dropped unanswered.
pub const DROPPED_REQUEST_MESSAGE: &str = "Codex app-server request was not answered";

impl Responder {
    /// `Ok(None)` mirrors a handler resolving `undefined`: the response carries
    /// only the id because `JSON.stringify` drops the `result` key.
    pub fn respond(mut self, outcome: Result<Option<Value>, String>) {
        self.reply(outcome);
    }

    fn reply(&mut self, outcome: Result<Option<Value>, String>) {
        let Some(id) = self.id.take() else {
            return;
        };
        let mut response = Map::new();
        response.insert("id".to_owned(), id);
        match outcome {
            Ok(Some(result)) => {
                response.insert("result".to_owned(), result);
            }
            Ok(None) => {}
            Err(message) => {
                let mut error = Map::new();
                error.insert("message".to_owned(), Value::String(message));
                response.insert("error".to_owned(), Value::Object(error));
            }
        }
        self.shared.write_response(&Value::Object(response));
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.reply(Err(DROPPED_REQUEST_MESSAGE.to_owned()));
    }
}

pub type RequestHandler = Arc<dyn Fn(Option<Value>, Value, Responder) + Send + Sync>;
pub type NotificationHandler = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;
pub type TerminationHandler = Box<dyn FnOnce(ClientError) + Send>;

type PendingSender = mpsc::Sender<Result<Value, ClientError>>;

#[derive(Default)]
struct ExitState {
    exited: bool,
}

struct Shared {
    /// Lines queued for the stdin writer thread; `None` once stdin is closed.
    writer: Mutex<Option<mpsc::Sender<String>>>,
    pending: Mutex<HashMap<u64, PendingSender>>,
    request_handlers: Mutex<HashMap<String, RequestHandler>>,
    notification_handler: Mutex<Option<NotificationHandler>>,
    termination_handler: Mutex<Option<TerminationHandler>>,
    next_id: AtomicU64,
    disposed: AtomicBool,
    reading: AtomicBool,
    stderr: Mutex<String>,
    exit: Mutex<ExitState>,
    exit_changed: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Paseo `CodexAppServerClient` over a child spawned with piped stdio.
#[derive(Clone)]
pub struct AppServerClient {
    shared: Arc<Shared>,
    pid: u32,
}

impl AppServerClient {
    /// Takes ownership of the child's pipes and starts the stdout, stderr, and
    /// exit watchers.
    ///
    /// # Errors
    /// Returns an error when the child does not expose all three stdio pipes,
    /// with Paseo's `Child process did not expose stdio pipes` message.
    pub fn new(mut child: Child) -> Result<Self, ClientError> {
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(ClientError::plain(
                "Child process did not expose stdio pipes",
            ));
        };
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(spawn_stdin_writer(stdin))),
            pending: Mutex::new(HashMap::new()),
            request_handlers: Mutex::new(HashMap::new()),
            notification_handler: Mutex::new(None),
            termination_handler: Mutex::new(None),
            next_id: AtomicU64::new(1),
            disposed: AtomicBool::new(false),
            reading: AtomicBool::new(true),
            stderr: Mutex::new(String::new()),
            exit: Mutex::new(ExitState::default()),
            exit_changed: Condvar::new(),
        });
        let pid = child.id();
        spawn_stdout_reader(Arc::clone(&shared), stdout);
        let stderr_done = spawn_stderr_reader(Arc::clone(&shared), stderr);
        spawn_exit_watcher(Arc::clone(&shared), child, stderr_done);
        Ok(Self { shared, pid })
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn set_termination_handler(&self, handler: TerminationHandler) {
        *lock(&self.shared.termination_handler) = Some(handler);
    }

    pub fn set_notification_handler(&self, handler: NotificationHandler) {
        *lock(&self.shared.notification_handler) = Some(handler);
    }

    pub fn set_request_handler(&self, method: &str, handler: RequestHandler) {
        lock(&self.shared.request_handlers).insert(method.to_owned(), handler);
    }

    /// Sends `{id, method, params}` and blocks until the matching response,
    /// an exit, a dispose, or the timeout.
    ///
    /// # Errors
    /// Rejects with the JSON-RPC error, `Codex app-server client is closed`,
    /// the exit error, or `Codex app-server request timed out for <method>`.
    pub fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        let (sender, receiver) = mpsc::channel();
        let id = {
            // Check and register under the pending lock so a concurrent
            // dispose either rejects this request or is seen here.
            let mut pending = lock(&self.shared.pending);
            if self.shared.disposed.load(Ordering::SeqCst) {
                return Err(ClientError::plain(CLIENT_CLOSED_MESSAGE));
            }
            let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
            pending.insert(id, sender);
            id
        };
        let mut payload = Map::new();
        payload.insert("id".to_owned(), Value::from(id));
        payload.insert("method".to_owned(), Value::String(method.to_owned()));
        if let Some(params) = params {
            payload.insert("params".to_owned(), params);
        }
        self.shared.write_line(&Value::Object(payload));
        if let Ok(outcome) = receiver.recv_timeout(timeout) {
            outcome
        } else {
            lock(&self.shared.pending).remove(&id);
            Err(ClientError::plain(format!(
                "Codex app-server request timed out for {method}"
            )))
        }
    }

    /// Sends `{method, params}` without an id. A disposed client drops it.
    pub fn notify(&self, method: &str, params: Option<Value>) {
        if self.shared.disposed.load(Ordering::SeqCst) {
            return;
        }
        let mut payload = Map::new();
        payload.insert("method".to_owned(), Value::String(method.to_owned()));
        if let Some(params) = params {
            payload.insert("params".to_owned(), params);
        }
        self.shared.write_line(&Value::Object(payload));
    }

    /// Closes the client: rejects pending requests, closes stdin, then sends
    /// SIGTERM to the child's process group, waits 2 s, sends SIGKILL, waits
    /// 1 s.
    ///
    /// # Errors
    /// Returns `Codex app-server did not report exit after SIGKILL` when the
    /// child is still running after the forced signal.
    pub fn dispose(&self) -> Result<(), ClientError> {
        self.shared.disposed.store(true, Ordering::SeqCst);
        lock(&self.shared.termination_handler).take();
        self.shared.reading.store(false, Ordering::SeqCst);
        self.shared
            .reject_pending(&ClientError::plain(CLIENT_CLOSED_MESSAGE));
        // Dropping the sender lets the writer flush queued lines, then close
        // stdin, as Node's `stdin.end()` does.
        lock(&self.shared.writer).take();
        if self.shared.has_exited() {
            return Ok(());
        }
        signal_process_group(self.pid, "TERM");
        if self.shared.wait_for_exit(GRACEFUL_SHUTDOWN_TIMEOUT) {
            return Ok(());
        }
        signal_process_group(self.pid, "KILL");
        if self.shared.wait_for_exit(FORCE_SHUTDOWN_TIMEOUT) {
            return Ok(());
        }
        Err(ClientError::plain(
            "Codex app-server did not report exit after SIGKILL",
        ))
    }

    /// True once the child process has exited.
    #[must_use]
    pub fn has_exited(&self) -> bool {
        self.shared.has_exited()
    }
}

impl Shared {
    fn write_line(&self, value: &Value) {
        let Ok(mut line) = serde_json::to_string(value) else {
            return;
        };
        line.push('\n');
        if let Some(writer) = lock(&self.writer).as_ref() {
            let _ = writer.send(line);
        }
    }

    fn write_response(&self, response: &Value) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        self.write_line(response);
    }

    fn reject_pending(&self, error: &ClientError) {
        let pending: Vec<PendingSender> = lock(&self.pending).drain().map(|(_, s)| s).collect();
        for sender in pending {
            let _ = sender.send(Err(error.clone()));
        }
    }

    fn has_exited(&self) -> bool {
        lock(&self.exit).exited
    }

    fn wait_for_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.exit);
        while !state.exited {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .exit_changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    fn handle_unexpected_termination(&self, error: &ClientError) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.reading.store(false, Ordering::SeqCst);
        self.reject_pending(error);
        let handler = lock(&self.termination_handler).take();
        if let Some(handler) = handler {
            // Paseo logs and ignores a throwing termination handler.
            let error = error.clone();
            let _ = catch_unwind(AssertUnwindSafe(move || handler(error)));
        }
    }

    fn handle_line(self: &Arc<Self>, line: &str) {
        if js_trim(line).is_empty() {
            return;
        }
        let Ok(raw) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let Value::Object(raw) = raw else {
            return;
        };
        let id = raw.get("id");
        if let Some(id @ Value::Number(_)) = id {
            let has_result = raw.contains_key("result");
            let error = raw.get("error").filter(|error| js_truthy(error));
            if has_result || error.is_some() {
                let Some(key) = pending_key(id) else {
                    return;
                };
                let Some(sender) = lock(&self.pending).remove(&key) else {
                    return;
                };
                let outcome = match error {
                    Some(error) => Err(rpc_error(error)),
                    None => Ok(raw.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(outcome);
                return;
            }
            if let Some(Value::String(method)) = raw.get("method") {
                let handler = lock(&self.request_handlers).get(method).cloned();
                let responder = Responder {
                    shared: Arc::clone(self),
                    id: Some(id.clone()),
                };
                match handler {
                    Some(handler) => {
                        let params = raw.get("params").cloned();
                        let id = id.clone();
                        // A panicking handler must not stop the reader.
                        let _ = catch_unwind(AssertUnwindSafe(|| handler(params, id, responder)));
                    }
                    None => responder.respond(Ok(Some(Value::Object(Map::new())))),
                }
                return;
            }
        }
        if id.is_none()
            && let Some(Value::String(method)) = raw.get("method")
        {
            let handler = lock(&self.notification_handler).clone();
            if let Some(handler) = handler {
                // Paseo catches and logs a failing line handler and keeps reading.
                let params = raw.get("params").cloned();
                let _ = catch_unwind(AssertUnwindSafe(|| handler(method, params)));
            }
        }
    }
}

fn pending_key(id: &Value) -> Option<u64> {
    if let Some(key) = id.as_u64() {
        return Some(key);
    }
    let float = id.as_f64()?;
    if float.fract() != 0.0 || !(0.0..=9_007_199_254_740_991.0).contains(&float) {
        return None;
    }
    format!("{float:.0}").parse().ok()
}

fn rpc_error(error: &Value) -> ClientError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Unknown error")
        .to_owned();
    ClientError {
        message,
        rpc: Some(Box::new(RpcErrorFields {
            code: error.get("code").cloned(),
            data: error.get("data").cloned(),
        })),
    }
}

/// A parsed JSON value as the contracts crate's JavaScript value, which its
/// `js` operators work on. Keys keep the value's order.
pub(crate) fn to_js_value(value: &Value) -> JsValue {
    js_value::parse(&value.to_string()).expect("serde_json writes JSON")
}

/// JavaScript truthiness for a parsed JSON value (`spocky_contracts::js`).
pub(crate) fn js_truthy(value: &Value) -> bool {
    truthy(Some(&to_js_value(value)))
}

/// Owns the child's stdin: writes queued lines in order, ignoring write
/// errors (Node reports them asynchronously; the exit handler settles
/// requests), and closes stdin when the queue is dropped.
fn spawn_stdin_writer(mut stdin: ChildStdin) -> mpsc::Sender<String> {
    let (sender, receiver) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in receiver {
            let _ = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush());
        }
    });
    sender
}

fn spawn_stdout_reader(shared: Arc<Shared>, stdout: ChildStdout) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let Ok(read) = reader.read_until(b'\n', &mut buffer) else {
                return;
            };
            if read == 0 {
                return;
            }
            if buffer.last() == Some(&b'\n') {
                buffer.pop();
            }
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
            // Node readline also ends a line at a lone carriage return.
            let text = String::from_utf8_lossy(&buffer).into_owned();
            for line in text.split('\r') {
                if !shared.reading.load(Ordering::SeqCst) {
                    return;
                }
                shared.handle_line(line);
            }
        }
    });
}

fn spawn_stderr_reader(shared: Arc<Shared>, mut stderr: ChildStderr) -> mpsc::Receiver<()> {
    let (done, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let text = String::from_utf8_lossy(&chunk[..read]);
                    let mut buffer = lock(&shared.stderr);
                    buffer.push_str(&text);
                    keep_last_utf16_units(&mut buffer, STDERR_BUFFER_LIMIT);
                }
            }
        }
        let _ = done.send(());
    });
    receiver
}

/// `buffer.slice(-limit)` measured in UTF-16 code units, never splitting a
/// character.
fn keep_last_utf16_units(buffer: &mut String, limit: usize) {
    let total: usize = buffer.chars().map(char::len_utf16).sum();
    if total <= limit {
        return;
    }
    let mut excess = total - limit;
    let mut cut = 0;
    for (index, character) in buffer.char_indices() {
        if excess == 0 {
            cut = index;
            break;
        }
        excess = excess.saturating_sub(character.len_utf16());
        cut = index + character.len_utf8();
    }
    buffer.drain(..cut);
}

fn spawn_exit_watcher(shared: Arc<Shared>, mut child: Child, stderr_done: mpsc::Receiver<()>) {
    thread::spawn(move || {
        let status = child.wait();
        let _ = stderr_done.recv_timeout(STDERR_DRAIN_AFTER_EXIT);
        {
            let mut state = lock(&shared.exit);
            state.exited = true;
        }
        shared.exit_changed.notify_all();
        let message = match status {
            Ok(status) => exit_message(status.code(), exit_signal_name(status)),
            Err(error) => error.to_string(),
        };
        let stderr = lock(&shared.stderr).clone();
        let error = ClientError::plain(js_trim(&format!("{message}\n{stderr}")).to_owned());
        shared.handle_unexpected_termination(&error);
    });
}

fn exit_message(code: Option<i32>, signal: Option<&'static str>) -> String {
    if code == Some(0) && signal.is_none() {
        return "Codex app-server exited".to_owned();
    }
    let code = code.map_or_else(|| "null".to_owned(), |code| code.to_string());
    format!(
        "Codex app-server exited with code {code} and signal {}",
        signal.unwrap_or("null")
    )
}

#[cfg(unix)]
fn exit_signal_name(status: std::process::ExitStatus) -> Option<&'static str> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(signal_name)
}

#[cfg(not(unix))]
fn exit_signal_name(_status: std::process::ExitStatus) -> Option<&'static str> {
    None
}

/// Node `signalCode` names for the POSIX signals a child can die from.
#[must_use]
pub fn signal_name(signal: i32) -> &'static str {
    match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGEMT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        12 => "SIGSYS",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 => "SIGURG",
        17 => "SIGSTOP",
        18 => "SIGTSTP",
        19 => "SIGCONT",
        20 => "SIGCHLD",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGIO",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGINFO",
        30 => "SIGUSR1",
        31 => "SIGUSR2",
        _ => "SIGUNKNOWN",
    }
}

/// Paseo kills the app-server with `tree-kill`, which walks descendants with
/// `pgrep -P` and signals each one. Signalling processes this code never
/// recorded is not allowed here, so the signal goes only to the recorded
/// child pid and to the process group it leads (`spawn_app_server` makes it a
/// group leader). Descendants that moved to another process group are not
/// signalled. A missing group is ignored.
fn signal_process_group(pid: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .arg("-s")
        .arg(signal)
        .arg("--")
        .arg(format!("-{pid}"))
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(script: &str) -> AppServerClient {
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn test child");
        AppServerClient::new(child).expect("client")
    }

    #[test]
    fn requests_racing_dispose_always_settle() {
        // `cat` echoes each request back; the echo is a server request the
        // client answers with `{}`, which `cat` echoes as the response.
        let client = spawn("exec cat");
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let client = client.clone();
                thread::spawn(move || client.request("ping", None, Duration::from_secs(10)))
            })
            .collect();
        client.dispose().expect("dispose");
        for worker in workers {
            match worker.join().expect("worker") {
                Ok(result) => assert_eq!(result, serde_json::json!({})),
                Err(error) => assert_eq!(error.message, CLIENT_CLOSED_MESSAGE),
            }
        }
        assert_eq!(
            client.request("late", None, Duration::from_secs(1)),
            Err(ClientError::plain(CLIENT_CLOSED_MESSAGE))
        );
    }

    /// Spawns `script` as a process group leader, as `spawn_app_server` does,
    /// with `$1` set to a scratch file the script writes a grandchild pid to.
    fn spawn_group_leader(script: &str) -> (AppServerClient, u32, std::path::PathBuf) {
        use std::os::unix::process::CommandExt;
        let file = std::env::temp_dir().join(format!(
            "spocky-p3-transport-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::SeqCst)
        ));
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg("sh")
            .arg(&file)
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn group leader");
        let client = AppServerClient::new(child).expect("client");
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            if let Some(pid) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "grandchild pid not written");
            thread::sleep(Duration::from_millis(20));
        };
        (client, grandchild, file)
    }

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    fn alive(pid: u32) -> bool {
        Command::new("/bin/kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn gone_within(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while alive(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
        true
    }

    #[test]
    fn dispose_signals_the_recorded_process_group() {
        let (client, grandchild, file) =
            spawn_group_leader("sleep 30 & echo $! > \"$1\"; exec cat");
        client.dispose().expect("dispose");
        let _ = std::fs::remove_file(&file);
        assert!(
            gone_within(grandchild, Duration::from_secs(3)),
            "a process in the child's group is signalled"
        );
    }

    #[test]
    fn dispose_leaves_processes_outside_the_group_alone() {
        // The grandchild leaves the group, so only a pid walk could reach it.
        let (client, grandchild, file) = spawn_group_leader(
            "/usr/bin/perl -e 'setpgrp(0, 0); sleep 30' & echo $! > \"$1\"; exec cat",
        );
        // Wait until perl has moved to its own group.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Command::new("/bin/ps")
            .args(["-o", "pgid=", "-p", &grandchild.to_string()])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim() != grandchild.to_string())
            .unwrap_or(true)
        {
            assert!(Instant::now() < deadline, "grandchild never left the group");
            thread::sleep(Duration::from_millis(20));
        }
        client.dispose().expect("dispose");
        let _ = std::fs::remove_file(&file);
        let survived = alive(grandchild);
        // Clean up by the exact pid this test recorded.
        let _ = Command::new("/bin/kill")
            .args(["-s", "KILL", &grandchild.to_string()])
            .output();
        assert!(survived, "no descendant walk outside the group");
    }

    #[test]
    fn a_dropped_responder_replies_with_an_error() {
        // `cat` echoes the request as a server request; its handler drops the
        // responder, whose error reply `cat` echoes back as the response.
        let client = spawn("exec cat");
        client.set_request_handler("ping", Arc::new(|_, _, responder| drop(responder)));
        let outcome = client.request("ping", None, Duration::from_secs(10));
        assert_eq!(
            outcome.map_err(|error| error.message),
            Err(DROPPED_REQUEST_MESSAGE.to_owned())
        );
        client.dispose().expect("dispose");
    }

    #[test]
    fn a_panicking_notification_handler_does_not_stop_the_reader() {
        let client =
            spawn("sleep 1; printf '{\"method\":\"boom\"}\\n{\"method\":\"after\"}\\n'; sleep 5");
        let (seen, received) = mpsc::channel();
        client.set_notification_handler(Arc::new(move |method, _| {
            assert!(method != "boom", "handler panic");
            let _ = seen.send(method.to_owned());
        }));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("after")
        );
        client.dispose().expect("dispose");
    }

    #[test]
    fn exit_message_matches_paseo_wording() {
        assert_eq!(exit_message(Some(0), None), "Codex app-server exited");
        assert_eq!(
            exit_message(Some(3), None),
            "Codex app-server exited with code 3 and signal null"
        );
        assert_eq!(
            exit_message(None, Some("SIGKILL")),
            "Codex app-server exited with code null and signal SIGKILL"
        );
    }

    #[test]
    fn stderr_buffer_keeps_the_last_utf16_units() {
        let mut buffer = "ab".repeat(5000);
        keep_last_utf16_units(&mut buffer, STDERR_BUFFER_LIMIT);
        assert_eq!(buffer.len(), STDERR_BUFFER_LIMIT);
        assert!(buffer.starts_with("ab"));

        let mut wide = format!("x{}", "\u{1f600}".repeat(4096));
        keep_last_utf16_units(&mut wide, STDERR_BUFFER_LIMIT);
        assert_eq!(wide, "\u{1f600}".repeat(4096));
    }

    #[test]
    fn pending_key_accepts_integral_json_numbers_only() {
        assert_eq!(pending_key(&serde_json::json!(7)), Some(7));
        assert_eq!(pending_key(&serde_json::json!(7.0)), Some(7));
        assert_eq!(pending_key(&serde_json::json!(7.5)), None);
        assert_eq!(pending_key(&serde_json::json!(-1)), None);
    }

    #[test]
    fn rpc_error_defaults_message_and_keeps_missing_fields_missing() {
        let error = rpc_error(&serde_json::json!({"code": -32600}));
        assert_eq!(error.message, "Unknown error");
        assert!(error.rpc_code_is(-32600));
        assert_eq!(error.rpc.and_then(|rpc| rpc.data), None);
    }

    #[test]
    fn javascript_truthiness_for_error_fields() {
        assert!(!js_truthy(&Value::Null));
        assert!(!js_truthy(&serde_json::json!(0)));
        assert!(!js_truthy(&serde_json::json!("")));
        assert!(js_truthy(&serde_json::json!({})));
        assert!(js_truthy(&serde_json::json!("x")));
        assert!(js_truthy(&serde_json::json!([])));
        assert!(js_truthy(&serde_json::json!(-1.5)));
        assert!(!js_truthy(&serde_json::json!(0.0)));
        assert!(!js_truthy(&serde_json::json!(false)));
        assert!(js_truthy(&serde_json::json!(true)));
    }
}
