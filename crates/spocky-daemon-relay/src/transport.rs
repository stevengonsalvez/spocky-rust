//! `relay-transport.ts`: the daemon's relay client. One control socket per daemon, one
//! data socket per connected client, reconnect with a capped linear backoff, keepalive
//! and staleness detection on the control socket, and the optional end-to-end handshake
//! before a data socket is handed to the application.
//!
//! The state machine never touches a socket or a clock. Every effect goes through
//! [`RelayIo`] synchronously, in the order the original performs it, and every input
//! arrives as a method call, so a real runtime and a recording harness drive the same
//! code.

use crate::control::{
    ControlMessage, MessageData, normalize_message_data, try_parse_control_message,
};
use crate::encrypted_socket::{EncryptedRelayEnv, EncryptedRelaySocket, EnvFailure, SendOutcome};
use crate::endpoint::{
    EndpointError, RelayRole, RelayUrlParams, VersionInput, build_relay_websocket_url,
};
use spocky_crypto::channel::{AppSend, Data};
use spocky_crypto::js_string::{JsString, utf16};

pub const CONTROL_PING_INTERVAL_MS: u64 = 10_000;
pub const CONTROL_STALE_TIMEOUT_MS: i64 = 30_000;
pub const CONTROL_READY_TIMEOUT_MS: u64 = 8_000;
pub const DATA_OPEN_TIMEOUT_MS: u64 = 15_000;
pub const MAX_RECONNECT_DELAY_MS: u64 = 30_000;
pub const RECONNECT_STEP_MS: u64 = 1_000;

/// `WebSocket.OPEN`.
pub const WEBSOCKET_OPEN: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SocketId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimerId(pub u64);

/// A synchronous failure of a socket call (the original catches a throw).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IoFailure(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
}

/// The pino child bindings of a record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogContext {
    /// `{ module: "relay-transport" }`.
    Transport,
    /// `{ module: "relay-transport", connectionId }` for end-to-end attach records.
    Attach(JsString),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldValue {
    Number(i64),
    Text(String),
    Js(JsString),
    /// An `Error`; pino's `err` field.
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    pub level: LogLevel,
    pub message: &'static str,
    pub context: LogContext,
    pub fields: Vec<(&'static str, FieldValue)>,
}

/// `ExternalSocketMetadata` for a relay data socket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachMetadata {
    /// `session:<connectionId>`.
    pub external_session_key: JsString,
    pub relay_connection_id: JsString,
}

/// How `socket.send(data, callback)` started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SendStart {
    /// `send` threw.
    Threw(IoFailure),
    /// The callback ran at once, with an error or without.
    Callback(Option<String>),
    /// The callback runs later; see [`RelayTransport::on_adapter_send_callback`].
    Pending,
}

/// What the adapter's `send` returned to the channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterSend {
    /// The promise settled: resolved, or rejected with the message.
    Settled(Result<(), String>),
    /// The promise settles when the callback runs.
    Pending,
}

/// The daemon process ends on these: `daemon-worker.ts` logs `fatal` and exits for an
/// uncaught exception and for an unhandled rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fatal {
    UncaughtException(String),
    UnhandledRejection(String),
}

/// Everything the transport does to the outside world.
pub trait RelayIo {
    /// `createWebSocket(url)`.
    fn create_socket(&mut self, url: &str) -> SocketId;
    /// # Errors
    ///
    /// The socket call threw.
    fn ping(&mut self, socket: SocketId) -> Result<(), IoFailure>;
    /// # Errors
    ///
    /// The socket call threw.
    fn terminate(&mut self, socket: SocketId) -> Result<(), IoFailure>;
    /// # Errors
    ///
    /// The socket call threw.
    fn close(
        &mut self,
        socket: SocketId,
        code: Option<u16>,
        reason: Option<&str>,
    ) -> Result<(), IoFailure>;
    /// # Errors
    ///
    /// The socket call threw.
    fn send_text(&mut self, socket: SocketId, text: &str) -> Result<(), IoFailure>;
    fn ready_state(&self, socket: SocketId) -> u8;
    /// `Date.now()`.
    fn now_ms(&self) -> i64;
    fn set_timeout(&mut self, delay_ms: u64) -> TimerId;
    fn set_interval(&mut self, delay_ms: u64) -> TimerId;
    fn clear_timer(&mut self, timer: TimerId);
    fn log(&mut self, record: LogRecord);
    /// `attachSocket(ws, metadata)` with the raw relay socket.
    fn attach_plain(&mut self, socket: SocketId, metadata: &AttachMetadata);
    /// `createDaemonChannel(adapter, keyPair, events)`; its result arrives through
    /// [`RelayTransport::on_channel_ready`] or [`RelayTransport::on_channel_failed`].
    fn start_daemon_channel(&mut self, socket: SocketId);
    /// `attachSocket(encryptedSocket, metadata)`; its result arrives through
    /// [`RelayTransport::on_attach_settled`].
    fn attach_encrypted(&mut self, socket: SocketId, metadata: &AttachMetadata);
    /// A frame the adapter hands to the channel (`relayTransport.onmessage`).
    fn channel_message(&mut self, socket: SocketId, data: Data, is_binary: bool);
    /// `relayTransport.onclose`.
    fn channel_closed(&mut self, socket: SocketId, code: u16, reason: &str);
    /// `relayTransport.onerror`.
    fn channel_error(&mut self, socket: SocketId, message: &str);
    /// `emitter.emit("message")` on the encrypted socket.
    ///
    /// # Errors
    ///
    /// A listener threw.
    fn emit_message(&mut self, socket: SocketId, data: &Data) -> Result<(), String>;
    /// `emitter.emit("close")`.
    ///
    /// # Errors
    ///
    /// A listener threw.
    fn emit_close(&mut self, socket: SocketId, code: u16, reason: &str) -> Result<(), String>;
    /// `emitter.emit("error")`.
    ///
    /// # Errors
    ///
    /// No `error` listener is registered, or one threw; `EventEmitter` throws the error then.
    fn emit_error(&mut self, socket: SocketId, message: &str) -> Result<(), String>;
    /// `channel.setState("open")` on the socket's channel.
    fn channel_set_state_open(&mut self, socket: SocketId);
    /// `channel.send(data)`.
    fn channel_send(&mut self, socket: SocketId, data: &Data) -> AppSend;
    /// `channel.outboundWireByteLength(data)`.
    fn channel_outbound_wire_byte_length(&self, socket: SocketId, data: &Data) -> u64;
    /// `channel.close(code, reason)`.
    ///
    /// # Errors
    ///
    /// `channel.close` threw.
    fn channel_close(
        &mut self,
        socket: SocketId,
        code: Option<u16>,
        reason: Option<&str>,
    ) -> Result<(), IoFailure>;
    /// `socket.send(data, callback)` for the end-to-end adapter.
    fn send_data(&mut self, socket: SocketId, data: &Data) -> SendStart;
    /// The physical socket's `bufferedAmount`.
    fn transport_buffered_amount(&self, socket: SocketId) -> Option<u64>;
}

/// The channel and physical socket behind one encrypted socket.
struct SocketEnv<'a> {
    io: &'a mut dyn RelayIo,
    socket: SocketId,
}

impl EncryptedRelayEnv for SocketEnv<'_> {
    fn set_state_open(&mut self) {
        self.io.channel_set_state_open(self.socket);
    }

    fn channel_send(&mut self, data: &Data) -> AppSend {
        self.io.channel_send(self.socket, data)
    }

    fn outbound_wire_byte_length(&self, data: &Data) -> u64 {
        self.io.channel_outbound_wire_byte_length(self.socket, data)
    }

    fn channel_close(&mut self, code: Option<u16>, reason: Option<&str>) -> Result<(), EnvFailure> {
        self.io
            .channel_close(self.socket, code, reason)
            .map_err(|failure| EnvFailure(failure.0))
    }

    fn transport_buffered_amount(&self) -> Option<u64> {
        self.io.transport_buffered_amount(self.socket)
    }

    fn terminate_transport(&mut self) -> Result<(), EnvFailure> {
        self.io
            .terminate(self.socket)
            .map_err(|failure| EnvFailure(failure.0))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportOptions {
    pub relay_endpoint: String,
    pub relay_use_tls: bool,
    pub server_id: String,
    pub has_daemon_key_pair: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimerKind {
    Keepalive(SocketId),
    ControlReady(SocketId),
    Reconnect,
    DataOpen(SocketId),
}

struct Control {
    socket: SocketId,
    seq: u64,
    url: String,
    connected: bool,
}

/// The per-data-socket closure of `ensureClientDataSocket`.
struct DataClosure {
    socket: SocketId,
    connection_id: JsString,
    url: String,
    open_timer: TimerId,
    attached: bool,
    /// The socket's `close` event ran. The closure is dropped once the end-to-end attach has
    /// settled too, so a long-running daemon does not keep every client it ever served.
    closed: bool,
    e2ee: Option<E2ee>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum E2eePhase {
    AwaitingChannel,
    AwaitingAttach,
    Attached,
    Failed,
}

/// The state of `attachEncryptedSocket` for one socket.
struct E2ee {
    phase: E2eePhase,
    pending: Vec<Data>,
    socket: Option<EncryptedRelaySocket>,
}

pub struct RelayTransport {
    options: TransportOptions,
    stopped: bool,
    control: Option<Control>,
    reconnect_timer: Option<TimerId>,
    reconnect_attempt: u64,
    /// `dataSockets`: insertion-ordered `connectionId -> socket`.
    data_sockets: Vec<(JsString, SocketId)>,
    keepalive: Option<TimerId>,
    ready_timer: Option<TimerId>,
    control_last_seen_at: i64,
    control_seq: u64,
    timers: Vec<(TimerId, TimerKind)>,
    closures: Vec<DataClosure>,
}

fn warn(io: &mut dyn RelayIo, message: &'static str, fields: Vec<(&'static str, FieldValue)>) {
    io.log(LogRecord {
        level: LogLevel::Warn,
        message,
        context: LogContext::Transport,
        fields,
    });
}

fn number(value: u64) -> FieldValue {
    FieldValue::Number(i64::try_from(value).unwrap_or(i64::MAX))
}

fn reason_text(reason: Option<&[u8]>) -> Option<String> {
    reason.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

impl RelayTransport {
    /// `startRelayTransport`: connects the control socket.
    ///
    /// # Errors
    ///
    /// Fails when the endpoint or URL is invalid, exactly where the original throws.
    pub fn start(io: &mut dyn RelayIo, options: TransportOptions) -> Result<Self, EndpointError> {
        let mut transport = Self {
            options,
            stopped: false,
            control: None,
            reconnect_timer: None,
            reconnect_attempt: 0,
            data_sockets: Vec::new(),
            keepalive: None,
            ready_timer: None,
            control_last_seen_at: 0,
            control_seq: 0,
            timers: Vec::new(),
            closures: Vec::new(),
        };
        transport.connect_control(io)?;
        Ok(transport)
    }

    /// `stop()`.
    pub fn stop(&mut self, io: &mut dyn RelayIo) {
        self.stopped = true;
        if let Some(timer) = self.reconnect_timer.take() {
            self.clear(io, timer);
        }
        if let Some(timer) = self.keepalive.take() {
            self.clear(io, timer);
        }
        if let Some(timer) = self.ready_timer.take() {
            self.clear(io, timer);
        }
        if let Some(control) = self.control.take() {
            let _ = io.close(control.socket, None, None);
        }
        for (_, socket) in std::mem::take(&mut self.data_sockets) {
            let _ = io.close(socket, None, None);
        }
    }

    fn url(&self, connection_id: Option<&JsString>) -> Result<String, EndpointError> {
        let connection_id = connection_id.map(|id| String::from_utf16_lossy(id));
        build_relay_websocket_url(&RelayUrlParams {
            endpoint: &self.options.relay_endpoint,
            use_tls: self.options.relay_use_tls,
            server_id: &self.options.server_id,
            role: RelayRole::Server,
            connection_id: connection_id.as_deref(),
            version: VersionInput::Missing,
        })
    }

    fn is_control(&self, socket: SocketId) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| control.socket == socket)
    }

    fn remember(&mut self, timer: TimerId, kind: TimerKind) {
        self.timers.push((timer, kind));
    }

    /// `clearTimeout` / `clearInterval`: the timer is forgotten here and cancelled in the io.
    fn clear(&mut self, io: &mut dyn RelayIo, timer: TimerId) {
        self.timers.retain(|(candidate, _)| *candidate != timer);
        io.clear_timer(timer);
    }

    fn connect_control(&mut self, io: &mut dyn RelayIo) -> Result<(), EndpointError> {
        if self.stopped {
            return Ok(());
        }
        self.control_seq += 1;
        let seq = self.control_seq;
        let url = self.url(None)?;
        let socket = io.create_socket(&url);
        self.control = Some(Control {
            socket,
            seq,
            url,
            connected: false,
        });
        Ok(())
    }

    fn schedule_reconnect(&mut self, io: &mut dyn RelayIo) {
        if self.stopped || self.reconnect_timer.is_some() {
            return;
        }
        self.reconnect_attempt += 1;
        let delay = MAX_RECONNECT_DELAY_MS.min(RECONNECT_STEP_MS * self.reconnect_attempt);
        let timer = io.set_timeout(delay);
        self.reconnect_timer = Some(timer);
        self.remember(timer, TimerKind::Reconnect);
    }

    /// A timer created through [`RelayIo::set_timeout`] or [`RelayIo::set_interval`] fired.
    pub fn on_timer(&mut self, io: &mut dyn RelayIo, timer: TimerId) {
        let Some(kind) = self
            .timers
            .iter()
            .find(|(candidate, _)| *candidate == timer)
            .map(|(_, kind)| *kind)
        else {
            return;
        };
        match kind {
            TimerKind::Reconnect => {
                self.timers.retain(|(candidate, _)| *candidate != timer);
                self.reconnect_timer = None;
                // The configuration already produced a URL once, so this cannot fail.
                let _ = self.connect_control(io);
            }
            TimerKind::ControlReady(socket) => {
                self.timers.retain(|(candidate, _)| *candidate != timer);
                self.on_control_ready_timeout(io, socket);
            }
            TimerKind::Keepalive(socket) => self.on_keepalive(io, socket),
            TimerKind::DataOpen(socket) => {
                self.timers.retain(|(candidate, _)| *candidate != timer);
                self.on_data_open_timeout(io, socket);
            }
        }
    }

    fn on_control_ready_timeout(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if self.stopped || !self.is_control(socket) {
            return;
        }
        let Some(control) = self.control.as_ref() else {
            return;
        };
        if control.connected {
            return;
        }
        let (url, seq) = (control.url.clone(), control.seq);
        warn(
            io,
            "relay_control_ready_timeout_terminating",
            vec![
                ("url", FieldValue::Text(url)),
                ("connectionId", number(seq)),
                ("waitedMs", number(CONTROL_READY_TIMEOUT_MS)),
            ],
        );
        let _ = io.terminate(socket);
    }

    fn on_keepalive(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if self.stopped || !self.is_control(socket) {
            return;
        }
        if io.ready_state(socket) != WEBSOCKET_OPEN {
            return;
        }
        let Some(control) = self.control.as_ref() else {
            return;
        };
        let (url, seq) = (control.url.clone(), control.seq);
        let stale_for = io.now_ms() - self.control_last_seen_at;
        if stale_for > CONTROL_STALE_TIMEOUT_MS {
            warn(
                io,
                "relay_control_stale_terminating",
                vec![
                    ("url", FieldValue::Text(url)),
                    ("staleForMs", FieldValue::Number(stale_for)),
                    ("connectionId", number(seq)),
                    (
                        "staleTimeoutMs",
                        FieldValue::Number(CONTROL_STALE_TIMEOUT_MS),
                    ),
                ],
            );
            let _ = io.terminate(socket);
            return;
        }
        Self::ping_or_terminate(io, socket, seq);
    }

    fn ping_or_terminate(io: &mut dyn RelayIo, socket: SocketId, seq: u64) {
        if let Err(failure) = io.ping(socket) {
            warn(
                io,
                "relay_control_ping_send_failed",
                vec![
                    ("err", FieldValue::Error(failure.0)),
                    ("connectionId", number(seq)),
                ],
            );
            let _ = io.terminate(socket);
        }
    }

    /// A socket's `open` event. Data sockets are known; any other socket is a control socket.
    pub fn on_open(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if self.closure(socket).is_some() {
            self.on_data_open(io, socket);
        } else {
            self.on_control_open(io, socket);
        }
    }

    /// A socket's `close` event.
    pub fn on_close(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        code: u16,
        reason: Option<&[u8]>,
    ) {
        if self.closure(socket).is_some() {
            self.on_data_close(io, socket, code, reason);
        } else {
            self.on_control_close(io, socket, code, reason);
        }
    }

    /// A socket's `error` event.
    pub fn on_error(&mut self, io: &mut dyn RelayIo, socket: SocketId, message: &str) {
        if self.closure(socket).is_some() {
            self.on_data_error(io, socket, message);
        } else {
            self.on_control_error(io, socket, message);
        }
    }

    /// A socket's `pong` event; only the control socket listens.
    pub fn on_pong(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if self.closure(socket).is_none() {
            self.on_control_pong(io, socket);
        }
    }

    /// A socket's `message` event.
    pub fn on_message(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        data: &MessageData,
        is_binary: bool,
    ) {
        if self.closure(socket).is_some() {
            self.on_data_message(io, socket, data, is_binary);
        } else {
            self.on_control_message(io, socket, data);
        }
    }

    /// The control socket's `open` event.
    pub fn on_control_open(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if !self.is_control(socket) {
            return;
        }
        let seq = self.control.as_ref().map_or(0, |control| control.seq);
        self.control_last_seen_at = io.now_ms();
        if let Some(timer) = self.keepalive.take() {
            self.clear(io, timer);
        }
        if let Some(timer) = self.ready_timer.take() {
            self.clear(io, timer);
        }
        let ready = io.set_timeout(CONTROL_READY_TIMEOUT_MS);
        self.ready_timer = Some(ready);
        self.remember(ready, TimerKind::ControlReady(socket));
        let keepalive = io.set_interval(CONTROL_PING_INTERVAL_MS);
        self.keepalive = Some(keepalive);
        self.remember(keepalive, TimerKind::Keepalive(socket));
        Self::ping_or_terminate(io, socket, seq);
        io.log(LogRecord {
            level: LogLevel::Debug,
            message: "relay_control_open_waiting_for_ready",
            context: LogContext::Transport,
            fields: vec![("connectionId", number(seq))],
        });
    }

    /// The control socket's `close` event.
    pub fn on_control_close(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        code: u16,
        reason: Option<&[u8]>,
    ) {
        if !self.is_control(socket) {
            return;
        }
        let Some(control) = self.control.take() else {
            return;
        };
        let mut fields = vec![("code", FieldValue::Number(i64::from(code)))];
        if let Some(reason) = reason_text(reason) {
            fields.push(("reason", FieldValue::Text(reason)));
        }
        fields.push(("url", FieldValue::Text(control.url)));
        fields.push(("connectionId", number(control.seq)));
        warn(io, "relay_control_disconnected", fields);
        if let Some(timer) = self.keepalive.take() {
            self.clear(io, timer);
        }
        if let Some(timer) = self.ready_timer.take() {
            self.clear(io, timer);
        }
        self.schedule_reconnect(io);
    }

    /// The control socket's `error` event.
    pub fn on_control_error(&mut self, io: &mut dyn RelayIo, socket: SocketId, message: &str) {
        if !self.is_control(socket) {
            return;
        }
        let seq = self.control.as_ref().map_or(0, |control| control.seq);
        warn(
            io,
            "relay_error",
            vec![
                ("err", FieldValue::Error(message.to_owned())),
                ("connectionId", number(seq)),
            ],
        );
    }

    /// The control socket's `pong` event.
    pub fn on_control_pong(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if !self.is_control(socket) {
            return;
        }
        self.control_last_seen_at = io.now_ms();
        let seq = self.control.as_ref().map_or(0, |control| control.seq);
        io.log(LogRecord {
            level: LogLevel::Debug,
            message: "relay_control_pong_received",
            context: LogContext::Transport,
            fields: vec![("connectionId", number(seq))],
        });
    }

    fn mark_control_ready(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if !self.is_control(socket) {
            return;
        }
        let Some(control) = self.control.as_mut() else {
            return;
        };
        if control.connected {
            return;
        }
        control.connected = true;
        let seq = control.seq;
        self.reconnect_attempt = 0;
        if let Some(timer) = self.ready_timer.take() {
            self.clear(io, timer);
        }
        io.log(LogRecord {
            level: LogLevel::Info,
            message: "relay_control_connected",
            context: LogContext::Transport,
            fields: vec![("connectionId", number(seq))],
        });
    }

    /// The control socket's `message` event.
    pub fn on_control_message(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        raw: &MessageData,
    ) {
        if !self.is_control(socket) {
            return;
        }
        self.control_last_seen_at = io.now_ms();
        let message = try_parse_control_message(raw);
        if message.is_some() {
            self.mark_control_ready(io, socket);
        }
        let Some(message) = message else {
            return;
        };
        match message {
            ControlMessage::Ping => {
                let pong = format!(r#"{{"type":"pong","ts":{}}}"#, io.now_ms());
                let _ = io.send_text(socket, &pong);
            }
            ControlMessage::Pong => {}
            ControlMessage::Sync { connection_ids } => {
                for connection_id in connection_ids {
                    self.ensure_client_data_socket(io, &connection_id);
                }
            }
            ControlMessage::Connected { connection_id } => {
                self.ensure_client_data_socket(io, &connection_id);
            }
            ControlMessage::Disconnected { connection_id } => {
                if let Some(index) = self
                    .data_sockets
                    .iter()
                    .position(|(candidate, _)| *candidate == connection_id)
                {
                    // The original closes, then deletes; `ws` never emits `close` from inside
                    // `close()`, so deleting first is not observable.
                    let (_, existing) = self.data_sockets.remove(index);
                    let _ = io.close(existing, Some(1001), Some("Client disconnected"));
                }
            }
        }
    }

    fn ensure_client_data_socket(&mut self, io: &mut dyn RelayIo, connection_id: &JsString) {
        if self.stopped || connection_id.is_empty() {
            return;
        }
        if self
            .data_sockets
            .iter()
            .any(|(candidate, _)| candidate == connection_id)
        {
            return;
        }
        let Ok(url) = self.url(Some(connection_id)) else {
            return;
        };
        let socket = io.create_socket(&url);
        self.data_sockets.push((connection_id.clone(), socket));
        let open_timer = io.set_timeout(DATA_OPEN_TIMEOUT_MS);
        self.remember(open_timer, TimerKind::DataOpen(socket));
        self.closures.push(DataClosure {
            socket,
            connection_id: connection_id.clone(),
            url,
            open_timer,
            attached: false,
            closed: false,
            e2ee: None,
        });
    }

    fn closure(&mut self, socket: SocketId) -> Option<&mut DataClosure> {
        self.closures
            .iter_mut()
            .find(|closure| closure.socket == socket)
    }

    fn on_data_open_timeout(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        if self.stopped {
            return;
        }
        if io.ready_state(socket) == WEBSOCKET_OPEN {
            return;
        }
        let Some(closure) = self.closure(socket) else {
            return;
        };
        let connection_id = closure.connection_id.clone();
        io.log(LogRecord {
            level: LogLevel::Warn,
            message: "relay_data_open_timeout_terminating",
            context: LogContext::Transport,
            fields: vec![("connectionId", FieldValue::Js(connection_id))],
        });
        let _ = io.terminate(socket);
    }

    /// A data socket's `open` event.
    pub fn on_data_open(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        let has_key_pair = self.options.has_daemon_key_pair;
        let Some(closure) = self.closure(socket) else {
            return;
        };
        let (open_timer, connection_id, attached) = (
            closure.open_timer,
            closure.connection_id.clone(),
            closure.attached,
        );
        self.clear(io, open_timer);
        io.log(LogRecord {
            level: LogLevel::Info,
            message: "relay_data_connected",
            context: LogContext::Transport,
            fields: vec![("connectionId", FieldValue::Js(connection_id.clone()))],
        });
        if attached {
            return;
        }
        let mut session_key = utf16("session:");
        session_key.extend_from_slice(&connection_id);
        let metadata = AttachMetadata {
            external_session_key: session_key,
            relay_connection_id: connection_id,
        };
        if let Some(closure) = self.closure(socket) {
            closure.attached = true;
            if has_key_pair {
                closure.e2ee = Some(E2ee {
                    phase: E2eePhase::AwaitingChannel,
                    pending: Vec::new(),
                    socket: None,
                });
            }
        }
        if has_key_pair {
            io.start_daemon_channel(socket);
        } else {
            io.attach_plain(socket, &metadata);
        }
    }

    /// A data socket's `close` event, in the order the original registers its handlers: the
    /// transport's own, then the end-to-end adapter's.
    pub fn on_data_close(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        code: u16,
        reason: Option<&[u8]>,
    ) {
        if let Some(closure) = self.closure(socket) {
            let (open_timer, connection_id, url) = (
                closure.open_timer,
                closure.connection_id.clone(),
                closure.url.clone(),
            );
            self.clear(io, open_timer);
            let mut fields = vec![("code", FieldValue::Number(i64::from(code)))];
            if let Some(reason) = reason_text(reason) {
                fields.push(("reason", FieldValue::Text(reason)));
            }
            fields.push(("url", FieldValue::Text(url)));
            fields.push(("connectionId", FieldValue::Js(connection_id.clone())));
            warn(io, "relay_data_disconnected", fields);
            if self
                .data_sockets
                .iter()
                .any(|(candidate, current)| *candidate == connection_id && *current == socket)
            {
                self.data_sockets
                    .retain(|(candidate, _)| *candidate != connection_id);
            }
        }
        if self.adapter_active(socket) {
            io.channel_closed(socket, code, &reason_text(reason).unwrap_or_default());
        }
        if let Some(closure) = self.closure(socket) {
            closure.closed = true;
        }
        self.prune(socket);
    }

    /// A data socket's `error` event.
    pub fn on_data_error(&mut self, io: &mut dyn RelayIo, socket: SocketId, message: &str) {
        if let Some(closure) = self.closure(socket) {
            let connection_id = closure.connection_id.clone();
            io.log(LogRecord {
                level: LogLevel::Warn,
                message: "relay_data_error",
                context: LogContext::Transport,
                fields: vec![
                    ("err", FieldValue::Error(message.to_owned())),
                    ("connectionId", FieldValue::Js(connection_id)),
                ],
            });
        }
        if self.adapter_active(socket) {
            io.channel_error(socket, message);
        }
    }

    /// A data socket's `message` event, which only the end-to-end adapter consumes.
    pub fn on_data_message(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        data: &MessageData,
        is_binary: bool,
    ) {
        if self.adapter_active(socket) {
            io.channel_message(socket, normalize_message_data(data, is_binary), is_binary);
        }
    }

    /// The adapter registers its listeners when `attachEncryptedSocket` starts.
    fn adapter_active(&self, socket: SocketId) -> bool {
        self.closures
            .iter()
            .find(|closure| closure.socket == socket)
            .is_some_and(|closure| closure.e2ee.is_some())
    }

    /// `createDaemonChannel` resolved.
    pub fn on_channel_ready(&mut self, io: &mut dyn RelayIo, socket: SocketId) {
        let Some(closure) = self.closure(socket) else {
            return;
        };
        let connection_id = closure.connection_id.clone();
        let Some(e2ee) = closure.e2ee.as_mut() else {
            return;
        };
        if e2ee.phase != E2eePhase::AwaitingChannel {
            return;
        }
        e2ee.phase = E2eePhase::AwaitingAttach;
        e2ee.socket = Some(EncryptedRelaySocket::new(&mut SocketEnv { io, socket }));
        let mut session_key = utf16("session:");
        session_key.extend_from_slice(&connection_id);
        io.attach_encrypted(
            socket,
            &AttachMetadata {
                external_session_key: session_key,
                relay_connection_id: connection_id,
            },
        );
    }

    /// `createDaemonChannel` rejected.
    pub fn on_channel_failed(&mut self, io: &mut dyn RelayIo, socket: SocketId, message: &str) {
        self.handshake_failed(io, socket, message);
    }

    /// `attachSocket(encryptedSocket, ...)` settled.
    pub fn on_attach_settled(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        result: Result<(), String>,
    ) {
        if let Err(message) = result {
            self.handshake_failed(io, socket, &message);
            return;
        }
        let Some(closure) = self.closure(socket) else {
            return;
        };
        let Some(e2ee) = closure.e2ee.as_mut() else {
            return;
        };
        if e2ee.phase != E2eePhase::AwaitingAttach {
            return;
        }
        // `attached = true` comes before the flush, so a later message goes straight out.
        e2ee.phase = E2eePhase::Attached;
        let pending = std::mem::take(&mut e2ee.pending);
        for data in &pending {
            // The flush sits inside the original's `try`: a listener that throws fails the
            // handshake and the remaining messages are dropped.
            if let Err(message) = io.emit_message(socket, data) {
                // `attached` stays true: later messages go straight to the emitter.
                self.fail_handshake(io, socket, &message, false);
                return;
            }
        }
        self.prune(socket);
    }

    /// A plain `attachSocket(socket, metadata)` settled. The original does not await it
    /// (`void attachSocket(...)`), so a rejection is an unhandled rejection.
    #[must_use]
    pub fn on_plain_attach_settled(&mut self, result: Result<(), String>) -> Option<Fatal> {
        result.err().map(Fatal::UnhandledRejection)
    }

    fn handshake_failed(&mut self, io: &mut dyn RelayIo, socket: SocketId, message: &str) {
        self.fail_handshake(io, socket, message, true);
    }

    fn fail_handshake(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        message: &str,
        mark_failed: bool,
    ) {
        let Some(closure) = self.closure(socket) else {
            return;
        };
        let connection_id = closure.connection_id.clone();
        if let Some(e2ee) = closure.e2ee.as_mut().filter(|_| mark_failed) {
            e2ee.phase = E2eePhase::Failed;
        }
        io.log(LogRecord {
            level: LogLevel::Warn,
            message: "relay_e2ee_handshake_failed",
            context: LogContext::Attach(connection_id),
            fields: vec![("err", FieldValue::Error(message.to_owned()))],
        });
        let _ = io.close(socket, Some(1011), Some("E2EE handshake failed"));
        self.prune(socket);
    }

    /// Drops the closure of a socket that has closed and whose end-to-end attach is over.
    fn prune(&mut self, socket: SocketId) {
        self.closures.retain(|closure| {
            closure.socket != socket
                || !closure.closed
                || closure.e2ee.as_ref().is_some_and(|e2ee| {
                    matches!(
                        e2ee.phase,
                        E2eePhase::AwaitingChannel | E2eePhase::AwaitingAttach
                    )
                })
        });
    }

    /// The channel decrypted an application message (`events.onmessage`). An exception from
    /// a listener on an attached socket is uncaught in the original.
    #[must_use]
    pub fn on_channel_message(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        data: Data,
    ) -> Option<Fatal> {
        let e2ee = self.closure(socket)?.e2ee.as_mut()?;
        if e2ee.phase == E2eePhase::Attached {
            return io
                .emit_message(socket, &data)
                .err()
                .map(Fatal::UncaughtException);
        }
        e2ee.pending.push(data);
        None
    }

    /// The channel closed (`events.onclose`): the emitter reports it at once.
    #[must_use]
    pub fn on_channel_close(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        code: u16,
        reason: &str,
    ) -> Option<Fatal> {
        if let Some(encrypted) = self
            .closure(socket)
            .and_then(|closure| closure.e2ee.as_mut())
            .and_then(|e2ee| e2ee.socket.as_mut())
        {
            encrypted.on_emitter_close();
        }
        io.emit_close(socket, code, reason)
            .err()
            .map(Fatal::UncaughtException)
    }

    /// The channel failed (`events.onerror`): log, then the emitter reports it. With no
    /// `error` listener `EventEmitter` throws, which is uncaught in the original: the
    /// application's `attachSocket` returns without binding listeners while the server is
    /// starting or stopping.
    #[must_use]
    pub fn on_channel_error(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        message: &str,
    ) -> Option<Fatal> {
        let closure = self.closure(socket)?;
        let connection_id = closure.connection_id.clone();
        io.log(LogRecord {
            level: LogLevel::Warn,
            message: "relay_e2ee_error",
            context: LogContext::Attach(connection_id),
            fields: vec![("err", FieldValue::Error(message.to_owned()))],
        });
        io.emit_error(socket, message)
            .err()
            .map(Fatal::UncaughtException)
    }

    /// The end-to-end adapter's `send` (`relayTransport.send`): the channel's frame goes to
    /// the relay socket. A failed callback or a throw logs `relay_socket_send_failed` and
    /// rejects.
    pub fn adapter_send(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        data: &Data,
    ) -> AdapterSend {
        match io.send_data(socket, data) {
            SendStart::Threw(failure) => {
                AdapterSend::Settled(self.send_failed(io, socket, failure.0))
            }
            SendStart::Callback(None) => AdapterSend::Settled(Ok(())),
            SendStart::Callback(Some(message)) => {
                AdapterSend::Settled(self.send_failed(io, socket, message))
            }
            SendStart::Pending => AdapterSend::Pending,
        }
    }

    /// The `ws` send callback of a pending [`RelayTransport::adapter_send`] ran.
    ///
    /// # Errors
    ///
    /// The callback reported an error: the adapter's promise rejects with it.
    pub fn on_adapter_send_callback(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        error: Option<String>,
    ) -> Result<(), String> {
        match error {
            None => Ok(()),
            Some(message) => self.send_failed(io, socket, message),
        }
    }

    fn send_failed(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        message: String,
    ) -> Result<(), String> {
        if let Some(closure) = self.closure(socket) {
            let connection_id = closure.connection_id.clone();
            io.log(LogRecord {
                level: LogLevel::Warn,
                message: "relay_socket_send_failed",
                context: LogContext::Attach(connection_id),
                fields: vec![("err", FieldValue::Error(message.clone()))],
            });
        }
        Err(message)
    }

    /// The application sends on its encrypted socket.
    ///
    /// # Errors
    ///
    /// The terminate at the high-water mark threw.
    pub fn encrypted_send(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        data: &Data,
    ) -> Option<Result<SendOutcome, EnvFailure>> {
        let encrypted = self.closure(socket)?.e2ee.as_mut()?.socket.as_mut()?;
        Some(encrypted.send(&mut SocketEnv { io, socket }, data))
    }

    /// The channel's send for an application frame settled. A failure reaches the
    /// socket's `error` listeners first, then rejects with the same error, or with what a
    /// throwing listener threw.
    ///
    /// # Errors
    ///
    /// The send's promise rejects with the returned message.
    pub fn encrypted_send_settled(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        result: Result<(), String>,
    ) -> Result<(), String> {
        match result {
            Ok(()) => Ok(()),
            Err(message) => match io.emit_error(socket, &message) {
                Ok(()) => Err(message),
                Err(thrown) => Err(thrown),
            },
        }
    }

    /// The application closes its encrypted socket.
    ///
    /// # Errors
    ///
    /// `channel.close` threw.
    pub fn encrypted_close(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
        code: Option<u16>,
        reason: Option<&str>,
    ) -> Result<(), EnvFailure> {
        match self
            .closure(socket)
            .and_then(|c| c.e2ee.as_mut())
            .and_then(|e| e.socket.as_mut())
        {
            Some(encrypted) => encrypted.close(&mut SocketEnv { io, socket }, code, reason),
            None => Ok(()),
        }
    }

    /// The application terminates its encrypted socket.
    ///
    /// # Errors
    ///
    /// `socket.terminate()` threw.
    pub fn encrypted_terminate(
        &mut self,
        io: &mut dyn RelayIo,
        socket: SocketId,
    ) -> Result<(), EnvFailure> {
        match self
            .closure(socket)
            .and_then(|c| c.e2ee.as_mut())
            .and_then(|e| e.socket.as_mut())
        {
            Some(encrypted) => encrypted.terminate(&mut SocketEnv { io, socket }),
            None => Ok(()),
        }
    }

    /// The `readyState` of the application's encrypted socket. `None` once the socket has
    /// closed and its closure was dropped: treat it as closed.
    #[must_use]
    pub fn encrypted_ready_state(&self, socket: SocketId) -> Option<u8> {
        self.closures
            .iter()
            .find(|closure| closure.socket == socket)?
            .e2ee
            .as_ref()?
            .socket
            .as_ref()
            .map(EncryptedRelaySocket::ready_state)
    }
}
