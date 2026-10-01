//! The WebSocket server: connection admission, hello, `server_info`, ping,
//! session attach and reconnect, and the close codes.
//!
//! Source at Paseo `5de45e2`: `VoiceAssistantWebSocketServer` in
//! `websocket-server.ts`. One thread reads each socket; frames bound for a
//! socket go through its queue so that a send, a rejection frame and the close
//! that follows it keep their order.
//!
//! Not ported (outside the vertical slice): relay, hub and plugin sockets, the
//! binary-frame fast path, runtime metrics, and the slow-request log.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize as _;
use serde_json::{Map, Value};
use spocky_contracts::js_value::{JsValue, parse as parse_js};
use spocky_contracts::json::{JsValueDeserializer, js_wire_text};
use spocky_contracts::ws::{
    DaemonPermission, Hello, HelloRejected, HelloRejectedReason, ServerCapabilities,
    ServerFeatureGates, ServerId, WsControlInbound, WsControlOutbound,
};
use tungstenite::error::CapacityError;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tungstenite::{Error as WsError, Message, WebSocket};
use uuid::Uuid;

use crate::admission::{
    AdmissionFailure, AdmissionTransport, PasswordVerifier, SessionAdmission,
    resolve_session_admission,
};
use crate::bearer::{
    extract_http_bearer_token, extract_ws_bearer_protocol, extract_ws_bearer_token,
};
use crate::hostnames::Hostnames;
use crate::http::{
    HttpContext, HttpResponse, ParsedHead, current_ms, handle_request, is_upgrade_request,
    parse_head,
};
use crate::js;
use crate::log::Logger;
use crate::server_info::{ServerInfoInputs, server_info_message};
use crate::session_api::{
    ProtocolFailure, SessionBackend, SessionHandle, SessionOpen, SessionSink, SocketId,
};
use crate::upgrade::{
    ConnectionLifecycle, UpgradeDecision, UpgradePolicy, UpgradeRequest, evaluate_upgrade,
};

const WS_CLOSE_HELLO_TIMEOUT: u16 = 4001;
const WS_CLOSE_INVALID_HELLO: u16 = 4002;
const WS_CLOSE_INCOMPATIBLE_PROTOCOL: u16 = 4003;
const WS_CLOSE_DAEMON_AUTH_FAILED: u16 = 4401;
const WS_CLOSE_SERVER_SHUTDOWN: u16 = 1001;
const WS_CLOSE_MAX_PAYLOAD: u16 = 1009;
/// `ws` `maxPayload` default.
const MAX_PAYLOAD_BYTES: usize = 100 * 1024 * 1024;
/// `MAX_PHYSICAL_SOCKET_BUFFERED_BYTES`.
const MAX_BUFFERED_BYTES: usize = 64 * 1024 * 1024;
/// How often a connection thread wakes to check queues and deadlines.
const POLL: Duration = Duration::from_millis(20);
const CLEANUP_POLL: Duration = Duration::from_millis(100);
/// Node's `headersTimeout`.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(60);

/// The three timers of the baseline, overridable for tests.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// `HELLO_TIMEOUT_MS`.
    pub hello: Duration,
    /// `EXTERNAL_SESSION_DISCONNECT_GRACE_MS`.
    pub reconnect_grace: Duration,
    /// `APPLICATION_SOCKET_LEASE_MS`, enforced to `APPLICATION_SOCKET_LEASE_CHECK_INTERVAL_MS`.
    pub application_lease: Duration,
    /// How long a socket waits for the peer's close frame (`ws` `closeTimeout`).
    pub close: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(15),
            reconnect_grace: Duration::from_secs(90),
            application_lease: Duration::from_secs(45),
            close: Duration::from_secs(30),
        }
    }
}

/// Daemon facts the server reads.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct ServerConfig {
    pub server_id: ServerId,
    pub daemon_version: String,
    pub hostname: String,
    pub hostnames: Option<Hostnames>,
    pub allowed_origins: HashSet<String>,
    /// bcrypt hash of the daemon password.
    pub password_hash: Option<String>,
    pub desktop_managed: bool,
    pub workspace_labels: bool,
    /// `wsConfig.daemonStatusRpc !== false`.
    pub advertise_daemon_status_rpc: bool,
    /// `wsConfig.relayConfig !== false`.
    pub advertise_relay_config: bool,
    /// `wsConfig.startPaused === true`.
    pub start_paused: bool,
    pub capabilities: Option<ServerCapabilities>,
    pub timeouts: Timeouts,
}

/// Collaborators the server calls.
pub struct ServerDeps {
    pub backend: Arc<dyn SessionBackend>,
    pub verifier: Arc<dyn PasswordVerifier>,
    /// `auth.localCredential()`: the running daemon's credential, if any.
    pub local_credential: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub logger: Arc<dyn Logger>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A byte stream the server can serve: TCP or a Unix socket.
pub trait Connection: Read + Write + Send + 'static {
    /// Sets the read timeout used to poll queues and deadlines.
    ///
    /// # Errors
    ///
    /// Any error from the socket option.
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Bounds a blocking write, so a peer that stops reading cannot stall the
    /// connection thread: the write returns and the rest stays queued.
    ///
    /// # Errors
    ///
    /// Any error from the socket option.
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Switches blocking mode. A socket accepted from a non-blocking listener
    /// inherits that mode on macOS and BSD, which makes a read timeout a no-op.
    ///
    /// # Errors
    ///
    /// Any error from the socket option.
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
    /// `remoteAddress`; `None` for a Unix socket (`local_ipc`).
    fn remote_address(&self) -> Option<IpAddr>;
    /// Drops the transport without a close frame (`terminate()`).
    fn shutdown(&self);
}

impl Connection for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        TcpStream::set_nonblocking(self, nonblocking)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
    fn remote_address(&self) -> Option<IpAddr> {
        self.peer_addr().ok().map(|address| address.ip())
    }
    fn shutdown(&self) {
        let _ = TcpStream::shutdown(self, std::net::Shutdown::Both);
    }
}

#[cfg(unix)]
impl Connection for UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        UnixStream::set_nonblocking(self, nonblocking)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_write_timeout(self, timeout)
    }
    fn remote_address(&self) -> Option<IpAddr> {
        None
    }
    fn shutdown(&self) {
        let _ = UnixStream::shutdown(self, std::net::Shutdown::Both);
    }
}

/// Frames queued for one socket.
enum Outbound {
    Text(String),
    Close { code: Option<u16>, reason: String },
    Terminate,
}

struct SocketEntry {
    queue: Sender<Outbound>,
    queued_bytes: Arc<AtomicUsize>,
    /// Set when the socket must be dropped at once. The connection thread checks
    /// it every pass, so it takes effect without draining the queue first.
    terminate: Arc<AtomicBool>,
}

/// A client session and the sockets attached to it (`SessionConnection`).
struct SessionConnection {
    session: Arc<dyn SessionHandle>,
    session_key: String,
    client_id: String,
    state: Mutex<ConnectionState>,
    /// Set by the first cleanup, so the grace thread and `close` cannot both
    /// run `SessionHandle::cleanup`.
    cleaned: AtomicBool,
}

impl SessionConnection {
    fn cleanup_once(&self) {
        if !self.cleaned.swap(true, Ordering::SeqCst) {
            self.session.cleanup();
        }
    }
}

struct ConnectionState {
    app_version: Option<String>,
    client_capabilities: Option<Value>,
    sockets: Vec<SocketId>,
    /// Set while no socket is attached; the session is cleaned up at this time.
    cleanup_at: Option<Instant>,
}

#[derive(Default)]
struct Registry {
    sockets: HashMap<SocketId, SocketEntry>,
    attached: HashMap<SocketId, Arc<SessionConnection>>,
    by_key: HashMap<String, Arc<SessionConnection>>,
}

struct Shared {
    config: ServerConfig,
    deps: ServerDeps,
    lifecycle: Mutex<ConnectionLifecycle>,
    next_socket: AtomicU64,
    /// Connections being served, and the most the process may hold.
    active_connections: AtomicUsize,
    max_connections: AtomicUsize,
    /// Bytes queued for one socket and not yet written to the kernel; past this
    /// the socket is terminated (`MAX_PHYSICAL_SOCKET_BUFFERED_BYTES`).
    max_buffered_bytes: AtomicUsize,
    janitor_started: AtomicBool,
    /// One lock per session key. It serializes "find or create the session for
    /// this client" with the grace-period cleanup of the same key, so two hellos
    /// for one client cannot both create a session and a resume cannot land on a
    /// session being cleaned up. A slow `SessionBackend::open` holds up only its
    /// own client. Always taken before `registry`.
    key_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    registry: Mutex<Registry>,
    stop: AtomicBool,
    /// `formatListenTarget(boundListenTarget)` and whether it is TCP.
    listen: Mutex<(String, bool)>,
}

impl Shared {
    fn lifecycle(&self) -> ConnectionLifecycle {
        *lock(&self.lifecycle)
    }

    /// Queues a frame for a socket; `false` when the socket is gone.
    fn enqueue(&self, socket: SocketId, outbound: Outbound) -> bool {
        let registry = lock(&self.registry);
        let Some(entry) = registry.sockets.get(&socket) else {
            return false;
        };
        let bytes = match &outbound {
            Outbound::Text(text) => text.len(),
            _ => 0,
        };
        let max_buffered = self.max_buffered_bytes.load(Ordering::SeqCst);
        // Count and check in one step, so concurrent senders cannot jointly
        // pass the limit and a refused frame never inflates the total.
        let counted =
            entry
                .queued_bytes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |queued| {
                    queued
                        .checked_add(bytes)
                        .filter(|total| *total <= max_buffered)
                });
        if counted.is_err() {
            self.deps.logger.warn(
                &[("maxBufferedBytes", &max_buffered.to_string())],
                "Closing physical WebSocket at outbound high-water mark",
            );
            entry.terminate.store(true, Ordering::SeqCst);
            return entry.queue.send(Outbound::Terminate).is_ok();
        }
        entry.queue.send(outbound).is_ok()
    }

    fn send_json(&self, socket: SocketId, value: &Value) {
        // `JSON.stringify` writes a lone surrogate as `\udXXX`; the value holds it
        // in the JavaScript text encoding of spocky-contracts.
        self.enqueue(socket, Outbound::Text(js_wire_text(&value.to_string())));
    }

    fn close_socket(&self, socket: SocketId, code: Option<u16>, reason: &str) {
        self.enqueue(
            socket,
            Outbound::Close {
                code,
                reason: reason.to_owned(),
            },
        );
    }

    fn server_info_inputs(&self, session: &dyn SessionHandle) -> ServerInfoInputs {
        ServerInfoInputs {
            server_id: self.config.server_id.clone(),
            hostname: self.config.hostname.clone(),
            version: self.config.daemon_version.clone(),
            permissions: session.permissions(),
            desktop_managed: self.config.desktop_managed,
            capabilities: self.config.capabilities.clone(),
            gates: ServerFeatureGates {
                workspace_labels: self.config.workspace_labels,
                daemon_status_rpc: self.config.advertise_daemon_status_rpc,
                relay_config: self.config.advertise_relay_config,
                desktop_managed: self.config.desktop_managed,
            },
        }
    }

    /// The lock for a session key. Locks nobody holds or waits on are dropped
    /// here, which bounds the map to the keys in use.
    fn key_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = lock(&self.key_locks);
        locks.retain(|_, held| Arc::strong_count(held) > 1);
        Arc::clone(locks.entry(key.to_owned()).or_default())
    }

    /// Ends a session: detach its sockets, forget its key, call `cleanup`.
    fn cleanup_connection(&self, connection: &Arc<SessionConnection>, message: &str) {
        {
            let key_lock = self.key_lock(&connection.session_key);
            let _serialized = lock(&key_lock);
            let mut registry = lock(&self.registry);
            let mut state = lock(&connection.state);
            // A hello that resumed the session since the caller looked wins.
            if !state.sockets.is_empty() || state.cleanup_at.is_none_or(|at| at > Instant::now()) {
                return;
            }
            state.cleanup_at = None;
            for socket in state.sockets.drain(..) {
                registry.attached.remove(&socket);
            }
            if registry
                .by_key
                .get(&connection.session_key)
                .is_some_and(|existing| Arc::ptr_eq(existing, connection))
            {
                registry.by_key.remove(&connection.session_key);
            }
        }
        self.deps
            .logger
            .info(&[("clientId", &connection.client_id)], message);
        connection.cleanup_once();
    }
}

/// `wrapSessionMessage`.
fn wrap_session_message(message: &Value) -> Value {
    let mut wrapped = Map::new();
    wrapped.insert("type".to_owned(), Value::from("session"));
    wrapped.insert("message".to_owned(), message.clone());
    Value::Object(wrapped)
}

/// The `onMessage` callbacks the baseline gives a `Session`.
struct ConnectionSink {
    shared: Weak<Shared>,
    connection: Weak<SessionConnection>,
}

impl SessionSink for ConnectionSink {
    fn send_to_connection(&self, message: &Value) {
        let (Some(shared), Some(connection)) = (self.shared.upgrade(), self.connection.upgrade())
        else {
            return;
        };
        let wrapped = wrap_session_message(message);
        let sockets = lock(&connection.state).sockets.clone();
        for socket in sockets {
            shared.send_json(socket, &wrapped);
        }
    }

    fn send_to_source(&self, source: SocketId, message: &Value) {
        let (Some(shared), Some(connection)) = (self.shared.upgrade(), self.connection.upgrade())
        else {
            return;
        };
        if lock(&connection.state).sockets.contains(&source) {
            shared.send_json(source, &wrap_session_message(message));
        }
    }

    fn buffered_amount(&self, source: Option<SocketId>) -> Option<usize> {
        let (shared, connection) = (self.shared.upgrade()?, self.connection.upgrade()?);
        let registry = lock(&shared.registry);
        let queued = |socket: &SocketId| {
            registry
                .sockets
                .get(socket)
                .map(|entry| entry.queued_bytes.load(Ordering::SeqCst))
        };
        match source {
            Some(source) => queued(&source),
            None => lock(&connection.state)
                .sockets
                .iter()
                .filter_map(queued)
                .max(),
        }
    }
}

/// Identity fields logged for a connection (`WebSocketConnectionIdentity`).
struct Identity {
    connection_id: String,
    peer: &'static str,
    host: Option<String>,
    origin: Option<String>,
    remote_address: Option<String>,
}

impl Identity {
    fn fields(&self) -> Vec<(&str, &str)> {
        let mut fields = vec![
            ("connectionId", self.connection_id.as_str()),
            ("transport", "direct"),
            ("peer", self.peer),
        ];
        if let Some(host) = &self.host {
            fields.push(("host", host));
        }
        if let Some(origin) = &self.origin {
            fields.push(("origin", origin));
        }
        if let Some(address) = &self.remote_address {
            fields.push(("remoteAddress", address));
        }
        fields
    }
}

/// `isLoopbackAddress`.
fn is_loopback_address(address: &str) -> bool {
    let normalized = address.to_lowercase();
    if normalized == "::1" || normalized == "0:0:0:0:0:0:0:1" {
        return true;
    }
    normalized
        .strip_prefix("::ffff:")
        .unwrap_or(&normalized)
        .starts_with("127.")
}

/// `extractRequestInfoFromUnknownWsInbound`.
fn extract_request_info(payload: &JsValue) -> Option<(String, Option<String>)> {
    let record = payload.as_object()?;
    if record.get("type").and_then(JsValue::as_str) == Some("session")
        && let Some(message) = record.get("message").and_then(JsValue::as_object)
        && let Some(request_id) = message.get("requestId").and_then(JsValue::as_str)
    {
        return Some((
            request_id.to_owned(),
            message
                .get("type")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
        ));
    }
    let request_id = record.get("requestId").and_then(JsValue::as_str)?;
    Some((
        request_id.to_owned(),
        record
            .get("type")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
    ))
}

/// zod's `invalid_union` issue for a `type` the inbound schema does not list.
const ZOD_NO_DISCRIMINATOR: &str = "[\n  {\n    \"code\": \"invalid_union\",\n    \"errors\": [],\n    \"note\": \"No matching discriminator\",\n    \"discriminator\": \"type\",\n    \"options\": [\n      \"ping\",\n      \"hello\",\n      \"recording_state\",\n      \"session\"\n    ],\n    \"path\": [\n      \"type\"\n    ],\n    \"message\": \"Invalid discriminator value. Expected 'ping' | 'hello' | 'recording_state' | 'session'\"\n  }\n]";

/// zod's `invalid_type` issue ("expected object") at `path`.
fn zod_invalid_type(path: &[&str], received: &JsValue) -> String {
    let kind = match received {
        JsValue::Undefined => "undefined",
        JsValue::Null => "null",
        JsValue::Bool(_) => "boolean",
        JsValue::Number(_) => "number",
        JsValue::String(_) => "string",
        JsValue::Array(_) => "array",
        JsValue::Object(_) => "object",
    };
    let path = if path.is_empty() {
        "[]".to_owned()
    } else {
        format!(
            "[\n{}\n    ]",
            path.iter()
                .map(|key| format!("      \"{key}\""))
                .collect::<Vec<_>>()
                .join(",\n")
        )
    };
    format!(
        "[\n  {{\n    \"code\": \"invalid_type\",\n    \"expected\": \"object\",\n    \"path\": {path},\n    \"message\": \"Invalid input: expected object, received {kind}\"\n  }}\n]"
    )
}

/// Nesting a session message may have before the backend seam, which takes a
/// `serde_json::Value` (recursive to build and to drop), refuses it. Control
/// frames have no such limit: they are read straight from the parsed value.
const SESSION_MESSAGE_MAX_DEPTH: usize = 128;

/// The session message as a `serde_json::Value` for the backend seam; `None`
/// past [`SESSION_MESSAGE_MAX_DEPTH`]. Whole numbers stay integers so a
/// message echoed back is written as `JSON.stringify` writes it.
fn session_value(value: &JsValue, depth: usize) -> Option<Value> {
    if depth > SESSION_MESSAGE_MAX_DEPTH {
        return None;
    }
    Some(match value {
        JsValue::Undefined | JsValue::Null => Value::Null,
        JsValue::Bool(flag) => Value::Bool(*flag),
        JsValue::Number(number) => {
            #[allow(clippy::cast_possible_truncation)]
            if number.fract() == 0.0 && number.abs() < 9_007_199_254_740_992.0 {
                Value::from(*number as i64)
            } else {
                serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
            }
        }
        JsValue::String(text) => Value::String(text.clone()),
        JsValue::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| session_value(item, depth + 1))
                .collect::<Option<_>>()?,
        ),
        JsValue::Object(object) => {
            let mut map = Map::new();
            for (key, item) in object.iter() {
                map.insert(key.to_owned(), session_value(item, depth + 1)?);
            }
            Value::Object(map)
        }
    })
}

/// [`Phase`] without borrowing the socket task.
enum PhaseKind {
    Pending,
    Active(Arc<SessionConnection>),
    Done,
}

enum Phase {
    Pending(Pending),
    Active(Arc<SessionConnection>),
    /// Closed by the server or never admitted; further frames are dropped.
    Done,
}

struct Pending {
    deadline: Instant,
    admission: Option<SessionAdmission>,
}

/// What a frame is, once it passes the inbound schema.
enum Inbound {
    Control(Box<WsControlInbound>),
    Session(Value),
}

/// A running server.
#[derive(Clone)]
pub struct Server {
    shared: Arc<Shared>,
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

/// A listener the server accepts on.
pub struct ListenHandle {
    local_addr: Option<SocketAddr>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ListenHandle {
    /// The bound TCP address, for a listener created with port 0.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_addr
    }

    /// Stops accepting and waits for the accept thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ListenHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Server {
    /// `new VoiceAssistantWebSocketServer(...)`.
    #[must_use]
    pub fn new(config: ServerConfig, deps: ServerDeps) -> Self {
        let lifecycle = if config.start_paused {
            ConnectionLifecycle::Starting
        } else {
            ConnectionLifecycle::Accepting
        };
        let shared = Arc::new(Shared {
            config,
            deps,
            lifecycle: Mutex::new(lifecycle),
            next_socket: AtomicU64::new(0),
            active_connections: AtomicUsize::new(0),
            max_connections: AtomicUsize::new(default_max_connections()),
            max_buffered_bytes: AtomicUsize::new(MAX_BUFFERED_BYTES),
            janitor_started: AtomicBool::new(false),
            key_locks: Mutex::new(HashMap::new()),
            registry: Mutex::new(Registry::default()),
            stop: AtomicBool::new(false),
            listen: Mutex::new((String::new(), true)),
        });
        Self {
            shared,
            threads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Starts the grace-period cleanup thread once; the first listener does it.
    fn start_janitor(&self) -> io::Result<()> {
        if self.shared.janitor_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let janitor = thread::Builder::new()
            .name("spocky-cleanup".to_owned())
            .spawn(move || {
                while !shared.stop.load(Ordering::SeqCst) {
                    thread::sleep(CLEANUP_POLL);
                    let due: Vec<Arc<SessionConnection>> = lock(&shared.registry)
                        .by_key
                        .values()
                        .filter(|connection| {
                            let state = lock(&connection.state);
                            state.sockets.is_empty()
                                && state.cleanup_at.is_some_and(|at| at <= Instant::now())
                        })
                        .cloned()
                        .collect();
                    for connection in due {
                        shared
                            .cleanup_connection(&connection, "Client disconnected (grace timeout)");
                    }
                }
            })
            .inspect_err(|_| self.shared.janitor_started.store(false, Ordering::SeqCst))?;
        lock(&self.threads).push(janitor);
        Ok(())
    }

    /// Sets the per-socket outbound high-water mark. The default is 64 MiB.
    pub fn set_max_buffered_bytes(&self, max: usize) {
        self.shared.max_buffered_bytes.store(max, Ordering::SeqCst);
    }

    /// Caps concurrent connections. The default is the process's open-file limit
    /// less a reserve, which is the cap Node meets when `accept` hits `EMFILE`.
    pub fn set_max_connections(&self, max: usize) {
        self.shared.max_connections.store(max, Ordering::SeqCst);
    }

    /// Records the address plain HTTP reports under `/api/status`.
    pub fn set_listen(&self, listen: &str, tcp: bool) {
        *lock(&self.shared.listen) = (listen.to_owned(), tcp);
    }

    /// `beginAcceptingConnections`.
    pub fn begin_accepting_connections(&self) {
        let mut lifecycle = lock(&self.shared.lifecycle);
        if *lifecycle == ConnectionLifecycle::Starting {
            *lifecycle = ConnectionLifecycle::Accepting;
        }
    }

    /// `prepareForShutdown`.
    pub fn prepare_for_shutdown(&self) {
        *lock(&self.shared.lifecycle) = ConnectionLifecycle::Stopping;
    }

    /// Accepts TCP connections until the handle is stopped.
    ///
    /// # Errors
    ///
    /// Any error from reading the local address or setting non-blocking mode.
    pub fn serve_tcp(&self, listener: TcpListener) -> io::Result<ListenHandle> {
        self.start_janitor()?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (server, flag) = (self.clone(), Arc::clone(&stop));
        let thread = thread::Builder::new()
            .name("spocky-accept".to_owned())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) && !server.shared.stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => server.spawn_connection(stream),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(5)),
                    }
                }
            })?;
        Ok(ListenHandle {
            local_addr: Some(local_addr),
            stop,
            thread: Some(thread),
        })
    }

    /// Accepts Unix socket connections until the handle is stopped.
    ///
    /// # Errors
    ///
    /// Any error from setting non-blocking mode.
    #[cfg(unix)]
    pub fn serve_unix(&self, listener: UnixListener) -> io::Result<ListenHandle> {
        self.start_janitor()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (server, flag) = (self.clone(), Arc::clone(&stop));
        let thread = thread::Builder::new()
            .name("spocky-accept".to_owned())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) && !server.shared.stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => server.spawn_connection(stream),
                        Err(_) => thread::sleep(Duration::from_millis(5)),
                    }
                }
            })?;
        Ok(ListenHandle {
            local_addr: None,
            stop,
            thread: Some(thread),
        })
    }

    /// Serves an accepted connection on its own thread. Past the connection cap,
    /// or when no thread can be started, the connection is closed at once, as
    /// libuv closes a connection it accepts after `EMFILE`.
    fn spawn_connection<C: Connection>(&self, stream: C) {
        let shared = Arc::clone(&self.shared);
        let active = shared.active_connections.fetch_add(1, Ordering::SeqCst) + 1;
        let guard = ConnectionGuard(Arc::clone(&shared));
        if active > shared.max_connections.load(Ordering::SeqCst) {
            shared.deps.logger.warn(
                &[(
                    "maxConnections",
                    &shared.max_connections.load(Ordering::SeqCst).to_string(),
                )],
                "Connection limit reached; closing the new connection",
            );
            return;
        }
        let spawned = thread::Builder::new()
            .name("spocky-connection".to_owned())
            .spawn(move || {
                let _guard = guard;
                serve_connection(&shared, Box::new(stream));
            });
        if let Err(error) = spawned {
            self.shared.deps.logger.warn(
                &[("err", &error.to_string())],
                "Failed to start a connection thread",
            );
        }
    }

    /// `close`: stop accepting, close every socket, clean up every session.
    pub fn close(&self) {
        self.prepare_for_shutdown();
        let (sockets, connections) = {
            let registry = lock(&self.shared.registry);
            let sockets: Vec<SocketId> = registry.sockets.keys().copied().collect();
            let mut connections: Vec<Arc<SessionConnection>> =
                registry.attached.values().cloned().collect();
            connections.extend(registry.by_key.values().cloned());
            (sockets, connections)
        };
        for socket in &sockets {
            self.shared.close_socket(*socket, None, "");
        }
        let mut seen: Vec<Arc<SessionConnection>> = Vec::new();
        for connection in connections {
            if !seen.iter().any(|known| Arc::ptr_eq(known, &connection)) {
                connection.cleanup_once();
                seen.push(connection);
            }
        }
        let deadline = Instant::now() + self.shared.config.timeouts.close;
        while !lock(&self.shared.registry).sockets.is_empty() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        self.shared.stop.store(true, Ordering::SeqCst);
        let handles: Vec<JoinHandle<()>> = lock(&self.threads).drain(..).collect();
        for handle in handles {
            let _ = handle.join();
        }
        let mut registry = lock(&self.shared.registry);
        registry.attached.clear();
        registry.by_key.clear();
    }
}

/// The text of a contracts message, whether it is a `&str` or a `String`.
fn text_of(message: &impl AsRef<str>) -> &str {
    message.as_ref()
}

/// A read or write that timed out or would block.
fn is_would_block(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Counts a connection while its thread is alive.
struct ConnectionGuard(Arc<Shared>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.active_connections.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The open-file limit less a reserve for the daemon's own files and pipes.
fn default_max_connections() -> usize {
    use rustix::process::{Resource, getrlimit};
    getrlimit(Resource::Nofile)
        .current
        .map_or(usize::MAX, |limit| {
            usize::try_from(limit)
                .unwrap_or(usize::MAX)
                .saturating_sub(64)
        })
        .max(16)
}

/// Puts an accepted socket in blocking mode with the polling read timeout. The
/// listeners are non-blocking so they can be stopped, and on macOS the accepted
/// socket inherits that mode, so without this every read returns at once and the
/// connection thread spins.
fn configure_accepted(stream: &dyn Connection) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(POLL))?;
    stream.set_read_timeout(Some(POLL))
}

/// Reads one request head, then serves an upgrade or a plain HTTP response.
fn serve_connection(shared: &Arc<Shared>, mut stream: Box<dyn Connection>) {
    if configure_accepted(stream.as_ref()).is_err() {
        return;
    }
    let started = Instant::now();
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 4096];
    let (request, used) = loop {
        match parse_head(&buffer) {
            ParsedHead::Complete(request, used) => break (request, used),
            ParsedHead::Partial => {}
            ParsedHead::Invalid => {
                let _ = stream.write_all(&HttpResponse::client_error(400).to_bytes());
                return;
            }
            ParsedHead::TooLarge => {
                let _ = stream.write_all(&HttpResponse::client_error(431).to_bytes());
                return;
            }
        }
        if started.elapsed() > HEADERS_TIMEOUT || shared.stop.load(Ordering::SeqCst) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
        }
    };
    let remaining = buffer.split_off(used.min(buffer.len()));

    if is_upgrade_request(&request) {
        upgrade(shared, stream, &request, remaining);
        return;
    }
    let (listen, tcp_listener) = lock(&shared.listen).clone();
    let local_credential = (shared.deps.local_credential)();
    let response = handle_request(
        &request,
        &HttpContext {
            server_id: shared.config.server_id.as_str(),
            hostname: &shared.config.hostname,
            version: &shared.config.daemon_version,
            listen: &listen,
            tcp_listener,
            hostnames: shared.config.hostnames.as_ref(),
            allowed_origins: &shared.config.allowed_origins,
            password_hash: shared.config.password_hash.as_deref(),
            local_credential: local_credential.as_deref(),
            verifier: shared.deps.verifier.as_ref(),
            now_ms: current_ms(),
        },
    );
    let _ = stream.write_all(&response.to_bytes());
    let _ = stream.flush();
    stream.shutdown();
}

fn upgrade(
    shared: &Arc<Shared>,
    mut stream: Box<dyn Connection>,
    request: &UpgradeRequest,
    remaining: Vec<u8>,
) {
    let policy = UpgradePolicy {
        lifecycle: shared.lifecycle(),
        hostnames: shared.config.hostnames.as_ref(),
        allowed_origins: &shared.config.allowed_origins,
        password_set: shared
            .config
            .password_hash
            .as_deref()
            .is_some_and(|hash| !hash.is_empty()),
    };
    match evaluate_upgrade(request, &policy) {
        UpgradeDecision::Reject { response, abort } => {
            if abort.code == 403 {
                shared.deps.logger.warn(
                    &[("host", request.header("host").as_deref().unwrap_or(""))],
                    &format!("Rejected connection: {}", abort.message),
                );
            }
            let _ = stream.write_all(&response);
            let _ = stream.flush();
            stream.shutdown();
        }
        UpgradeDecision::Accept(response) => {
            if stream.write_all(&response).is_err() || stream.flush().is_err() {
                return;
            }
            run_socket(shared, stream, request, remaining);
        }
    }
}

struct SocketTask {
    shared: Arc<Shared>,
    id: SocketId,
    ws: WebSocket<Box<dyn Connection>>,
    queue: Receiver<Outbound>,
    queued_bytes: Arc<AtomicUsize>,
    terminate: Arc<AtomicBool>,
    /// Bytes written to the library's buffer that a successful flush has not yet
    /// confirmed as handed to the kernel.
    unflushed: usize,
    identity: Identity,
    phase: Phase,
    /// Set once a close frame went out; the peer has until then to answer.
    closing_deadline: Option<Instant>,
    /// `applicationSocketLease` deadline, set by the first ping.
    lease_deadline: Option<Instant>,
    close_details: (Option<u16>, Option<String>),
}

/// `attachAuthenticatedSocket` and `attachSocket`, then the read loop.
fn run_socket(
    shared: &Arc<Shared>,
    stream: Box<dyn Connection>,
    request: &UpgradeRequest,
    remaining: Vec<u8>,
) {
    let remote = stream.remote_address();
    // The library's write buffer holds frames the kernel has not taken yet. Bound
    // it at the high-water mark so a peer that stops reading cannot grow it.
    let write_buffer = WebSocketConfig::default().write_buffer_size;
    let max_write_buffer = shared
        .max_buffered_bytes
        .load(Ordering::SeqCst)
        .saturating_add(write_buffer + 1);
    let config = WebSocketConfig::default()
        .max_write_buffer_size(max_write_buffer)
        .max_message_size(Some(MAX_PAYLOAD_BYTES))
        .max_frame_size(Some(MAX_PAYLOAD_BYTES));
    let ws = WebSocket::from_partially_read(stream, remaining, Role::Server, Some(config));

    let id = shared.next_socket.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = mpsc::channel();
    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let terminate = Arc::new(AtomicBool::new(false));
    lock(&shared.registry).sockets.insert(
        id,
        SocketEntry {
            queue: tx,
            queued_bytes: Arc::clone(&queued_bytes),
            terminate: Arc::clone(&terminate),
        },
    );
    let remote_address = remote.map(|address| address.to_string());
    let peer = match &remote_address {
        None => "local_ipc",
        Some(address) if is_loopback_address(address) => "loopback",
        Some(_) => "external",
    };
    let identity = Identity {
        connection_id: format!("conn_{}", Uuid::new_v4().simple()),
        peer,
        host: request.header("host").filter(|host| !host.is_empty()),
        origin: request.header("origin").filter(|origin| !origin.is_empty()),
        remote_address,
    };
    let mut task = SocketTask {
        shared: Arc::clone(shared),
        id,
        ws,
        queue: rx,
        queued_bytes,
        terminate,
        unflushed: 0,
        identity,
        phase: Phase::Done,
        closing_deadline: None,
        lease_deadline: None,
        close_details: (None, None),
    };
    task.attach(request);
    task.run();
    task.detach();
}

impl SocketTask {
    fn logger(&self) -> &dyn Logger {
        self.shared.deps.logger.as_ref()
    }

    fn close(&self, code: u16, reason: &str) {
        self.shared.close_socket(self.id, Some(code), reason);
    }

    /// `attachAuthenticatedSocket` then `attachSocket`.
    fn attach(&mut self, request: &UpgradeRequest) {
        let password_hash = self
            .shared
            .config
            .password_hash
            .clone()
            .filter(|hash| !hash.is_empty());
        // COMPAT(headerAuth): added in v0.9.1, remove after 2026-03-24.
        let protocol = request.header("sec-websocket-protocol");
        let ws_protocol = extract_ws_bearer_protocol(protocol.as_deref());
        let authorization = request.header("authorization");
        let token: Option<String> = extract_http_bearer_token(authorization.as_deref())
            .map(str::to_owned)
            .or_else(|| extract_ws_bearer_token(ws_protocol).map(str::to_owned));
        let has_header_credential = token.is_some();
        if let (Some(hash), Some(token)) = (&password_hash, &token)
            && !self.shared.deps.verifier.verify(token, hash)
        {
            self.logger().warn(
                &[("hasToken", "true")],
                "Rejected WebSocket connection with invalid daemon password",
            );
            self.close(WS_CLOSE_DAEMON_AUTH_FAILED, "Incorrect password");
            self.phase = Phase::Done;
            return;
        }

        match self.shared.lifecycle() {
            ConnectionLifecycle::Stopping | ConnectionLifecycle::Starting => {
                self.close(WS_CLOSE_SERVER_SHUTDOWN, "Server shutting down");
                self.phase = Phase::Done;
                return;
            }
            ConnectionLifecycle::Accepting => {}
        }

        let mut fields = self.identity.fields();
        fields.push(("totalPendingConnections", "1"));
        self.logger()
            .info(&fields, "Client connected; awaiting hello");
        self.phase = Phase::Pending(Pending {
            deadline: Instant::now() + self.shared.config.timeouts.hello,
            admission: has_header_credential.then(SessionAdmission::owner),
        });
    }

    fn run(&mut self) {
        let _ = self.ws.get_mut().set_read_timeout(Some(POLL));
        loop {
            if !self.drain_queue() {
                return;
            }
            if self
                .closing_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return;
            }
            self.check_deadlines();
            match self.ws.read() {
                Ok(Message::Text(text)) => self.on_data(text.as_str()),
                Ok(Message::Binary(bytes)) => self.on_data(&String::from_utf8_lossy(&bytes)),
                Ok(Message::Close(frame)) => {
                    self.close_details = (
                        frame.as_ref().map(|frame| u16::from(frame.code)),
                        frame.map(|frame| frame.reason.to_string()),
                    );
                }
                Ok(_) => {}
                Err(WsError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(WsError::Capacity(CapacityError::MessageTooLong { .. })) => {
                    // ws 8.20.0 `receiverOnError` closes with the status code only.
                    self.close(WS_CLOSE_MAX_PAYLOAD, "");
                    self.phase = Phase::Done;
                }
                Err(WsError::Utf8(_)) => {
                    self.close(1007, "");
                    self.phase = Phase::Done;
                }
                // ConnectionClosed, AlreadyClosed and every other error end the connection.
                Err(_) => return,
            }
            match self.ws.flush() {
                Ok(()) => {
                    // Everything written so far has reached the kernel.
                    self.queued_bytes
                        .fetch_sub(self.unflushed, Ordering::SeqCst);
                    self.unflushed = 0;
                }
                Err(WsError::Io(error)) if is_would_block(&error) => {}
                Err(_) => return,
            }
        }
    }

    /// Writes queued frames; `false` ends the connection.
    fn drain_queue(&mut self) -> bool {
        loop {
            if self.terminate.load(Ordering::SeqCst) {
                self.ws.get_mut().shutdown();
                return false;
            }
            match self.queue.try_recv() {
                Ok(Outbound::Text(text)) => {
                    let bytes = text.len();
                    if self.closing_deadline.is_some() {
                        self.queued_bytes.fetch_sub(bytes, Ordering::SeqCst);
                        continue;
                    }
                    self.unflushed += bytes;
                    match self.ws.write(Message::text(text)) {
                        Ok(()) => {}
                        // The peer is not taking data. The frame is buffered; stop
                        // here so reads, deadlines and the terminate flag get their
                        // turn, and write the rest on a later pass.
                        Err(WsError::Io(error)) if is_would_block(&error) => return true,
                        Err(WsError::WriteBufferFull(_)) => {
                            self.logger().warn(
                                &self.identity.fields(),
                                "Closing physical WebSocket at outbound high-water mark",
                            );
                            self.ws.get_mut().shutdown();
                            return false;
                        }
                        Err(_) => return false,
                    }
                }
                Ok(Outbound::Close { code, reason }) => {
                    if self.closing_deadline.is_some() {
                        continue;
                    }
                    let frame = code.map(|code| CloseFrame {
                        code: CloseCode::from(code),
                        reason: reason.into(),
                    });
                    let _ = self.ws.close(frame);
                    self.closing_deadline =
                        Some(Instant::now() + self.shared.config.timeouts.close);
                }
                Ok(Outbound::Terminate) => {
                    self.ws.get_mut().shutdown();
                    return false;
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn check_deadlines(&mut self) {
        let now = Instant::now();
        if let Phase::Pending(pending) = &self.phase
            && now >= pending.deadline
        {
            let timeout_ms = self.shared.config.timeouts.hello.as_millis().to_string();
            let mut fields = self.identity.fields();
            fields.push(("timeoutMs", &timeout_ms));
            self.logger()
                .warn(&fields, "Closing connection due to missing hello");
            self.phase = Phase::Done;
            self.close(WS_CLOSE_HELLO_TIMEOUT, "Hello timeout");
        }
        if self.lease_deadline.is_some_and(|deadline| now >= deadline) {
            self.lease_deadline = None;
            self.logger().warn(
                &self.identity.fields(),
                "Closing physical WebSocket with expired application lease",
            );
            self.ws.get_mut().shutdown();
            self.closing_deadline = Some(now);
        }
    }

    /// `handleRawMessage`.
    fn on_data(&mut self, text: &str) {
        if self.shared.lifecycle() != ConnectionLifecycle::Accepting {
            return;
        }
        if self.lease_deadline.is_some() {
            self.lease_deadline =
                Some(Instant::now() + self.shared.config.timeouts.application_lease);
        }
        // `JSON.parse(buffer.toString())` throws a V8 `SyntaxError`; the wire text
        // is "Invalid message: " + err.message. The message comes from the
        // contracts parser, never from a Display of the error, which adds text the
        // baseline does not have.
        let parsed = match parse_js(text) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.on_raw_error(text_of(&error.message));
                return;
            }
        };
        let inbound = match self.classify(&parsed) {
            Ok(inbound) => inbound,
            Err(message) => {
                self.on_invalid(&parsed, &message);
                return;
            }
        };
        let phase = match &self.phase {
            Phase::Pending(_) => PhaseKind::Pending,
            Phase::Active(connection) => PhaseKind::Active(Arc::clone(connection)),
            Phase::Done => PhaseKind::Done,
        };
        match (phase, inbound) {
            (PhaseKind::Active(_), Inbound::Control(control))
                if matches!(*control, WsControlInbound::Ping) =>
            {
                self.lease_deadline =
                    Some(Instant::now() + self.shared.config.timeouts.application_lease);
                self.shared.send_json(
                    self.id,
                    &serde_json::to_value(WsControlOutbound::Pong).unwrap_or(Value::Null),
                );
            }
            (PhaseKind::Pending, Inbound::Control(control)) => match *control {
                WsControlInbound::Hello(hello) => self.on_hello(&hello),
                other => self.reject_pending(type_name(&other)),
            },
            (PhaseKind::Pending, Inbound::Session(_)) => self.reject_pending("session"),
            (PhaseKind::Active(_), Inbound::Control(control)) => match *control {
                WsControlInbound::RecordingState { .. } | WsControlInbound::Ping => {}
                WsControlInbound::Hello(_) => {
                    self.logger().warn(
                        &self.identity.fields(),
                        "Received hello on active connection",
                    );
                    self.close(WS_CLOSE_INVALID_HELLO, "Unexpected hello");
                }
            },
            (PhaseKind::Active(connection), Inbound::Session(message)) => {
                connection.session.handle_message(message, self.id);
            }
            (PhaseKind::Done, _) => {
                self.logger()
                    .error(&[], "No connection found for websocket");
            }
        }
    }

    /// The inbound schema: control frames by `type`, session frames through
    /// the backend.
    ///
    /// The error text is what `WSInboundMessageSchema.safeParse(..).error.message`
    /// holds (zod's issue list) for a frame that is not an object, has no known
    /// `type`, or is a session frame without an object `message`. A control
    /// frame that has the right `type` but a bad field (a `hello` missing
    /// `clientId`, say) still reports the serde text from spocky-contracts, not
    /// zod's issue list: mapping every field issue is left to the contracts
    /// crate, which owns those schemas.
    fn classify(&self, parsed: &JsValue) -> Result<Inbound, String> {
        let Some(record) = parsed.as_object() else {
            return Err(zod_invalid_type(&[], parsed));
        };
        match record.get("type").and_then(JsValue::as_str) {
            Some("session") => {
                let Some(message) = record.get("message").filter(|m| m.is_object()) else {
                    return Err(zod_invalid_type(
                        &["message"],
                        record.get("message").unwrap_or(&JsValue::Undefined),
                    ));
                };
                let message = session_value(message, 0).ok_or("Invalid input")?;
                self.shared
                    .deps
                    .backend
                    .validate_inbound(&message)
                    .map(|()| Inbound::Session(message))
            }
            Some("ping" | "hello" | "recording_state") => {
                WsControlInbound::deserialize(JsValueDeserializer(parsed))
                    .map(|control| Inbound::Control(Box::new(control)))
                    .map_err(|error| error.to_string())
            }
            _ => Err(ZOD_NO_DISCRIMINATOR.to_owned()),
        }
    }

    /// `handleInvalidInboundMessage`.
    fn on_invalid(&mut self, parsed: &JsValue, message: &str) {
        if matches!(self.phase, Phase::Pending(_)) {
            self.logger().warn(
                &[("error", message)],
                "Rejected pending message before hello",
            );
            self.phase = Phase::Done;
            self.close(WS_CLOSE_INVALID_HELLO, "Invalid hello");
            return;
        }
        let Phase::Active(connection) = &self.phase else {
            return;
        };
        let request_info = extract_request_info(parsed);
        let unknown_schema = request_info.is_some()
            && parsed.get("type").and_then(JsValue::as_str) == Some("session");
        let version = &self.shared.config.daemon_version;
        let failure = ProtocolFailure {
            request_id: request_info.as_ref().map(|(id, _)| id.clone()),
            request_type: request_info.and_then(|(_, kind)| kind),
            error: if unknown_schema {
                format!("Unknown request, try upgrading the daemon (currently v{version})")
            } else {
                format!("Invalid message: {message}")
            },
            code: if unknown_schema {
                "unknown_schema"
            } else {
                "invalid_message"
            },
        };
        connection.session.protocol_failure(self.id, failure);
    }

    /// `handleRawMessageError`.
    fn on_raw_error(&mut self, message: &str) {
        self.logger().error(
            &[("errorName", "SyntaxError")],
            "Failed to parse/handle message",
        );
        if matches!(self.phase, Phase::Pending(_)) {
            self.phase = Phase::Done;
            self.close(WS_CLOSE_INVALID_HELLO, "Invalid hello");
            return;
        }
        if let Phase::Active(connection) = &self.phase {
            connection.session.protocol_failure(
                self.id,
                ProtocolFailure {
                    request_id: None,
                    request_type: None,
                    error: format!("Invalid message: {message}"),
                    code: "invalid_message",
                },
            );
        }
    }

    /// `handlePendingConnectionMessage` for anything but a hello.
    fn reject_pending(&mut self, message_type: &str) {
        self.logger().warn(
            &[("messageType", message_type)],
            "Rejected pending message before hello",
        );
        self.phase = Phase::Done;
        self.close(WS_CLOSE_INVALID_HELLO, "Session message before hello");
    }

    /// `rejectHello`.
    fn reject_hello(&self, hello: &Hello, reason: HelloRejectedReason) {
        let wants_frame = hello.auth.is_some()
            || hello
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.is_enabled("hello_rejection"));
        if wants_frame {
            let frame = WsControlOutbound::HelloRejected(HelloRejected::new(reason));
            self.shared
                .send_json(self.id, &serde_json::to_value(frame).unwrap_or(Value::Null));
        }
        let (code, text) = match reason {
            HelloRejectedReason::PasswordRequired => {
                (WS_CLOSE_DAEMON_AUTH_FAILED, "Password required")
            }
            HelloRejectedReason::IncorrectPassword => {
                (WS_CLOSE_DAEMON_AUTH_FAILED, "Incorrect password")
            }
            HelloRejectedReason::IncompatibleProtocol => (
                WS_CLOSE_INCOMPATIBLE_PROTOCOL,
                "Incompatible protocol version",
            ),
        };
        self.close(code, text);
    }

    /// `handleHello`.
    fn on_hello(&mut self, hello: &Hello) {
        let Phase::Pending(pending) = std::mem::replace(&mut self.phase, Phase::Done) else {
            return;
        };
        if hello.protocol_version.get() < 1 {
            self.logger().warn(
                &[(
                    "receivedProtocolVersion",
                    &hello.protocol_version.get().to_string(),
                )],
                "Rejected hello due to protocol version mismatch",
            );
            self.reject_hello(hello, HelloRejectedReason::IncompatibleProtocol);
            return;
        }
        let admission = if let Some(admission) = pending.admission {
            admission
        } else {
            let local_credential = (self.shared.deps.local_credential)();
            match resolve_session_admission(
                hello.auth.as_ref(),
                self.shared.config.password_hash.as_deref(),
                local_credential.as_deref(),
                AdmissionTransport::Direct,
                self.shared.deps.verifier.as_ref(),
            ) {
                Ok(admission) => admission,
                Err(failure) => {
                    let reason = match failure {
                        AdmissionFailure::PasswordRequired => HelloRejectedReason::PasswordRequired,
                        AdmissionFailure::IncorrectPassword => {
                            HelloRejectedReason::IncorrectPassword
                        }
                    };
                    self.reject_hello(hello, reason);
                    return;
                }
            }
        };
        let client_id = js::trim(hello.client_id.as_str()).to_owned();
        if client_id.is_empty() {
            self.logger()
                .warn(&[], "Rejected hello with empty clientId");
            self.close(WS_CLOSE_INVALID_HELLO, "Invalid hello");
            return;
        }
        if client_id.starts_with("plugin:") {
            self.logger().warn(
                &[("clientId", &client_id)],
                "Rejected reserved plugin clientId",
            );
            self.close(WS_CLOSE_INVALID_HELLO, "Invalid plugin clientId");
            return;
        }
        let session_key =
            serde_json::to_string(&[admission.principal_id.as_str(), client_id.as_str()])
                .unwrap_or_default();
        let capabilities = hello
            .capabilities
            .as_ref()
            .and_then(|capabilities| serde_json::to_value(capabilities).ok());
        let key_lock = self.shared.key_lock(&session_key);
        let _serialized = lock(&key_lock);
        let existing = lock(&self.shared.registry)
            .by_key
            .get(&session_key)
            .cloned();
        let connection = match existing {
            Some(existing) => {
                self.resume(&existing, hello, capabilities.as_ref());
                existing
            }
            None => self.create(
                &admission,
                &client_id,
                session_key,
                hello,
                capabilities.as_ref(),
            ),
        };
        self.phase = Phase::Active(Arc::clone(&connection));
        let message =
            server_info_message(&self.shared.server_info_inputs(connection.session.as_ref()));
        self.shared
            .send_json(self.id, &wrap_session_message(&message));
    }

    /// `createSessionConnection` for a first hello.
    fn create(
        &self,
        admission: &SessionAdmission,
        client_id: &str,
        session_key: String,
        hello: &Hello,
        capabilities: Option<&Value>,
    ) -> Arc<SessionConnection> {
        let app_version = hello
            .app_version
            .as_ref()
            .map(|version| version.as_str().to_owned());
        let shared = Arc::downgrade(&self.shared);
        let connection = Arc::new_cyclic(|weak| {
            let session = self.shared.deps.backend.open(SessionOpen {
                client_id: client_id.to_owned(),
                app_version: app_version.clone(),
                client_capabilities: capabilities.cloned(),
                permissions: admission.permissions.clone(),
                sink: Arc::new(ConnectionSink {
                    shared,
                    connection: weak.clone(),
                }),
            });
            SessionConnection {
                session,
                session_key: session_key.clone(),
                client_id: client_id.to_owned(),
                state: Mutex::new(ConnectionState {
                    app_version: app_version.clone(),
                    client_capabilities: capabilities.cloned(),
                    sockets: vec![self.id],
                    cleanup_at: None,
                }),
                cleaned: AtomicBool::new(false),
            }
        });
        connection.session.update_client_capabilities(
            capabilities,
            self.id,
            app_version.as_deref(),
        );
        {
            let mut registry = lock(&self.shared.registry);
            registry.attached.insert(self.id, Arc::clone(&connection));
            registry.by_key.insert(session_key, Arc::clone(&connection));
        }
        let mut fields = self.identity.fields();
        fields.push(("clientId", client_id));
        fields.push(("resumed", "false"));
        self.logger().info(&fields, "Client connected via hello");
        connection
    }

    /// `resumeSession`: a client that reconnects inside the grace period.
    fn resume(
        &self,
        existing: &Arc<SessionConnection>,
        hello: &Hello,
        capabilities: Option<&Value>,
    ) {
        let new_app_version = hello
            .app_version
            .as_ref()
            .map(|version| version.as_str().to_owned());
        // Decide under the state lock, call the backend after releasing it: a
        // backend may send through the sink, which takes the same lock.
        let changed_version = {
            let mut state = lock(&existing.state);
            state.cleanup_at = None;
            match &new_app_version {
                Some(version) if state.app_version.as_ref() != Some(version) => {
                    state.app_version = Some(version.clone());
                    Some(version.clone())
                }
                _ => None,
            }
        };
        if let Some(version) = changed_version {
            existing.session.update_app_version(&version);
        }
        existing.session.update_client_capabilities(
            capabilities,
            self.id,
            new_app_version.as_deref(),
        );
        {
            let mut state = lock(&existing.state);
            if state.client_capabilities.as_ref() != capabilities {
                state.client_capabilities = capabilities.cloned();
            }
            state.sockets.push(self.id);
        }
        lock(&self.shared.registry)
            .attached
            .insert(self.id, Arc::clone(existing));
        let mut fields = self.identity.fields();
        fields.push(("clientId", &existing.client_id));
        fields.push(("resumed", "true"));
        self.logger().info(&fields, "Client connected via hello");
    }

    /// `detachSocket`.
    fn detach(&mut self) {
        let code = self.close_details.0.map(|code| code.to_string());
        let reason = self
            .close_details
            .1
            .clone()
            .filter(|reason| !reason.is_empty());
        let mut details: Vec<(&str, &str)> = Vec::new();
        if let Some(code) = &code {
            details.push(("code", code));
        }
        if let Some(reason) = &reason {
            details.push(("reason", reason));
        }
        let mut registry = lock(&self.shared.registry);
        registry.sockets.remove(&self.id);
        let attached = registry.attached.remove(&self.id);
        drop(registry);
        match (&self.phase, attached) {
            (Phase::Pending(_), _) => {
                let mut fields = self.identity.fields();
                fields.extend(details);
                self.logger().info(&fields, "Pending client disconnected");
            }
            (_, None) => {
                let mut fields = self.identity.fields();
                fields.extend(details);
                self.logger()
                    .info(&fields, "Client socket closed without active session");
            }
            (_, Some(connection)) => {
                connection.session.socket_detached(self.id);
                let remaining = {
                    let mut state = lock(&connection.state);
                    state.sockets.retain(|socket| *socket != self.id);
                    if state.sockets.is_empty() {
                        state.cleanup_at =
                            Some(Instant::now() + self.shared.config.timeouts.reconnect_grace);
                    }
                    state.sockets.len()
                };
                let mut fields = self.identity.fields();
                fields.extend(details);
                if remaining == 0 {
                    let grace = self
                        .shared
                        .config
                        .timeouts
                        .reconnect_grace
                        .as_millis()
                        .to_string();
                    fields.push(("reconnectGraceMs", &grace));
                    self.logger()
                        .info(&fields, "Client disconnected; waiting for reconnect");
                } else {
                    let remaining = remaining.to_string();
                    fields.push(("remainingSockets", &remaining));
                    self.logger().info(
                        &fields,
                        "Client socket disconnected; session remains attached",
                    );
                }
            }
        }
    }
}

fn type_name(control: &WsControlInbound) -> &'static str {
    match control {
        WsControlInbound::Ping => "ping",
        WsControlInbound::Hello(_) => "hello",
        WsControlInbound::RecordingState { .. } => "recording_state",
    }
}

/// Permissions an owner session reports, re-exported for backends and tests.
#[must_use]
pub fn owner_permissions() -> Vec<DaemonPermission> {
    DaemonPermission::ALL.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accepts one connection from a non-blocking listener, as `serve_tcp` does.
    fn accepted_from_nonblocking_listener() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((accepted, _)) => return (accepted, client),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no connection accepted");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        }
    }

    #[test]
    fn an_accepted_socket_waits_for_the_read_timeout_instead_of_spinning() {
        let (accepted, _client) = accepted_from_nonblocking_listener();
        configure_accepted(&accepted).unwrap();
        let mut reader = &accepted;
        let started = Instant::now();
        let error = reader.read(&mut [0_u8; 1]).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        assert!(
            started.elapsed() >= POLL / 2,
            "the read returned after {:?}; the socket is still non-blocking",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_accepted_unix_socket_is_blocking_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let _client = UnixStream::connect(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let accepted = loop {
            if let Ok((accepted, _)) = listener.accept() {
                break accepted;
            }
            assert!(Instant::now() < deadline, "no connection accepted");
            thread::sleep(Duration::from_millis(5));
        };
        configure_accepted(&accepted).unwrap();
        let mut reader = &accepted;
        let started = Instant::now();
        assert!(reader.read(&mut [0_u8; 1]).is_err());
        assert!(started.elapsed() >= POLL / 2);
    }
}
