//! Real loopback WebSocket runtime for the direct daemon relationship pilot.

#![allow(clippy::missing_panics_doc, clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::{HeaderValue, StatusCode};
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonStatus {
    Active,
    Revoked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonPresence {
    Offline,
    Connected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonRuntimeRecord {
    pub daemon_id: String,
    pub credential_verifier: String,
    pub permissions: Vec<String>,
    pub status: DaemonStatus,
    pub presence: DaemonPresence,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonEnrollment {
    pub daemon_id: String,
    pub credential: String,
    pub permissions: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonRequestError {
    Unavailable,
    Superseded,
    Timeout,
}

pub struct PendingDaemonRequest {
    receiver: mpsc::Receiver<Result<Value, DaemonRequestError>>,
}

impl PendingDaemonRequest {
    pub fn wait(self, timeout: Duration) -> Result<Value, DaemonRequestError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                Err(DaemonRequestError::Timeout)
            }
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct DurableState {
    daemons: BTreeMap<String, DaemonRuntimeRecord>,
}

#[derive(Clone)]
struct ActiveConnection {
    generation: u64,
    ready: bool,
    sender: mpsc::Sender<ConnectionCommand>,
}

enum ConnectionCommand {
    Send(Message),
    Close(u16, &'static str),
}

struct PendingRequest {
    generation: u64,
    sender: mpsc::Sender<Result<Value, DaemonRequestError>>,
}

struct Shared {
    state_path: PathBuf,
    state: Mutex<DurableState>,
    active: Mutex<BTreeMap<String, ActiveConnection>>,
    pending: Mutex<BTreeMap<(String, String), PendingRequest>>,
    connection_workers: Mutex<Vec<JoinHandle<()>>>,
    persistence_failure: Mutex<Option<String>>,
    next_request: AtomicU64,
    running: AtomicBool,
}

pub struct HubDaemonRuntime {
    shared: Arc<Shared>,
    address: SocketAddr,
    listener_worker: Mutex<Option<JoinHandle<()>>>,
}

struct OutboundShared {
    running: AtomicBool,
    ready: AtomicBool,
    connection_attempts: AtomicU64,
    successful_connections: AtomicU64,
}

/// Direct daemon-to-Hub relationship controller for the loopback pilot.
pub struct DaemonOutboundController {
    shared: Arc<OutboundShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl DaemonOutboundController {
    #[must_use]
    pub fn start<I, S, F>(
        address: SocketAddr,
        daemon_id: impl Into<String>,
        credential: impl Into<String>,
        permissions: I,
        handler: F,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        let shared = Arc::new(OutboundShared {
            running: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            connection_attempts: AtomicU64::new(0),
            successful_connections: AtomicU64::new(0),
        });
        let worker_shared = Arc::clone(&shared);
        let daemon_id = daemon_id.into();
        let credential = credential.into();
        let mut permissions: Vec<String> = permissions.into_iter().map(Into::into).collect();
        permissions.sort();
        permissions.dedup();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            outbound_loop(
                worker_shared,
                address,
                daemon_id,
                credential,
                permissions,
                handler,
            );
        });
        Self {
            shared,
            worker: Mutex::new(Some(worker)),
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.shared.ready.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn connection_attempts(&self) -> u64 {
        self.shared.connection_attempts.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn successful_connections(&self) -> u64 {
        self.shared.successful_connections.load(Ordering::Relaxed)
    }

    /// Stops this controller and joins only its worker.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker panics.
    pub fn stop(&self) -> io::Result<()> {
        self.shared.running.store(false, Ordering::SeqCst);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            worker
                .join()
                .map_err(|_| io::Error::other("daemon outbound controller panicked"))?;
        }
        Ok(())
    }
}

impl Drop for DaemonOutboundController {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl HubDaemonRuntime {
    /// Binds a daemon WebSocket listener to a random loopback port.
    ///
    /// # Errors
    ///
    /// Returns an error when durable state cannot be loaded or the listener cannot bind.
    pub fn bind(state_path: impl AsRef<Path>) -> io::Result<Self> {
        let state_path = state_path.as_ref().to_path_buf();
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut state = load_state(&state_path)?;
        for daemon in state.daemons.values_mut() {
            daemon.presence = DaemonPresence::Offline;
        }
        persist_to(&state_path, &state)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            state_path,
            state: Mutex::new(state),
            active: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeMap::new()),
            connection_workers: Mutex::new(Vec::new()),
            persistence_failure: Mutex::new(None),
            next_request: AtomicU64::new(0),
            running: AtomicBool::new(true),
        });
        let listener_shared = Arc::clone(&shared);
        let worker = thread::spawn(move || listen(listener_shared, listener));
        Ok(Self {
            shared,
            address,
            listener_worker: Mutex::new(Some(worker)),
        })
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Persists a daemon credential verifier and relationship contract.
    ///
    /// # Errors
    ///
    /// Returns an error when the state snapshot cannot be written.
    pub fn enroll(&self, enrollment: DaemonEnrollment) -> io::Result<()> {
        let mut permissions = enrollment.permissions;
        permissions.sort();
        permissions.dedup();
        self.shared.state.lock().unwrap().daemons.insert(
            enrollment.daemon_id.clone(),
            DaemonRuntimeRecord {
                daemon_id: enrollment.daemon_id,
                credential_verifier: credential_verifier(&enrollment.credential),
                permissions,
                status: DaemonStatus::Active,
                presence: DaemonPresence::Offline,
                generation: 0,
            },
        );
        persist(&self.shared)
    }

    #[must_use]
    pub fn daemon(&self, daemon_id: &str) -> Option<DaemonRuntimeRecord> {
        self.shared
            .state
            .lock()
            .unwrap()
            .daemons
            .get(daemon_id)
            .cloned()
    }

    #[must_use]
    pub fn snapshot_contains(&self, value: &str) -> bool {
        serde_json::to_string(&*self.shared.state.lock().unwrap())
            .is_ok_and(|snapshot| snapshot.contains(value))
    }

    #[must_use]
    pub fn is_ready(&self, daemon_id: &str) -> bool {
        self.shared
            .active
            .lock()
            .unwrap()
            .get(daemon_id)
            .is_some_and(|active| active.ready)
    }

    pub fn request(
        &self,
        daemon_id: &str,
        payload: Value,
    ) -> Result<PendingDaemonRequest, DaemonRequestError> {
        let request_id = format!(
            "hub-request-{}",
            self.shared.next_request.fetch_add(1, Ordering::Relaxed) + 1
        );
        let active = self.shared.active.lock().unwrap();
        let Some(connection) = active.get(daemon_id) else {
            return Err(DaemonRequestError::Unavailable);
        };
        if !connection.ready {
            return Err(DaemonRequestError::Unavailable);
        }
        let (sender, receiver) = mpsc::channel();
        self.shared.pending.lock().unwrap().insert(
            (daemon_id.to_owned(), request_id.clone()),
            PendingRequest {
                generation: connection.generation,
                sender,
            },
        );
        let message = Message::Text(
            json!({
                "type": "session",
                "message": {
                    "type": "request",
                    "requestId": request_id,
                    "payload": payload
                }
            })
            .to_string()
            .into(),
        );
        if connection
            .sender
            .send(ConnectionCommand::Send(message))
            .is_err()
        {
            self.shared
                .pending
                .lock()
                .unwrap()
                .remove(&(daemon_id.to_owned(), request_id));
            return Err(DaemonRequestError::Unavailable);
        }
        Ok(PendingDaemonRequest { receiver })
    }

    /// Revokes a relationship and closes its current socket with code 4403.
    ///
    /// # Errors
    ///
    /// Returns an error when the daemon is unknown or state cannot be persisted.
    pub fn revoke(&self, daemon_id: &str) -> io::Result<()> {
        let generation = {
            let mut state = self.shared.state.lock().unwrap();
            let daemon = state
                .daemons
                .get_mut(daemon_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "daemon unavailable"))?;
            daemon.status = DaemonStatus::Revoked;
            daemon.presence = DaemonPresence::Offline;
            daemon.generation
        };
        persist(&self.shared)?;
        reject_generation(&self.shared, daemon_id, generation);
        if let Some(active) = self.shared.active.lock().unwrap().remove(daemon_id) {
            let _ = active
                .sender
                .send(ConnectionCommand::Close(4403, "revoked"));
        }
        Ok(())
    }

    /// Stops only this loopback runtime and waits for its socket workers.
    ///
    /// # Errors
    ///
    /// Returns an error if a runtime thread panics.
    pub fn stop(&self) -> io::Result<()> {
        if !self.shared.running.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        for active in self.shared.active.lock().unwrap().values() {
            let _ = active
                .sender
                .send(ConnectionCommand::Close(1001, "server shutdown"));
        }
        if let Some(worker) = self.listener_worker.lock().unwrap().take() {
            worker
                .join()
                .map_err(|_| io::Error::other("daemon listener panicked"))?;
        }
        for worker in self.shared.connection_workers.lock().unwrap().drain(..) {
            worker
                .join()
                .map_err(|_| io::Error::other("daemon connection panicked"))?;
        }
        if let Some(error) = self.shared.persistence_failure.lock().unwrap().take() {
            return Err(io::Error::other(error));
        }
        Ok(())
    }
}

impl Drop for HubDaemonRuntime {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn outbound_loop<F>(
    shared: Arc<OutboundShared>,
    address: SocketAddr,
    daemon_id: String,
    credential: String,
    permissions: Vec<String>,
    handler: Arc<F>,
) where
    F: Fn(Value) -> Value + Send + Sync + 'static,
{
    while shared.running.load(Ordering::Relaxed) {
        shared.connection_attempts.fetch_add(1, Ordering::Relaxed);
        if let Some(mut socket) = connect_outbound(address, &daemon_id, &credential) {
            shared
                .successful_connections
                .fetch_add(1, Ordering::Relaxed);
            run_outbound_session(&shared, &mut socket, &permissions, handler.as_ref());
        }
        shared.ready.store(false, Ordering::Relaxed);
        for _ in 0..5 {
            if !shared.running.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

fn connect_outbound(
    address: SocketAddr,
    daemon_id: &str,
    credential: &str,
) -> Option<tungstenite::WebSocket<TcpStream>> {
    let stream = TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    let mut request = format!("ws://{address}/api/daemons/socket")
        .into_client_request()
        .ok()?;
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {credential}")).ok()?,
    );
    request
        .headers_mut()
        .insert("x-paseo-daemon-id", HeaderValue::from_str(daemon_id).ok()?);
    request
        .headers_mut()
        .insert("x-paseo-session-protocol", HeaderValue::from_static("1"));
    let Ok((mut socket, _)) = tungstenite::client(request, stream) else {
        return None;
    };
    socket
        .get_mut()
        .set_read_timeout(Some(SOCKET_TIMEOUT))
        .ok()?;
    socket
        .get_mut()
        .set_write_timeout(Some(SOCKET_TIMEOUT))
        .ok()?;
    Some(socket)
}

fn run_outbound_session<F>(
    shared: &OutboundShared,
    socket: &mut tungstenite::WebSocket<TcpStream>,
    permissions: &[String],
    handler: &F,
) where
    F: Fn(Value) -> Value,
{
    let handshake_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        if !shared.running.load(Ordering::Relaxed) || Instant::now() >= handshake_deadline {
            return;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(message) = serde_json::from_str::<Value>(&text) else {
                    return;
                };
                if message.get("type").and_then(Value::as_str) == Some("hello") {
                    break;
                }
            }
            Ok(Message::Ping(bytes)) => {
                if socket.send(Message::Pong(bytes)).is_err() {
                    return;
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(_) => {}
        }
    }
    let server_info = json!({
        "type": "session",
        "message": {
            "type": "status",
            "payload": {
                "status": "server_info",
                "permissions": permissions
            }
        }
    });
    if socket
        .send(Message::Text(server_info.to_string().into()))
        .is_err()
    {
        return;
    }
    shared.ready.store(true, Ordering::Relaxed);

    while shared.running.load(Ordering::Relaxed) {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let Some(message) = value.get("message") else {
                    continue;
                };
                if value.get("type").and_then(Value::as_str) != Some("session")
                    || message.get("type").and_then(Value::as_str) != Some("request")
                {
                    continue;
                }
                let Some(request_id) = message.get("requestId").and_then(Value::as_str) else {
                    continue;
                };
                let result = handler(message.get("payload").cloned().unwrap_or(Value::Null));
                let response = json!({
                    "type": "session",
                    "message": {
                        "type": "response",
                        "requestId": request_id,
                        "result": result
                    }
                });
                if socket
                    .send(Message::Text(response.to_string().into()))
                    .is_err()
                {
                    return;
                }
            }
            Ok(Message::Ping(bytes)) => {
                if socket.send(Message::Pong(bytes)).is_err() {
                    return;
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(_) => {}
        }
    }
    let _ = socket.close(Some(CloseFrame {
        code: CloseCode::Away,
        reason: "daemon shutdown".into(),
    }));
}

#[must_use]
pub fn credential_verifier(credential: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(credential.as_bytes()))
}

fn listen(shared: Arc<Shared>, listener: TcpListener) {
    while shared.running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let connection_shared = Arc::clone(&shared);
                let worker = thread::spawn(move || serve_connection(connection_shared, stream));
                shared.connection_workers.lock().unwrap().push(worker);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => break,
        }
    }
}

#[derive(Clone, Copy)]
enum SessionProtocol {
    Legacy,
    Standard,
}

struct AcceptedDaemon {
    daemon_id: String,
    protocol: SessionProtocol,
}

#[allow(clippy::result_large_err)]
fn serve_connection(shared: Arc<Shared>, stream: TcpStream) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let accepted = Arc::new(Mutex::new(None));
    let callback_accepted = Arc::clone(&accepted);
    let callback_shared = Arc::clone(&shared);
    let upgraded = tungstenite::accept_hdr(stream, move |request: &Request, response: Response| {
        authorize_upgrade(&callback_shared, request, response, &callback_accepted)
    });
    let Ok(mut socket) = upgraded else {
        return;
    };
    let _ = socket.get_mut().set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = socket.get_mut().set_write_timeout(Some(SOCKET_TIMEOUT));
    let Some(accepted) = accepted.lock().unwrap().take() else {
        return;
    };
    let (sender, receiver) = mpsc::channel();
    let Some(generation) = begin_connection(&shared, &accepted.daemon_id, sender) else {
        let _ = socket.close(Some(CloseFrame {
            code: CloseCode::Error,
            reason: "daemon state persistence failed".into(),
        }));
        return;
    };
    match accepted.protocol {
        SessionProtocol::Legacy => {
            if !mark_ready(&shared, &accepted.daemon_id, generation) {
                let _ = socket.close(Some(CloseFrame {
                    code: CloseCode::Error,
                    reason: "daemon presence persistence failed".into(),
                }));
                finish_connection(&shared, &accepted.daemon_id, generation);
                return;
            }
        }
        SessionProtocol::Standard => {
            let hello = json!({
                "type": "hello",
                "clientId": format!("hub:{}", accepted.daemon_id),
                "clientType": "hub",
                "capabilities": {
                    "all_providers": true,
                    "selective_agent_timeline": true
                },
                "protocolVersion": 1
            });
            if socket
                .send(Message::Text(hello.to_string().into()))
                .is_err()
            {
                finish_connection(&shared, &accepted.daemon_id, generation);
                return;
            }
        }
    }

    while shared.running.load(Ordering::Relaxed) {
        match receiver.try_recv() {
            Ok(ConnectionCommand::Send(message)) => {
                if socket.send(message).is_err() {
                    break;
                }
            }
            Ok(ConnectionCommand::Close(code, reason)) => {
                let _ = socket.close(Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: reason.into(),
                }));
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if !receive_message(&shared, &accepted.daemon_id, generation, &text, &mut socket) {
                    break;
                }
            }
            Ok(Message::Ping(bytes)) => {
                if socket.send(Message::Pong(bytes)).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    finish_connection(&shared, &accepted.daemon_id, generation);
}

#[allow(clippy::result_large_err)]
fn authorize_upgrade(
    shared: &Shared,
    request: &Request,
    mut response: Response,
    accepted: &Mutex<Option<AcceptedDaemon>>,
) -> Result<Response, ErrorResponse> {
    if request.uri().path() != "/api/daemons/socket" {
        return Err(rejection(StatusCode::NOT_FOUND));
    }
    let Some(daemon_id) = request
        .headers()
        .get("x-paseo-daemon-id")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(rejection(StatusCode::UNAUTHORIZED));
    };
    let Some(credential) = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Err(rejection(StatusCode::UNAUTHORIZED));
    };
    let state = shared.state.lock().unwrap();
    let Some(daemon) = state.daemons.get(daemon_id) else {
        return Err(rejection(StatusCode::FORBIDDEN));
    };
    if daemon.status != DaemonStatus::Active
        || !constant_time_eq(
            credential_verifier(credential).as_bytes(),
            daemon.credential_verifier.as_bytes(),
        )
    {
        return Err(rejection(StatusCode::FORBIDDEN));
    }
    let protocol = if request
        .headers()
        .get("x-paseo-session-protocol")
        .is_some_and(|value| value == "1")
    {
        response
            .headers_mut()
            .insert("x-paseo-session-protocol", HeaderValue::from_static("1"));
        SessionProtocol::Standard
    } else {
        SessionProtocol::Legacy
    };
    *accepted.lock().unwrap() = Some(AcceptedDaemon {
        daemon_id: daemon_id.to_owned(),
        protocol,
    });
    Ok(response)
}

fn rejection(status: StatusCode) -> ErrorResponse {
    Response::builder()
        .status(status)
        .body(Some("rejected".to_owned()))
        .unwrap()
}

fn begin_connection(
    shared: &Shared,
    daemon_id: &str,
    sender: mpsc::Sender<ConnectionCommand>,
) -> Option<u64> {
    let generation = {
        let mut state = shared.state.lock().unwrap();
        let daemon = state.daemons.get_mut(daemon_id).unwrap();
        daemon.generation += 1;
        daemon.generation
    };
    let previous = shared.active.lock().unwrap().insert(
        daemon_id.to_owned(),
        ActiveConnection {
            generation,
            ready: false,
            sender,
        },
    );
    if let Some(previous) = previous {
        reject_generation(shared, daemon_id, previous.generation);
        let _ = previous
            .sender
            .send(ConnectionCommand::Close(4001, "replaced"));
    }
    if let Err(error) = persist(shared) {
        record_persistence_failure(shared, error);
        shared.active.lock().unwrap().remove(daemon_id);
        return None;
    }
    Some(generation)
}

fn mark_ready(shared: &Shared, daemon_id: &str, generation: u64) -> bool {
    let current = {
        let mut active = shared.active.lock().unwrap();
        let Some(connection) = active.get_mut(daemon_id) else {
            return false;
        };
        if connection.generation != generation {
            return false;
        }
        connection.ready = true;
        true
    };
    if current {
        if let Some(daemon) = shared.state.lock().unwrap().daemons.get_mut(daemon_id) {
            daemon.presence = DaemonPresence::Connected;
        }
        if let Err(error) = persist(shared) {
            record_persistence_failure(shared, error);
            return false;
        }
    }
    true
}

fn receive_message(
    shared: &Shared,
    daemon_id: &str,
    generation: u64,
    raw: &str,
    socket: &mut tungstenite::WebSocket<TcpStream>,
) -> bool {
    if shared
        .active
        .lock()
        .unwrap()
        .get(daemon_id)
        .is_none_or(|active| active.generation != generation)
    {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        let _ = socket.close(Some(CloseFrame {
            code: CloseCode::from(4400),
            reason: "invalid daemon message".into(),
        }));
        return false;
    };
    if server_info_permissions(&value).is_some() {
        let expected = shared
            .state
            .lock()
            .unwrap()
            .daemons
            .get(daemon_id)
            .map(|daemon| daemon.permissions.clone())
            .unwrap_or_default();
        let mut actual = server_info_permissions(&value).unwrap();
        actual.sort();
        actual.dedup();
        if actual != expected {
            let _ = socket.close(Some(CloseFrame {
                code: CloseCode::from(4403),
                reason: "daemon session permissions do not match enrollment".into(),
            }));
            return false;
        }
        if !mark_ready(shared, daemon_id, generation) {
            let _ = socket.close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: "daemon presence persistence failed".into(),
            }));
            return false;
        }
        return true;
    }
    let Some(message) = value.get("message") else {
        return true;
    };
    if message.get("type").and_then(Value::as_str) != Some("response") {
        return true;
    }
    let Some(request_id) = message.get("requestId").and_then(Value::as_str) else {
        return true;
    };
    let key = (daemon_id.to_owned(), request_id.to_owned());
    let mut pending = shared.pending.lock().unwrap();
    if pending
        .get(&key)
        .is_none_or(|request| request.generation != generation)
    {
        return true;
    }
    if let Some(request) = pending.remove(&key) {
        let _ = request
            .sender
            .send(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
    }
    true
}

fn server_info_permissions(value: &Value) -> Option<Vec<String>> {
    let payload = value.get("message")?.get("payload")?;
    if value.get("type")?.as_str()? != "session"
        || value.get("message")?.get("type")?.as_str()? != "status"
        || payload.get("status")?.as_str()? != "server_info"
    {
        return None;
    }
    payload
        .get("permissions")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(ToOwned::to_owned))
        .collect()
}

fn finish_connection(shared: &Shared, daemon_id: &str, generation: u64) {
    let removed = {
        let mut active = shared.active.lock().unwrap();
        if active
            .get(daemon_id)
            .is_some_and(|connection| connection.generation == generation)
        {
            active.remove(daemon_id);
            true
        } else {
            false
        }
    };
    reject_generation(shared, daemon_id, generation);
    if removed {
        if let Some(daemon) = shared.state.lock().unwrap().daemons.get_mut(daemon_id) {
            daemon.presence = DaemonPresence::Offline;
        }
        if let Err(error) = persist(shared) {
            record_persistence_failure(shared, error);
        }
    }
}

fn reject_generation(shared: &Shared, daemon_id: &str, generation: u64) {
    let mut pending = shared.pending.lock().unwrap();
    let keys: Vec<_> = pending
        .iter()
        .filter(|((pending_daemon, _), request)| {
            pending_daemon == daemon_id && request.generation == generation
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        if let Some(request) = pending.remove(&key) {
            let _ = request.sender.send(Err(DaemonRequestError::Superseded));
        }
    }
}

fn load_state(path: &Path) -> io::Result<DurableState> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DurableState::default()),
        Err(error) => Err(error),
    }
}

fn persist(shared: &Shared) -> io::Result<()> {
    persist_to(&shared.state_path, &shared.state.lock().unwrap())
}

fn record_persistence_failure(shared: &Shared, error: io::Error) {
    let mut failure = shared.persistence_failure.lock().unwrap();
    if failure.is_none() {
        *failure = Some(error.to_string());
    }
}

fn persist_to(path: &Path, state: &DurableState) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn constant_time_eq(actual: &[u8], expected: &[u8]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
