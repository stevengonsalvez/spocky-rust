//! Newline-delimited JSON-RPC client for a spawned `codex app-server` child.
//!
//! Port of pinned Paseo `codex/app-server-transport.ts` (`CodexAppServerClient`)
//! and the process-tree shutdown of `utils/tree-kill.ts`. Requests carry
//! increasing numeric ids starting at 1, responses resolve the matching pending
//! request, server-initiated requests go to registered handlers (unhandled
//! methods answer `{}`), and notifications go to one notification handler in
//! stdout order. An unexpected exit rejects every pending request with
//! `Codex app-server exited ...` plus the last 8192 stderr characters.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
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

/// Completes one server-initiated request. Dropping it without answering leaves
/// the request unanswered, as an unresolved Paseo handler promise would.
pub struct Responder {
    shared: Arc<Shared>,
    id: Value,
}

impl Responder {
    /// `Ok(None)` mirrors a handler resolving `undefined`: the response carries
    /// only the id because `JSON.stringify` drops the `result` key.
    pub fn respond(self, outcome: Result<Option<Value>, String>) {
        let mut response = Map::new();
        response.insert("id".to_owned(), self.id);
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

pub type RequestHandler = Arc<dyn Fn(Option<Value>, Value, Responder) + Send + Sync>;
pub type NotificationHandler = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;
pub type TerminationHandler = Box<dyn FnOnce(ClientError) + Send>;

type PendingSender = mpsc::Sender<Result<Value, ClientError>>;

#[derive(Default)]
struct ExitState {
    exited: bool,
}

struct Shared {
    stdin: Mutex<Option<ChildStdin>>,
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
            stdin: Mutex::new(Some(stdin)),
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
        if self.shared.disposed.load(Ordering::SeqCst) {
            return Err(ClientError::plain(CLIENT_CLOSED_MESSAGE));
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        lock(&self.shared.pending).insert(id, sender);
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
    /// SIGTERM to the process tree, waits 2 s, sends SIGKILL, waits 1 s.
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
        lock(&self.shared.stdin).take();
        if self.shared.has_exited() {
            return Ok(());
        }
        signal_process_tree(self.pid, "TERM");
        if self.shared.wait_for_exit(GRACEFUL_SHUTDOWN_TIMEOUT) {
            return Ok(());
        }
        signal_process_tree(self.pid, "KILL");
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
        if let Some(stdin) = lock(&self.stdin).as_mut() {
            // Node reports write failures asynchronously and the exit handler
            // rejects the request; a broken pipe here is handled the same way.
            let _ = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush());
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
            handler(error.clone());
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
                    id: id.clone(),
                };
                match handler {
                    Some(handler) => handler(raw.get("params").cloned(), id.clone(), responder),
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
                handler(method, raw.get("params").cloned());
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

/// JavaScript truthiness for a parsed JSON value.
pub(crate) fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
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

/// `tree-kill` on macOS: walk descendants with `pgrep -P`, then signal each
/// parent's children before the parent. Exited processes are ignored.
fn signal_process_tree(root: u32, signal: &str) {
    let mut order: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        let children = child_pids(parent);
        queue.extend(children.iter().copied());
        order.push((parent, children));
    }
    order.sort_by_key(|(pid, _)| *pid);
    let mut killed = std::collections::HashSet::new();
    for (parent, children) in order {
        for child in children {
            if killed.insert(child) {
                send_signal(child, signal);
            }
        }
        if killed.insert(parent) {
            send_signal(parent, signal);
        }
    }
}

fn child_pids(parent: u32) -> Vec<u32> {
    let Ok(output) = Command::new("pgrep")
        .arg("-P")
        .arg(parent.to_string())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

fn send_signal(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg("-s")
        .arg(signal)
        .arg(pid.to_string())
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
