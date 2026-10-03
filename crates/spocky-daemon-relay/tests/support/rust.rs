//! The Rust side of the differential: executes the driver's operations on the port.
//!
//! The harness reproduces the driver's virtual clock and fake sockets, implements
//! [`RelayIo`] so every effect the transport performs becomes a trace entry, and runs the
//! driver's promise continuations (channel ready, attach settled) at the end of each
//! operation, in the order the microtask queue would.

use std::collections::VecDeque;
use std::fmt::Write as _;

use serde_json::Value;
use spocky_crypto::channel::{AppSend, ChannelError, Data, SendId, TransportError};
use spocky_crypto::js_string::{JsString, utf16};
use spocky_daemon_relay::control::MessageData;
use spocky_daemon_relay::encrypted_socket::{
    EncryptedRelayEnv, EncryptedRelaySocket, SendOutcome, Settlement,
};
use spocky_daemon_relay::endpoint::{
    RelayRole, RelayUrlParams, VersionInput, build_relay_websocket_url,
    normalize_relay_protocol_version, parse_host_port,
};
use spocky_daemon_relay::runtime::{
    RelayRuntime, RelayRuntimeConfig, RuntimeEffect, TransportController, TransportStarter,
};
use spocky_daemon_relay::transport::{
    AttachMetadata, FieldValue, IoFailure, LogContext, LogLevel, LogRecord, RelayIo,
    RelayTransport, SocketId, TimerId, TransportOptions,
};

use super::{Endpoint, entry::Entry};

const START: i64 = 1_000_000_000_000;

fn hex_decode(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

fn data_entry(entry: Entry, data: &Data) -> Entry {
    match data {
        Data::Text(text) => entry.str("text", text),
        Data::Binary(bytes) => entry.str("binary", &hex_encode(bytes)),
    }
}

#[derive(Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
struct Modes {
    send_throws: bool,
    close_throws: bool,
    terminate_throws: bool,
    ping_throws: bool,
}

struct Socket {
    ready_state: u8,
    buffered: u64,
    modes: Modes,
}

struct Timer {
    id: u64,
    due: i64,
    seq: u64,
    interval: Option<u64>,
}

#[derive(Clone, Copy, PartialEq)]
enum AttachMode {
    Ok,
    Reject,
    Pending,
}

#[derive(Clone, Copy, PartialEq)]
enum ChannelMode {
    Ok,
    Fail,
    Pending,
}

/// A continuation the driver runs as a promise reaction.
enum Reaction {
    ChannelReady(SocketId),
    ChannelFailed(SocketId, String),
    AttachSettled(SocketId, Result<(), String>),
}

struct Channel {
    socket: SocketId,
}

/// Everything except the transport, so the transport can borrow it as [`RelayIo`].
struct Io {
    entries: Vec<String>,
    now: i64,
    timer_seq: u64,
    timers: Vec<Timer>,
    sockets: Vec<Socket>,
    defaults: Modes,
    attach_mode: AttachMode,
    /// Pending `attachSocket` promises, in call order; `None` is a plain attach whose
    /// result nobody observes.
    attach_waiters: VecDeque<Option<SocketId>>,
    channel_mode: ChannelMode,
    channel_fail_message: String,
    channels: Vec<Channel>,
    reactions: VecDeque<Reaction>,
    /// Plain sockets whose raw events the application listens to.
    app_plain: Vec<SocketId>,
    /// Encrypted sockets whose emitter the application listens to.
    app_encrypted: Vec<SocketId>,
    ids: Vec<(SocketId, JsString)>,
}

impl Io {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            now: START,
            timer_seq: 0,
            timers: Vec::new(),
            sockets: Vec::new(),
            defaults: Modes::default(),
            attach_mode: AttachMode::Ok,
            attach_waiters: VecDeque::new(),
            channel_mode: ChannelMode::Pending,
            channel_fail_message: "handshake failed".to_owned(),
            channels: Vec::new(),
            reactions: VecDeque::new(),
            app_plain: Vec::new(),
            app_encrypted: Vec::new(),
            ids: Vec::new(),
        }
    }

    fn log(&mut self, entry: &Entry) {
        self.entries.push(entry.render());
    }

    fn socket(&mut self, socket: SocketId) -> &mut Socket {
        &mut self.sockets[usize::try_from(socket.0).unwrap() - 1]
    }

    fn channel_number(&self, socket: SocketId) -> u64 {
        self.channels
            .iter()
            .position(|channel| channel.socket == socket)
            .map_or(0, |index| u64::try_from(index).unwrap() + 1)
    }

    fn connection_id(&self, socket: SocketId) -> JsString {
        self.ids
            .iter()
            .find(|(candidate, _)| *candidate == socket)
            .map(|(_, id)| id.clone())
            .unwrap_or_default()
    }

    fn metadata_entry(metadata: &AttachMetadata) -> String {
        Entry::new("m")
            .str("transport", "relay")
            .js("externalSessionKey", &metadata.external_session_key)
            .js("relayConnectionId", &metadata.relay_connection_id)
            .render()
            .replacen("\"t\":\"m\",", "", 1)
    }

    fn app_entry(&self, action: &str, socket: SocketId) -> Entry {
        Entry::new("app")
            .str("a", action)
            .js("id", &self.connection_id(socket))
    }
}

impl RelayIo for Io {
    fn create_socket(&mut self, url: &str) -> SocketId {
        let id = SocketId(u64::try_from(self.sockets.len()).unwrap() + 1);
        self.sockets.push(Socket {
            ready_state: 0,
            buffered: 0,
            modes: self.defaults.clone(),
        });
        self.log(
            &Entry::new("ws")
                .str("a", "create")
                .num("id", id.0)
                .str("url", url),
        );
        id
    }

    fn ping(&mut self, socket: SocketId) -> Result<(), IoFailure> {
        self.log(&Entry::new("ws").str("a", "ping").num("id", socket.0));
        if self.socket(socket).modes.ping_throws {
            return Err(IoFailure("ping threw".to_owned()));
        }
        Ok(())
    }

    fn terminate(&mut self, socket: SocketId) -> Result<(), IoFailure> {
        self.log(&Entry::new("ws").str("a", "terminate").num("id", socket.0));
        if self.socket(socket).modes.terminate_throws {
            return Err(IoFailure("terminate threw".to_owned()));
        }
        Ok(())
    }

    fn close(
        &mut self,
        socket: SocketId,
        code: Option<u16>,
        reason: Option<&str>,
    ) -> Result<(), IoFailure> {
        let mut entry = Entry::new("ws").str("a", "close").num("id", socket.0);
        if let Some(code) = code {
            entry = entry.num("code", code);
        }
        self.log(&entry.opt_str("reason", reason));
        if self.socket(socket).modes.close_throws {
            return Err(IoFailure("close threw".to_owned()));
        }
        Ok(())
    }

    fn send_text(&mut self, socket: SocketId, text: &str) -> Result<(), IoFailure> {
        self.log(
            &Entry::new("ws")
                .str("a", "send")
                .num("id", socket.0)
                .str("text", text),
        );
        if self.socket(socket).modes.send_throws {
            return Err(IoFailure("send threw".to_owned()));
        }
        Ok(())
    }

    fn ready_state(&self, socket: SocketId) -> u8 {
        self.sockets[usize::try_from(socket.0).unwrap() - 1].ready_state
    }

    fn now_ms(&self) -> i64 {
        self.now
    }

    fn set_timeout(&mut self, delay_ms: u64) -> TimerId {
        self.timer_seq += 1;
        let id = self.timer_seq;
        self.timers.push(Timer {
            id,
            due: self.now + i64::try_from(delay_ms).unwrap(),
            seq: id,
            interval: None,
        });
        TimerId(id)
    }

    fn set_interval(&mut self, delay_ms: u64) -> TimerId {
        self.timer_seq += 1;
        let id = self.timer_seq;
        self.timers.push(Timer {
            id,
            due: self.now + i64::try_from(delay_ms).unwrap(),
            seq: id,
            interval: Some(delay_ms),
        });
        TimerId(id)
    }

    fn clear_timer(&mut self, timer: TimerId) {
        self.timers.retain(|candidate| candidate.id != timer.0);
    }

    fn log(&mut self, record: LogRecord) {
        let level = match record.level {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
        };
        let context = match &record.context {
            LogContext::Transport => r#"{"module":"relay-transport"}"#.to_owned(),
            LogContext::Attach(id) => format!(
                r#"{{"module":"relay-transport","connectionId":{}}}"#,
                spocky_crypto::js_string::json_quote(id)
            ),
        };
        let fields: Vec<String> = record
            .fields
            .iter()
            .map(|(name, value)| {
                let rendered = match value {
                    FieldValue::Number(number) => number.to_string(),
                    FieldValue::Text(text) => spocky_crypto::js_string::json_quote(&utf16(text)),
                    FieldValue::Js(text) => spocky_crypto::js_string::json_quote(text),
                    FieldValue::Error(message) => format!(
                        r#"{{"error":{}}}"#,
                        spocky_crypto::js_string::json_quote(&utf16(message))
                    ),
                };
                format!("\"{name}\":{rendered}")
            })
            .collect();
        self.entries.push(
            Entry::new("log")
                .str("level", level)
                .str("msg", record.message)
                .raw("ctx", context)
                .raw("fields", format!("{{{}}}", fields.join(",")))
                .render(),
        );
    }

    fn attach_plain(&mut self, socket: SocketId, metadata: &AttachMetadata) {
        self.ids
            .push((socket, metadata.relay_connection_id.clone()));
        self.log(
            &Entry::new("attach")
                .js("id", &metadata.relay_connection_id)
                .str("kind", "plain")
                .raw("metadata", Self::metadata_entry(metadata)),
        );
        self.app_plain.push(socket);
        if self.attach_mode == AttachMode::Pending {
            self.attach_waiters.push_back(None);
        }
    }

    fn start_daemon_channel(&mut self, socket: SocketId) {
        self.channels.push(Channel { socket });
        let n = self.channels.len();
        self.log(
            &Entry::new("channel")
                .str("a", "create")
                .num("n", n)
                .raw("keyPairShape", "[32,32]"),
        );
        match self.channel_mode {
            ChannelMode::Ok => self.reactions.push_back(Reaction::ChannelReady(socket)),
            ChannelMode::Fail => self.reactions.push_back(Reaction::ChannelFailed(
                socket,
                self.channel_fail_message.clone(),
            )),
            ChannelMode::Pending => {}
        }
    }

    fn attach_encrypted(&mut self, socket: SocketId, metadata: &AttachMetadata) {
        self.ids
            .push((socket, metadata.relay_connection_id.clone()));
        self.log(
            &Entry::new("attach")
                .js("id", &metadata.relay_connection_id)
                .str("kind", "encrypted")
                .raw("metadata", Self::metadata_entry(metadata)),
        );
        self.app_encrypted.push(socket);
        match self.attach_mode {
            AttachMode::Ok => self
                .reactions
                .push_back(Reaction::AttachSettled(socket, Ok(()))),
            AttachMode::Reject => self.reactions.push_back(Reaction::AttachSettled(
                socket,
                Err("attach rejected".to_owned()),
            )),
            AttachMode::Pending => self.attach_waiters.push_back(Some(socket)),
        }
    }

    fn channel_message(&mut self, socket: SocketId, data: Data, is_binary: bool) {
        let n = self.channel_number(socket);
        let entry = data_entry(Entry::new("channel").str("a", "rx").num("n", n), &data);
        self.log(&entry.bool("isBinary", is_binary));
    }

    fn channel_closed(&mut self, socket: SocketId, code: u16, reason: &str) {
        let n = self.channel_number(socket);
        self.log(
            &Entry::new("channel")
                .str("a", "closed")
                .num("n", n)
                .num("code", code)
                .str("reason", reason),
        );
    }

    fn channel_error(&mut self, socket: SocketId, message: &str) {
        let n = self.channel_number(socket);
        self.log(
            &Entry::new("channel")
                .str("a", "error")
                .num("n", n)
                .str("message", message),
        );
    }

    fn emit_message(&mut self, socket: SocketId, data: &Data) {
        if self.app_encrypted.contains(&socket) {
            let entry = data_entry(self.app_entry("message", socket), data);
            self.log(&entry);
        }
    }

    fn emit_close(&mut self, socket: SocketId, code: u16, reason: &str) {
        if self.app_encrypted.contains(&socket) {
            let entry = self
                .app_entry("close", socket)
                .num("code", code)
                .str("reason", reason);
            self.log(&entry);
        }
    }

    fn emit_error(&mut self, socket: SocketId, message: &str) {
        if self.app_encrypted.contains(&socket) {
            let entry = self.app_entry("error", socket).str("message", message);
            self.log(&entry);
        }
    }

    fn channel_set_state_open(&mut self, socket: SocketId) {
        let n = self.channel_number(socket);
        self.log(
            &Entry::new("channel")
                .str("a", "setState")
                .num("n", n)
                .str("state", "open"),
        );
    }

    fn channel_send(&mut self, socket: SocketId, data: &Data) -> AppSend {
        let n = self.channel_number(socket);
        let entry = data_entry(Entry::new("channel").str("a", "send").num("n", n), data);
        self.log(&entry);
        AppSend::Settled(Ok(()))
    }

    fn channel_outbound_wire_byte_length(&self, _socket: SocketId, data: &Data) -> u64 {
        let length = match data {
            Data::Text(text) => text.len(),
            Data::Binary(bytes) => bytes.len(),
        };
        u64::try_from(length).unwrap() + 40
    }

    fn channel_close(&mut self, socket: SocketId, code: Option<u16>, reason: Option<&str>) {
        let n = self.channel_number(socket);
        let mut entry = Entry::new("channel").str("a", "close").num("n", n);
        if let Some(code) = code {
            entry = entry.num("code", code);
        }
        self.log(&entry.opt_str("reason", reason));
    }

    fn transport_buffered_amount(&self, socket: SocketId) -> Option<u64> {
        Some(self.sockets[usize::try_from(socket.0).unwrap() - 1].buffered)
    }
}

pub struct RustEndpoint {
    io: Io,
    transport: Option<RelayTransport>,
    options: Option<TransportOptions>,
    runtime: Option<RelayRuntime<RuntimeStarter>>,
    runtime_log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    runtime_start_mode: std::rc::Rc<std::cell::Cell<StartMode>>,
    enc: Option<Enc>,
}

#[derive(Clone, Copy, PartialEq)]
enum StartMode {
    Ok,
    Throw,
    StopRejects,
}

struct RuntimeStarter {
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    mode: std::rc::Rc<std::cell::Cell<StartMode>>,
    server_id: String,
}

struct RuntimeController {
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    mode: std::rc::Rc<std::cell::Cell<StartMode>>,
}

impl TransportController for RuntimeController {
    fn stop(self: Box<Self>) -> Result<(), String> {
        self.log
            .borrow_mut()
            .push(Entry::new("runtime").str("a", "stop").render());
        if self.mode.get() == StartMode::StopRejects {
            return Err("stop failed".to_owned());
        }
        Ok(())
    }
}

impl TransportStarter for RuntimeStarter {
    fn start(
        &mut self,
        config: &RelayRuntimeConfig,
    ) -> Result<Box<dyn TransportController>, String> {
        self.log.borrow_mut().push(
            Entry::new("runtime")
                .str("a", "start")
                .str("endpoint", &config.endpoint)
                .bool("useTls", config.use_tls)
                .str("serverId", &self.server_id)
                .render(),
        );
        if self.mode.get() == StartMode::Throw {
            return Err("Invalid relay endpoint".to_owned());
        }
        Ok(Box::new(RuntimeController {
            log: self.log.clone(),
            mode: self.mode.clone(),
        }))
    }
}

/// A standalone encrypted socket scenario (`enc.*` operations).
struct Enc {
    socket: EncryptedRelaySocket,
    env: EncEnv,
    listeners: bool,
    sends: Vec<Option<SendId>>,
    pending: VecDeque<usize>,
}

struct EncEnv {
    entries: Vec<String>,
    send_mode: String,
    overhead: u64,
    buffered: Option<u64>,
    next_send_id: u64,
}

impl EncryptedRelayEnv for EncEnv {
    fn set_state_open(&mut self) {
        self.entries.push(
            Entry::new("enc")
                .str("a", "setState")
                .str("state", "open")
                .render(),
        );
    }

    fn channel_send(&mut self, data: &Data) -> AppSend {
        let entry = data_entry(Entry::new("enc").str("a", "channel.send"), data);
        self.entries.push(entry.render());
        match self.send_mode.as_str() {
            "reject" => AppSend::Settled(Err(ChannelError::Transport(TransportError(
                "channel send failed".to_owned(),
            )))),
            "pending" => {
                self.next_send_id += 1;
                AppSend::Pending(SendId(self.next_send_id))
            }
            _ => AppSend::Settled(Ok(())),
        }
    }

    fn outbound_wire_byte_length(&self, data: &Data) -> u64 {
        let length = match data {
            Data::Text(text) => text.len(),
            Data::Binary(bytes) => bytes.len(),
        };
        u64::try_from(length).unwrap() + self.overhead
    }

    fn channel_close(&mut self, code: Option<u16>, reason: Option<&str>) {
        let mut entry = Entry::new("enc").str("a", "channel.close");
        if let Some(code) = code {
            entry = entry.num("code", code);
        }
        self.entries.push(entry.opt_str("reason", reason).render());
    }

    fn transport_buffered_amount(&self) -> Option<u64> {
        self.buffered
    }

    fn terminate_transport(&mut self) {
        self.entries
            .push(Entry::new("enc").str("a", "terminateTransport").render());
    }
}

impl RustEndpoint {
    pub fn new() -> Self {
        Self {
            io: Io::new(),
            transport: None,
            options: None,
            runtime: None,
            runtime_log: std::rc::Rc::default(),
            runtime_start_mode: std::rc::Rc::new(std::cell::Cell::new(StartMode::Ok)),
            enc: None,
        }
    }

    fn thrown(&mut self, message: &str) {
        self.io.log(&Entry::new("throw").str("message", message));
    }

    fn version_input(value: &Value) -> VersionInput {
        match value {
            Value::Null => VersionInput::Missing,
            Value::String(text) => VersionInput::String(text.clone()),
            Value::Number(number) => VersionInput::Number(number.as_f64().unwrap()),
            _ => VersionInput::Other,
        }
    }

    /// Runs the transport against `io`, with the encrypted environment view available.
    fn with_transport<R>(
        &mut self,
        action: impl FnOnce(&mut RelayTransport, &mut dyn RelayIo) -> R,
    ) -> Option<R> {
        let mut transport = self.transport.take()?;
        let result = action(&mut transport, &mut self.io);
        self.transport = Some(transport);
        Some(result)
    }

    /// Promise reactions queued during an operation, in order.
    fn drain(&mut self) {
        while let Some(reaction) = self.io.reactions.pop_front() {
            match reaction {
                Reaction::ChannelReady(socket) => {
                    self.with_transport(|transport, io| transport.on_channel_ready(io, socket));
                }
                Reaction::ChannelFailed(socket, message) => {
                    self.with_transport(|transport, io| {
                        transport.on_channel_failed(io, socket, &message);
                    });
                }
                Reaction::AttachSettled(socket, result) => {
                    self.with_transport(|transport, io| {
                        transport.on_attach_settled(io, socket, result);
                    });
                }
            }
        }
    }

    fn socket_event(&mut self, op: &Value) {
        let socket = SocketId(op["id"].as_u64().unwrap());
        match op["event"].as_str().unwrap() {
            "open" => {
                self.io.socket(socket).ready_state = 1;
                self.with_transport(|transport, io| transport.on_open(io, socket));
            }
            "message" => {
                let data = match op["kind"].as_str().unwrap_or("buffer") {
                    "buffer" => MessageData::Buffer(match op["hex"].as_str() {
                        Some(hex) => hex_decode(hex),
                        None => op["text"].as_str().unwrap().as_bytes().to_vec(),
                    }),
                    "string" => MessageData::Text(op["text"].as_str().unwrap().to_owned()),
                    "arraybuffer" => {
                        MessageData::ArrayBuffer(hex_decode(op["hex"].as_str().unwrap()))
                    }
                    "fragments" => MessageData::Fragments(
                        op["parts"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|part| hex_decode(part.as_str().unwrap()))
                            .collect(),
                    ),
                    other => panic!("unknown message kind {other}"),
                };
                let is_binary = op["isBinary"] == true;
                self.with_transport(|transport, io| {
                    transport.on_message(io, socket, &data, is_binary);
                });
                if self.io.app_plain.contains(&socket) {
                    let bytes = match &data {
                        MessageData::Buffer(bytes) | MessageData::ArrayBuffer(bytes) => {
                            Some(bytes.clone())
                        }
                        MessageData::Fragments(parts) => Some(parts.concat()),
                        MessageData::Text(_) => None,
                    };
                    let entry = self.io.app_entry("message", socket);
                    let entry = match (&data, bytes) {
                        (MessageData::Text(text), _) => entry.str("text", text),
                        (MessageData::ArrayBuffer(_), Some(bytes)) => {
                            entry.str("binary", &hex_encode(&bytes))
                        }
                        // A Buffer (or Buffer[]) is a Uint8Array view: dataEntry reports binary.
                        (_, Some(bytes)) => entry.str("binary", &hex_encode(&bytes)),
                        _ => entry,
                    };
                    self.io.log(&entry);
                }
            }
            "close" => {
                self.io.socket(socket).ready_state = 3;
                let code = u16::try_from(op["code"].as_u64().unwrap()).unwrap();
                let reason: Option<Vec<u8>> =
                    op["reason"].as_str().map(|text| text.as_bytes().to_vec());
                self.with_transport(|transport, io| {
                    transport.on_close(io, socket, code, reason.as_deref());
                });
                if self.io.app_plain.contains(&socket) {
                    let entry = self.io.app_entry("close", socket).num("code", code);
                    let entry = match &reason {
                        Some(bytes) => entry.str("reason", &String::from_utf8_lossy(bytes)),
                        None => entry,
                    };
                    self.io.log(&entry);
                }
            }
            "error" => {
                let message = op["message"].as_str().unwrap();
                self.with_transport(|transport, io| transport.on_error(io, socket, message));
                if self.io.app_plain.contains(&socket) {
                    let entry = self.io.app_entry("error", socket).str("message", message);
                    self.io.log(&entry);
                }
            }
            "pong" => {
                self.with_transport(|transport, io| transport.on_pong(io, socket));
            }
            other => panic!("unknown socket event {other}"),
        }
    }

    fn app_socket(&self, id: &str) -> SocketId {
        let units = utf16(id);
        self.io
            .ids
            .iter()
            .find(|(_, candidate)| *candidate == units)
            .map(|(socket, _)| *socket)
            .unwrap()
    }

    fn app_op(&mut self, op: &Value) {
        let id = op["id"].as_str().unwrap();
        let socket = self.app_socket(id);
        match op["op"].as_str().unwrap() {
            "app.send" => {
                let data = match op["text"].as_str() {
                    Some(text) => Data::Text(text.to_owned()),
                    None => Data::Binary(hex_decode(op["binary"].as_str().unwrap())),
                };
                let outcome = self
                    .with_transport(|transport, io| transport.encrypted_send(io, socket, &data))
                    .flatten()
                    .unwrap();
                let entry = Entry::new("app").str("a", "send.settled").str("id", id);
                match outcome {
                    SendOutcome::Rejected(error) => self.io.log(
                        &entry
                            .str("result", "rejected")
                            .str("message", &error.to_string()),
                    ),
                    SendOutcome::Channel(_) => self.io.log(&entry.str("result", "resolved")),
                }
            }
            "app.close" => {
                let code = op["code"].as_u64().map(|code| u16::try_from(code).unwrap());
                let reason = op["reason"].as_str();
                self.with_transport(|transport, io| {
                    transport.encrypted_close(io, socket, code, reason);
                });
            }
            "app.terminate" => {
                self.with_transport(|transport, io| transport.encrypted_terminate(io, socket));
            }
            _ => {
                let ready_state = self
                    .transport
                    .as_ref()
                    .and_then(|transport| transport.encrypted_ready_state(socket))
                    .unwrap_or(self.io.ready_state(socket));
                let buffered = self.io.socket(socket).buffered;
                self.io.log(
                    &Entry::new("app")
                        .str("a", "read")
                        .str("id", id)
                        .num("readyState", ready_state)
                        .num("bufferedAmount", buffered),
                );
            }
        }
    }

    fn advance(&mut self, ms: i64) {
        let target = self.io.now + ms;
        loop {
            let next = self
                .io
                .timers
                .iter()
                .enumerate()
                .min_by_key(|(_, timer)| (timer.due, timer.seq))
                .map(|(index, timer)| (index, timer.due, timer.id));
            let Some((index, due, id)) = next else { break };
            if due > target {
                break;
            }
            self.io.now = due;
            match self.io.timers[index].interval {
                None => {
                    self.io.timers.remove(index);
                }
                Some(interval) => {
                    self.io.timer_seq += 1;
                    let seq = self.io.timer_seq;
                    let timer = &mut self.io.timers[index];
                    timer.due += i64::try_from(interval).unwrap();
                    timer.seq = seq;
                }
            }
            self.with_transport(|transport, io| transport.on_timer(io, TimerId(id)));
            self.drain();
        }
        self.io.now = target;
    }

    fn runtime_config_entry(&self) -> Entry {
        let config = self.runtime.as_ref().unwrap().config();
        Entry::new("runtime").str("a", "config").raw(
            "config",
            format!(
                r#"{{"enabled":{},"endpoint":{},"publicEndpoint":{},"publicUseTls":{},"useTls":{}}}"#,
                config.enabled,
                spocky_crypto::js_string::json_quote(&utf16(&config.endpoint)),
                spocky_crypto::js_string::json_quote(&utf16(&config.public_endpoint)),
                config.public_use_tls,
                config.use_tls
            ),
        )
    }

    fn flush_runtime_log(&mut self) {
        let lines: Vec<String> = self.runtime_log.borrow_mut().drain(..).collect();
        self.io.entries.extend(lines);
    }

    fn runtime_effects(&mut self, effects: Vec<RuntimeEffect>) {
        for effect in effects {
            let RuntimeEffect::StopFailed(message) = effect;
            self.io.entries.push(
                Entry::new("log")
                    .str("level", "warn")
                    .str("msg", "Failed to stop relay transport")
                    .raw("ctx", r#"{"runtime":true}"#)
                    .raw(
                        "fields",
                        format!(
                            r#"{{"err":{{"error":{}}}}}"#,
                            spocky_crypto::js_string::json_quote(&utf16(&message))
                        ),
                    )
                    .render(),
            );
        }
    }

    fn enc_op(&mut self, op: &Value) {
        let name = op["op"].as_str().unwrap();
        if name == "enc.create" {
            let mut env = EncEnv {
                entries: Vec::new(),
                send_mode: op["sendMode"].as_str().unwrap_or("sync").to_owned(),
                overhead: op["overhead"].as_u64().unwrap_or(40),
                buffered: op["buffered"].as_u64(),
                next_send_id: 0,
            };
            let socket = EncryptedRelaySocket::new(&mut env);
            let ready_state = socket.ready_state();
            let buffered = socket.buffered_amount(&env);
            self.io.entries.append(&mut env.entries);
            self.io.log(
                &Entry::new("enc")
                    .str("a", "created")
                    .num("readyState", ready_state)
                    .num("bufferedAmount", buffered),
            );
            self.enc = Some(Enc {
                socket,
                env,
                listeners: op["listeners"] != false,
                sends: Vec::new(),
                pending: VecDeque::new(),
            });
            return;
        }
        let mut enc = self.enc.take().unwrap();
        match name {
            "enc.send" => {
                let data = match op["text"].as_str() {
                    Some(text) => Data::Text(text.to_owned()),
                    None => Data::Binary(hex_decode(op["binary"].as_str().unwrap())),
                };
                let index = enc.sends.len();
                let outcome = enc.socket.send(&mut enc.env, &data);
                self.io.entries.append(&mut enc.env.entries);
                match outcome {
                    SendOutcome::Rejected(error) => {
                        enc.sends.push(None);
                        self.enc_settled(index, Err(error.to_string()));
                    }
                    SendOutcome::Channel(AppSend::Settled(result)) => {
                        enc.sends.push(None);
                        let settled = match EncryptedRelaySocket::settle(result) {
                            Settlement::Resolved => Ok(()),
                            Settlement::EmitErrorThenReject(error) => {
                                if enc.listeners {
                                    self.io.log(
                                        &Entry::new("enc")
                                            .str("a", "emit.error")
                                            .str("message", &error.to_string()),
                                    );
                                }
                                Err(error.to_string())
                            }
                        };
                        self.enc_settled(index, settled);
                    }
                    SendOutcome::Channel(AppSend::Pending(id)) => {
                        enc.sends.push(Some(id));
                        enc.pending.push_back(index);
                    }
                }
            }
            "enc.settle" => {
                let index = enc.pending.pop_front().unwrap();
                let settled = if op["result"] == "reject" {
                    if enc.listeners {
                        self.io.log(
                            &Entry::new("enc")
                                .str("a", "emit.error")
                                .str("message", "channel send failed"),
                        );
                    }
                    Err("channel send failed".to_owned())
                } else {
                    Ok(())
                };
                self.enc_settled(index, settled);
            }
            "enc.state" => {
                if let Some(buffered) = op.get("buffered") {
                    enc.env.buffered = buffered.as_u64();
                }
            }
            "enc.close" => {
                enc.socket.close(
                    &mut enc.env,
                    op["code"].as_u64().map(|code| u16::try_from(code).unwrap()),
                    op["reason"].as_str(),
                );
                self.io.entries.append(&mut enc.env.entries);
            }
            "enc.terminate" => {
                enc.socket.terminate(&mut enc.env);
                self.io.entries.append(&mut enc.env.entries);
            }
            "enc.emit" => match op["event"].as_str().unwrap() {
                "close" => {
                    enc.socket.on_emitter_close();
                    let code = op["code"].as_u64().unwrap();
                    let entry = Entry::new("enc").str("a", "emit.close").num("code", code);
                    self.io.log(&entry.opt_str("reason", op["reason"].as_str()));
                }
                "error" => {
                    if enc.listeners {
                        self.io.log(
                            &Entry::new("enc")
                                .str("a", "emit.error")
                                .str("message", op["message"].as_str().unwrap()),
                        );
                    } else {
                        self.io.log(
                            &Entry::new("driver-error")
                                .str("message", op["message"].as_str().unwrap()),
                        );
                    }
                }
                _ => {
                    self.io.log(
                        &Entry::new("enc")
                            .str("a", "emit.message")
                            .str("text", op["text"].as_str().unwrap()),
                    );
                }
            },
            "enc.read" => {
                let buffered = enc.socket.buffered_amount(&enc.env);
                self.io.log(
                    &Entry::new("enc")
                        .str("a", "read")
                        .num("readyState", enc.socket.ready_state())
                        .num("bufferedAmount", buffered),
                );
            }
            other => panic!("unsupported operation {other}"),
        }
        self.enc = Some(enc);
    }

    fn enc_settled(&mut self, index: usize, result: Result<(), String>) {
        let entry = Entry::new("enc")
            .str("a", "send.settled")
            .num("index", index);
        match result {
            Ok(()) => self.io.log(&entry.str("result", "resolved")),
            Err(message) => self
                .io
                .log(&entry.str("result", "rejected").str("message", &message)),
        }
    }
}

impl Endpoint for RustEndpoint {
    fn op(&mut self, op: &Value) -> Vec<String> {
        let name = op["op"].as_str().unwrap();
        self.io.entries.clear();
        if name.starts_with("enc.") {
            self.enc_op(op);
            return std::mem::take(&mut self.io.entries);
        }
        match name {
            "reset" => {
                self.io = Io::new();
                self.transport = None;
                self.options = None;
                self.runtime = None;
                self.runtime_log.borrow_mut().clear();
                self.runtime_start_mode.set(StartMode::Ok);
                self.enc = None;
            }
            "parseHostPort" => match parse_host_port(op["input"].as_str().unwrap()) {
                Ok(parts) => self.io.log(
                    &Entry::new("hostport")
                        .str("host", &parts.host)
                        .num("port", parts.port)
                        .bool("isIpv6", parts.is_ipv6),
                ),
                Err(error) => self.thrown(&error.0),
            },
            "buildUrl" => {
                let params = RelayUrlParams {
                    endpoint: op["endpoint"].as_str().unwrap(),
                    use_tls: op["useTls"].as_bool().unwrap(),
                    server_id: op["serverId"].as_str().unwrap(),
                    role: if op["role"] == "server" {
                        RelayRole::Server
                    } else {
                        RelayRole::Client
                    },
                    connection_id: op["connectionId"].as_str(),
                    version: Self::version_input(&op["version"]),
                };
                match build_relay_websocket_url(&params) {
                    Ok(url) => self.io.log(&Entry::new("url").str("url", &url)),
                    Err(error) => self.thrown(&error.0),
                }
            }
            "normalizeVersion" => {
                match normalize_relay_protocol_version(&Self::version_input(&op["value"])) {
                    Ok(version) => self.io.log(&Entry::new("version").str("value", version)),
                    Err(error) => self.thrown(&error.0),
                }
            }
            "constants" => self.io.log(&Entry::new("constants").num(
                "maxPhysicalSocketBufferedBytes",
                spocky_daemon_relay::encrypted_socket::MAX_PHYSICAL_SOCKET_BUFFERED_BYTES,
            )),
            "socketDefaults" => {
                self.io.defaults = modes_from(&op["modes"], &Modes::default());
            }
            "socketMode" => {
                let socket = SocketId(op["id"].as_u64().unwrap());
                let current = self.io.socket(socket).modes.clone();
                self.io.socket(socket).modes = modes_from(&op["modes"], &current);
            }
            "socketState" => {
                let socket = SocketId(op["id"].as_u64().unwrap());
                if let Some(state) = op["readyState"].as_u64() {
                    self.io.socket(socket).ready_state = u8::try_from(state).unwrap();
                }
                if let Some(buffered) = op["bufferedAmount"].as_u64() {
                    self.io.socket(socket).buffered = buffered;
                }
            }
            "attachMode" => {
                self.io.attach_mode = match op["mode"].as_str().unwrap() {
                    "ok" => AttachMode::Ok,
                    "reject" => AttachMode::Reject,
                    _ => AttachMode::Pending,
                };
            }
            "attachSettle" => {
                if let Some(Some(socket)) = self.io.attach_waiters.pop_front() {
                    let result = if op["result"] == "reject" {
                        Err("attach rejected".to_owned())
                    } else {
                        Ok(())
                    };
                    self.io
                        .reactions
                        .push_back(Reaction::AttachSettled(socket, result));
                }
            }
            "start" => {
                let options = TransportOptions {
                    relay_endpoint: op["endpoint"].as_str().unwrap().to_owned(),
                    relay_use_tls: op["useTls"].as_bool().unwrap(),
                    server_id: op["serverId"].as_str().unwrap().to_owned(),
                    has_daemon_key_pair: op["keyPair"] == true,
                };
                match RelayTransport::start(&mut self.io, options.clone()) {
                    Ok(transport) => {
                        self.transport = Some(transport);
                        self.options = Some(options);
                    }
                    Err(error) => self.thrown(&error.0),
                }
            }
            "stop" => {
                if let Some(transport) = self.transport.as_mut() {
                    transport.stop(&mut self.io);
                }
                self.io.log(&Entry::new("stopped"));
            }
            "advance" => self.advance(op["ms"].as_i64().unwrap()),
            "app.send" | "app.close" | "app.terminate" | "app.read" => self.app_op(op),
            "socket" => self.socket_event(op),
            "channelMode" => {
                self.io.channel_mode = match op["mode"].as_str().unwrap() {
                    "ok" => ChannelMode::Ok,
                    "fail" => ChannelMode::Fail,
                    _ => ChannelMode::Pending,
                };
                if let Some(message) = op["message"].as_str() {
                    message.clone_into(&mut self.io.channel_fail_message);
                }
            }
            "channel" => {
                let socket = self.io.channels
                    [usize::try_from(op["n"].as_u64().unwrap()).unwrap() - 1]
                    .socket;
                if op["result"] == "fail" {
                    let message = op["message"]
                        .as_str()
                        .unwrap_or("handshake failed")
                        .to_owned();
                    self.io
                        .reactions
                        .push_back(Reaction::ChannelFailed(socket, message));
                } else {
                    self.io.reactions.push_back(Reaction::ChannelReady(socket));
                }
            }
            "channelEvent" => {
                let socket = self.io.channels
                    [usize::try_from(op["n"].as_u64().unwrap()).unwrap() - 1]
                    .socket;
                match op["event"].as_str().unwrap() {
                    "message" => {
                        let data = match op["text"].as_str() {
                            Some(text) => Data::Text(text.to_owned()),
                            None => Data::Binary(hex_decode(op["binary"].as_str().unwrap())),
                        };
                        self.with_transport(|transport, io| {
                            transport.on_channel_message(io, socket, data);
                        });
                    }
                    "close" => {
                        let code = u16::try_from(op["code"].as_u64().unwrap()).unwrap();
                        let reason = op["reason"].as_str().unwrap_or_default().to_owned();
                        self.with_transport(|transport, io| {
                            transport.on_channel_close(io, socket, code, &reason);
                        });
                    }
                    _ => {
                        let message = op["message"].as_str().unwrap().to_owned();
                        self.with_transport(|transport, io| {
                            transport.on_channel_error(io, socket, &message);
                        });
                    }
                }
            }
            "runtime" => {
                let config = &op["config"];
                let config = RelayRuntimeConfig {
                    enabled: config["enabled"].as_bool().unwrap(),
                    endpoint: config["endpoint"].as_str().unwrap().to_owned(),
                    public_endpoint: config["publicEndpoint"].as_str().unwrap().to_owned(),
                    use_tls: config["useTls"].as_bool().unwrap(),
                    public_use_tls: config["publicUseTls"].as_bool().unwrap(),
                };
                self.runtime_start_mode.set(match op["startMode"].as_str() {
                    Some("throw") => StartMode::Throw,
                    Some("stop-rejects") => StartMode::StopRejects,
                    _ => StartMode::Ok,
                });
                let starter = RuntimeStarter {
                    log: self.runtime_log.clone(),
                    mode: self.runtime_start_mode.clone(),
                    server_id: op["serverId"].as_str().unwrap().to_owned(),
                };
                match RelayRuntime::new(config, starter) {
                    Ok(runtime) => {
                        self.runtime = Some(runtime);
                        self.flush_runtime_log();
                        let entry = self.runtime_config_entry();
                        self.io.log(&entry);
                    }
                    Err(message) => {
                        self.flush_runtime_log();
                        self.io
                            .log(&Entry::new("driver-error").str("message", &message));
                    }
                }
            }
            "runtimeStartMode" => {
                self.runtime_start_mode
                    .set(match op["mode"].as_str().unwrap() {
                        "throw" => StartMode::Throw,
                        "stop-rejects" => StartMode::StopRejects,
                        _ => StartMode::Ok,
                    });
            }
            "setEnabled" => {
                let enabled = op["enabled"].as_bool().unwrap();
                let result = self.runtime.as_mut().unwrap().set_enabled(enabled);
                self.flush_runtime_log();
                let effects = match result {
                    Ok(effects) => effects,
                    Err(message) => {
                        self.thrown(&message);
                        Vec::new()
                    }
                };
                let entry = self.runtime_config_entry();
                self.io.log(&entry);
                self.runtime_effects(effects);
            }
            "runtimeStop" => {
                let result = self.runtime.as_mut().unwrap().stop();
                self.flush_runtime_log();
                match result {
                    Ok(()) => self.io.log(&Entry::new("runtime").str("a", "stopped")),
                    Err(message) => self.thrown(&message),
                }
            }
            other => panic!("unsupported operation {other}"),
        }
        self.drain();
        std::mem::take(&mut self.io.entries)
    }
}

fn modes_from(value: &Value, base: &Modes) -> Modes {
    let mut modes = base.clone();
    for (key, mode) in value.as_object().into_iter().flatten() {
        let throws = mode == "throw";
        match key.as_str() {
            "send" => modes.send_throws = throws,
            "close" => modes.close_throws = throws,
            "terminate" => modes.terminate_throws = throws,
            "ping" => modes.ping_throws = throws,
            _ => {}
        }
    }
    modes
}
