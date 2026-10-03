//! Port of the pinned relay `encrypted-channel.ts` (Paseo 5de45e2).
//!
//! The channel is a sans-IO state machine. The runtime that owns the
//! WebSocket feeds it transport input (`handle_message`, `handle_close`,
//! `handle_error`), settles transport sends it left pending
//! (`settle_send`), and fires the client hello retry every
//! [`HANDSHAKE_RETRY_MS`] while [`EncryptedChannel::retry_active`] holds.
//! Event callbacks run inline, in the order the original invokes them, and
//! receive a [`ChannelControl`] so they can send or close re-entrantly the
//! way original callbacks can.
//!
//! Each original `await` on a transport send maps to [`SendStatus`]: a send
//! that settles at once continues immediately, and a pending send parks its
//! continuation until `settle_send`. A synchronous throw and an immediate
//! rejection are the same `Failed` status because the original handles
//! both identically. Every input is processed to completion before the next
//! one, which is the original order when each input arrives in its own task.
//! The original still yields a microtask after a send that settles at once,
//! so frames a transport delivers synchronously within that same task are
//! interleaved differently there (a daemon buffers them behind its ready
//! frame and drops buffered hello and ready frames). A runtime that reports
//! socket writes as `Pending` until they complete, as the pinned daemon
//! transport does, sees the original buffering exactly.
//!
//! Known baseline defects are reproduced, not fixed:
//! - no replay or reordering protection within a live session: any frame
//!   that authenticates is delivered, however often and in whatever order;
//! - at most [`MAX_PENDING_SENDS`] sends queue during the client handshake,
//!   and each send past that silently drops the oldest queued one;
//! - a decryption or protocol failure closes the transport with 1011 but
//!   leaves the channel open, so later valid frames are still delivered;
//! - a daemon re-hello whose key import, key derivation, close, or ready
//!   send fails falls through to ciphertext decoding of the hello text,
//!   unless the error message contains `plaintext frame`, which closes the
//!   transport with 1011 and that message instead, but only when the failure
//!   is an `Error` instance (a send rejected with a string is
//!   [`SendStatus::FailedNonError`] and falls through); a `{` frame whose
//!   `JSON.parse` error quotes `plaintext frame` closes the same way;
//! - a daemon hello whose key is rejected leaves every later frame
//!   buffered and never delivered.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fmt, mem,
};

use rand_core::{OsRng, RngCore};

use crate::{
    CryptoError, KEY_LENGTH, KeyPair, NONCE_LENGTH, SharedKey,
    base64_js::{Base64JsError, array_buffer_to_base64, base64_to_array_buffer},
    decrypt, derive_shared_key, encrypt_with_nonce, export_public_key, import_public_key,
    js_json::{self, JsonValue},
    js_string::{
        JsString, collapse_whitespace, decode_utf8_fatal, decode_utf8_lossy, json_quote, trim,
        utf16,
    },
    key_pair_from_secret,
};

/// Interval of the client `e2ee_hello` retry timer, in milliseconds.
pub const HANDSHAKE_RETRY_MS: u64 = 1000;
/// Sends queued while the client handshake is in progress.
pub const MAX_PENDING_SENDS: usize = 200;
/// Close code for a daemon re-hello that changes the client key.
pub const REHANDSHAKE_REJECTION_CODE: u16 = 1008;
/// Close reason for a daemon re-hello that changes the client key.
pub const REHANDSHAKE_KEY_MISMATCH_CLOSE_REASON: &str = "E2EE re-handshake key mismatch";
/// Close code for decryption, protocol, and backlog flush failures.
pub const PROTOCOL_ERROR_CLOSE_CODE: u16 = 1011;
/// Default `close` code of the original channel.
pub const NORMAL_CLOSURE_CODE: u16 = 1000;
/// Default `close` reason of the original channel.
pub const NORMAL_CLOSURE_REASON: &str = "Normal closure";
/// Nonce plus authentication tag added to every plaintext.
pub const ENCRYPTED_PAYLOAD_OVERHEAD_BYTES: u64 = 40;

const PLAINTEXT_FRAME_MESSAGE: &str = "Received plaintext frame on encrypted channel";
/// The original rethrows any error caught while checking for plaintext
/// handshake traffic whose message contains this text.
const RETHROWN_ERROR_TEXT: &str = "plaintext frame";
const INVALID_HELLO_PREVIEW_LIMIT: usize = 160;
const INVALID_HELLO_PREVIEW_KEEP: usize = 157;

/// `base64EncryptedWireByteLength`.
#[must_use]
pub const fn base64_encrypted_wire_byte_length(plaintext_bytes: u64) -> u64 {
    4 * (plaintext_bytes + ENCRYPTED_PAYLOAD_OVERHEAD_BYTES).div_ceil(3)
}

/// `maxBase64EncryptedPlaintextByteLength`. Negative when no plaintext fits.
#[must_use]
pub fn max_base64_encrypted_plaintext_byte_length(wire_bytes: u64) -> i128 {
    i128::from(wire_bytes / 4) * 3 - i128::from(ENCRYPTED_PAYLOAD_OVERHEAD_BYTES)
}

/// A frame or application payload: a JavaScript `string` or `ArrayBuffer`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Data {
    Text(String),
    Binary(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
            Self::Binary(bytes) => bytes,
        }
    }
}

/// `TransportMessage`: frame data and the WebSocket opcode it arrived with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportMessage {
    pub data: Data,
    pub is_binary: bool,
}

impl TransportMessage {
    /// A text frame.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            data: Data::Text(text.into()),
            is_binary: false,
        }
    }

    /// A binary frame.
    #[must_use]
    pub fn binary(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            data: Data::Binary(bytes.into()),
            is_binary: true,
        }
    }
}

/// An error raised by the transport, carrying its message text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for TransportError {}

/// Identifies a pending transport send. Unique among unsettled sends.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SendId(pub u64);

/// Outcome of `Transport::send`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SendStatus {
    /// The send completed (the original returned `undefined` or resolved).
    Sent,
    /// The send threw or rejected with an `Error`.
    Failed(TransportError),
    /// The send rejected with a value that is not an `Error`, such as a
    /// string. It carries `String(value)`. Everywhere but one place the
    /// original treats it like [`SendStatus::Failed`]; the exception is the
    /// daemon re-hello, which rethrows only `Error` instances.
    FailedNonError(String),
    /// The send settles later through `EncryptedChannel::settle_send`.
    Pending(SendId),
}

/// The WebSocket-like transport under the channel.
pub trait Transport {
    /// Sends a text or binary frame.
    fn send(&mut self, data: Data) -> SendStatus;

    /// Closes the transport.
    ///
    /// # Errors
    ///
    /// Returns the error a throwing `close` raises.
    fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError>;
}

/// Outcome of an application send.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppSend {
    /// The send resolved or rejected.
    Settled(Result<(), ChannelError>),
    /// The transport send is pending; `settle_send` returns its result.
    Pending(SendId),
}

/// `ChannelState`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelState {
    Connecting,
    Handshaking,
    Open,
    Closed,
}

/// `EncryptedChannelOptions`.
#[derive(Clone, Default)]
pub struct ChannelOptions {
    /// Lets an open channel answer repeated `e2ee_hello` frames.
    pub daemon_key_pair: Option<KeyPair>,
    pub binary_ciphertext: bool,
}

/// Errors the channel reports, with the original message texts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelError {
    Crypto(CryptoError),
    Transport(TransportError),
    /// `buildInvalidHelloError`. `received_type` keeps any lone surrogate;
    /// `preview` is already `JSON.stringify` output.
    InvalidHello {
        received_type: JsString,
        has_key: bool,
        preview: String,
    },
    PlaintextFrame,
    NotOpen,
    BinaryFrameWithoutBytes,
    InvalidUtf8,
    Base64(Base64JsError),
    ClosedDuringHandshake {
        code: u16,
        reason: String,
    },
}

impl ChannelError {
    /// The exact JavaScript `message`, as UTF-16 code units.
    #[must_use]
    pub fn message(&self) -> JsString {
        match self {
            Self::InvalidHello {
                received_type,
                has_key,
                preview,
            } => {
                let mut message = utf16("Invalid hello message (receivedType=");
                message.extend_from_slice(received_type);
                message.extend(utf16(&format!(", hasKey={has_key}, preview={preview})")));
                message
            }
            other => utf16(&other.to_string()),
        }
    }
}

impl fmt::Display for ChannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Crypto(error) => error.fmt(formatter),
            Self::Transport(error) => error.fmt(formatter),
            Self::InvalidHello { .. } => {
                formatter.write_str(&String::from_utf16_lossy(&self.message()))
            }
            Self::PlaintextFrame => formatter.write_str(PLAINTEXT_FRAME_MESSAGE),
            Self::NotOpen => formatter.write_str("Channel not open"),
            Self::BinaryFrameWithoutBytes => {
                formatter.write_str("Binary WebSocket frame did not contain bytes")
            }
            Self::InvalidUtf8 => {
                formatter.write_str("The encoded data was not valid for encoding utf-8")
            }
            Self::Base64(error) => error.fmt(formatter),
            Self::ClosedDuringHandshake { code, reason } => {
                write!(
                    formatter,
                    "Connection closed during handshake: {code} {reason}"
                )
            }
        }
    }
}

impl Error for ChannelError {}

/// What an event callback may do with the channel that raised it.
pub trait ChannelControl {
    /// `send`.
    fn send(&mut self, data: Data) -> AppSend;

    /// `close`.
    ///
    /// # Errors
    ///
    /// Returns the error a throwing transport `close` raises.
    fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError>;

    /// `isOpen`.
    fn is_open(&self) -> bool;

    /// `outboundWireByteLength`.
    fn outbound_wire_byte_length(&self, data: &Data) -> u64;
}

/// `EncryptedChannelEvents`. Every callback is optional.
pub trait ChannelEvents {
    fn on_open(&mut self, _channel: &mut dyn ChannelControl) {}
    fn on_message(&mut self, _channel: &mut dyn ChannelControl, _data: Data) {}
    fn on_close(&mut self, _channel: &mut dyn ChannelControl, _code: u16, _reason: &str) {}
    fn on_error(&mut self, _channel: &mut dyn ChannelControl, _error: &ChannelError) {}
}

impl ChannelEvents for () {}

/// Where the channel stands relative to the original object graph.
enum Phase {
    /// `createDaemonChannel` waiting for a hello (`onmessage = handleHello`).
    AwaitingHello,
    /// The ready frame is in flight; frames are buffered (`bufferNext`).
    ReadyPending {
        shared_key: SharedKey,
        binary_ciphertext: bool,
        buffered: Vec<TransportMessage>,
    },
    /// The daemon handshake failed after `bufferNext` was installed, so
    /// frames are buffered forever.
    Buffering { buffered: Vec<TransportMessage> },
    /// The `EncryptedChannel` object exists.
    Channel,
}

/// Work the original resumes when a pending transport send settles.
enum Continuation {
    AppSend,
    ClientHello,
    Flush(VecDeque<Data>),
    Rehello(TransportMessage),
    DaemonReady,
}

enum SendStep {
    Done,
    Failed(ChannelError),
    Pending(SendId),
}

enum RehelloStep {
    Done,
    /// The re-hello threw. `is_error` is false for a rejection value that is
    /// not an `Error`, which the original never rethrows.
    Failed {
        message: String,
        is_error: bool,
    },
    Pending(SendId),
}

impl RehelloStep {
    fn error(message: String) -> Self {
        Self::Failed {
            message,
            is_error: true,
        }
    }
}

/// The `createClientChannel` closure state.
struct ClientHello {
    text: String,
    retry_active: bool,
}

struct Core<T> {
    transport: T,
    rng: Box<dyn RngCore + Send>,
    phase: Phase,
    handshake: Option<Result<(), ChannelError>>,
    shared_key: SharedKey,
    state: ChannelState,
    options: ChannelOptions,
    pending_sends: VecDeque<Data>,
    client_hello: Option<ClientHello>,
    continuations: BTreeMap<SendId, Continuation>,
}

impl<T: Transport> Core<T> {
    /// `send` up to its `await`.
    fn send_data(&mut self, data: Data) -> SendStep {
        if !matches!(self.phase, Phase::Channel) {
            return SendStep::Failed(ChannelError::NotOpen);
        }
        match self.state {
            ChannelState::Handshaking => {
                if self.pending_sends.len() >= MAX_PENDING_SENDS {
                    self.pending_sends.pop_front();
                }
                self.pending_sends.push_back(data);
                SendStep::Done
            }
            ChannelState::Open => {
                let mut nonce = [0_u8; NONCE_LENGTH];
                self.rng.fill_bytes(&mut nonce);
                let bundle = match encrypt_with_nonce(&self.shared_key, &nonce, data.bytes()) {
                    Ok(bundle) => bundle,
                    Err(error) => return SendStep::Failed(ChannelError::Crypto(error)),
                };
                let frame = if self.options.binary_ciphertext && matches!(data, Data::Binary(_)) {
                    Data::Binary(bundle)
                } else {
                    Data::Text(array_buffer_to_base64(&bundle))
                };
                match self.transport.send(frame) {
                    SendStatus::Sent => SendStep::Done,
                    SendStatus::Failed(error) => SendStep::Failed(ChannelError::Transport(error)),
                    SendStatus::FailedNonError(message) => {
                        SendStep::Failed(ChannelError::Transport(TransportError(message)))
                    }
                    SendStatus::Pending(id) => SendStep::Pending(id),
                }
            }
            ChannelState::Connecting | ChannelState::Closed => {
                SendStep::Failed(ChannelError::NotOpen)
            }
        }
    }

    fn settle_handshake(&mut self, result: Result<(), ChannelError>) {
        if self.handshake.is_none() {
            self.handshake = Some(result);
        }
    }

    /// `transport.close(1011, err.message)` inside a swallowing `catch`.
    fn close_for_error(&mut self, error: &ChannelError) {
        self.close_with_reason(&error.to_string());
    }

    fn close_with_reason(&mut self, reason: &str) {
        let _ignored = self.transport.close(PROTOCOL_ERROR_CLOSE_CODE, reason);
    }

    /// The catch around the plaintext handshake check: an error whose
    /// message contains `plaintext frame` is rethrown and closes the
    /// transport with 1011 and that message. Returns true when it did.
    fn rethrow_closes(&mut self, message: &[u16]) -> bool {
        let marker = utf16(RETHROWN_ERROR_TEXT);
        if !message.windows(marker.len()).any(|window| window == marker) {
            return false;
        }
        // A lone surrogate reaches the WebSocket close frame as U+FFFD.
        self.close_with_reason(&String::from_utf16_lossy(message));
        true
    }

    /// The ciphertext half of `handleMessage`, after any handshake check.
    fn open_ciphertext(&self, message: &TransportMessage) -> Result<Data, ChannelError> {
        let (bytes, is_binary) = if self.options.binary_ciphertext {
            if message.is_binary {
                (require_bytes(&message.data)?, Some(true))
            } else {
                (decode_base64(&message.data)?, Some(false))
            }
        } else if !message.is_binary {
            (decode_base64(&message.data)?, None)
        } else {
            // Older transport adapters could lose the opcode, so the legacy
            // path tries base64 first.
            match decode_base64(&message.data) {
                Ok(bytes) => (bytes, None),
                Err(_) => (require_bytes(&message.data)?, None),
            }
        };
        let plaintext = decrypt(&self.shared_key, &bytes).map_err(ChannelError::Crypto)?;
        match is_binary {
            Some(true) => Ok(Data::Binary(plaintext)),
            Some(false) => decode_utf8_fatal(&plaintext)
                .map(Data::Text)
                .map_err(|_| ChannelError::InvalidUtf8),
            None => Ok(decode_utf8_fatal(&plaintext).map_or(Data::Binary(plaintext), Data::Text)),
        }
    }

    /// `handleDaemonRehello` up to its `await`.
    fn rehello(&mut self, hello: &JsonValue) -> RehelloStep {
        let Some(daemon_key_pair) = &self.options.daemon_key_pair else {
            return RehelloStep::Done;
        };
        let client_public_key = match hello.get("key") {
            Some(JsonValue::String(key)) => String::from_utf16(key)
                .map_err(|_| CryptoError::InvalidPublicKeyEncoding)
                .and_then(|key| import_public_key(&key)),
            _ => Err(CryptoError::InvalidPublicKeyEncoding),
        };
        let retry_key = match client_public_key.and_then(|client_public_key| {
            derive_shared_key(&daemon_key_pair.secret_key, &client_public_key)
        }) {
            Ok(retry_key) => retry_key,
            Err(error) => return RehelloStep::error(error.to_string()),
        };
        if !keys_equal(&retry_key, &self.shared_key) {
            self.state = ChannelState::Closed;
            return match self.transport.close(
                REHANDSHAKE_REJECTION_CODE,
                REHANDSHAKE_KEY_MISMATCH_CLOSE_REASON,
            ) {
                Ok(()) => RehelloStep::Done,
                Err(error) => RehelloStep::error(error.0),
            };
        }
        let ready = ready_frame(self.options.binary_ciphertext);
        match self.transport.send(Data::Text(ready)) {
            SendStatus::Sent => RehelloStep::Done,
            SendStatus::Failed(error) => RehelloStep::error(error.0),
            SendStatus::FailedNonError(message) => RehelloStep::Failed {
                message,
                is_error: false,
            },
            SendStatus::Pending(id) => RehelloStep::Pending(id),
        }
    }
}

impl<T: Transport> ChannelControl for Core<T> {
    fn send(&mut self, data: Data) -> AppSend {
        match self.send_data(data) {
            SendStep::Done => AppSend::Settled(Ok(())),
            SendStep::Failed(error) => AppSend::Settled(Err(error)),
            SendStep::Pending(id) => {
                self.continuations.insert(id, Continuation::AppSend);
                AppSend::Pending(id)
            }
        }
    }

    fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError> {
        self.state = ChannelState::Closed;
        self.transport.close(code, reason)
    }

    fn is_open(&self) -> bool {
        matches!(self.phase, Phase::Channel) && self.state == ChannelState::Open
    }

    fn outbound_wire_byte_length(&self, data: &Data) -> u64 {
        let plaintext_bytes = data.bytes().len() as u64;
        if self.options.binary_ciphertext && matches!(data, Data::Binary(_)) {
            plaintext_bytes + ENCRYPTED_PAYLOAD_OVERHEAD_BYTES
        } else {
            base64_encrypted_wire_byte_length(plaintext_bytes)
        }
    }
}

/// The encrypted channel, covering `createClientChannel`,
/// `createDaemonChannel`, and the `EncryptedChannel` class.
pub struct EncryptedChannel<T: Transport, E: ChannelEvents> {
    core: Core<T>,
    events: E,
}

impl<T: Transport, E: ChannelEvents> EncryptedChannel<T, E> {
    /// `new EncryptedChannel(transport, sharedKey, events, options)`: the
    /// channel starts in the handshaking state.
    pub fn new<R: RngCore + Send + 'static>(
        transport: T,
        shared_key: SharedKey,
        events: E,
        options: ChannelOptions,
        rng: R,
    ) -> Self {
        Self::from_parts(
            transport,
            shared_key,
            events,
            options,
            Box::new(rng),
            Phase::Channel,
        )
    }

    /// `createClientChannel` with the operating system random source.
    ///
    /// # Errors
    ///
    /// Returns the key import or derivation error for the daemon key.
    pub fn client(transport: T, daemon_public_key: &str, events: E) -> Result<Self, ChannelError> {
        Self::client_with_rng(transport, daemon_public_key, events, OsRng)
    }

    /// `createClientChannel` drawing its key pair and nonces from `rng`.
    ///
    /// # Errors
    ///
    /// Returns the key import or derivation error for the daemon key.
    pub fn client_with_rng<R: RngCore + Send + 'static>(
        transport: T,
        daemon_public_key: &str,
        events: E,
        rng: R,
    ) -> Result<Self, ChannelError> {
        let mut rng: Box<dyn RngCore + Send> = Box::new(rng);
        let mut secret_key = [0_u8; KEY_LENGTH];
        rng.fill_bytes(&mut secret_key);
        let key_pair = key_pair_from_secret(secret_key);
        let daemon_public_key =
            import_public_key(daemon_public_key).map_err(ChannelError::Crypto)?;
        let shared_key = derive_shared_key(&key_pair.secret_key, &daemon_public_key)
            .map_err(ChannelError::Crypto)?;
        let mut channel = Self::from_parts(
            transport,
            shared_key,
            events,
            ChannelOptions::default(),
            rng,
            Phase::Channel,
        );
        let key = export_public_key(&key_pair.public_key).map_err(ChannelError::Crypto)?;
        channel.core.client_hello = Some(ClientHello {
            text: format!(
                r#"{{"type":"e2ee_hello","key":{},"capabilities":{{"binaryCiphertext":true}}}}"#,
                json_quote(&utf16(&key))
            ),
            retry_active: false,
        });
        channel.send_hello();
        if let Some(hello) = &mut channel.core.client_hello {
            hello.retry_active = true;
        }
        Ok(channel)
    }

    /// `createDaemonChannel` with the operating system random source. The
    /// handshake outcome appears in [`Self::handshake_result`].
    pub fn daemon(transport: T, daemon_key_pair: KeyPair, events: E) -> Self {
        Self::daemon_with_rng(transport, daemon_key_pair, events, OsRng)
    }

    /// `createDaemonChannel` drawing its nonces from `rng`.
    pub fn daemon_with_rng<R: RngCore + Send + 'static>(
        transport: T,
        daemon_key_pair: KeyPair,
        events: E,
        rng: R,
    ) -> Self {
        Self::from_parts(
            transport,
            [0; KEY_LENGTH],
            events,
            ChannelOptions {
                daemon_key_pair: Some(daemon_key_pair),
                binary_ciphertext: false,
            },
            Box::new(rng),
            Phase::AwaitingHello,
        )
    }

    fn from_parts(
        transport: T,
        shared_key: SharedKey,
        events: E,
        options: ChannelOptions,
        rng: Box<dyn RngCore + Send>,
        phase: Phase,
    ) -> Self {
        Self {
            core: Core {
                transport,
                rng,
                phase,
                handshake: None,
                shared_key,
                state: ChannelState::Handshaking,
                options,
                pending_sends: VecDeque::new(),
                client_hello: None,
                continuations: BTreeMap::new(),
            },
            events,
        }
    }

    /// The settled `createDaemonChannel` promise: `None` while pending. The
    /// first settlement wins, as with a promise.
    #[must_use]
    pub const fn handshake_result(&self) -> Option<&Result<(), ChannelError>> {
        self.core.handshake.as_ref()
    }

    /// `setState`.
    pub const fn set_state(&mut self, state: ChannelState) {
        self.core.state = state;
    }

    /// `isOpen`.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.core.is_open()
    }

    /// `send`. Before a daemon channel exists it fails as not open.
    pub fn send(&mut self, data: Data) -> AppSend {
        self.core.send(data)
    }

    /// `close`; the original defaults are [`NORMAL_CLOSURE_CODE`] and
    /// [`NORMAL_CLOSURE_REASON`].
    ///
    /// # Errors
    ///
    /// Returns the error a throwing transport `close` raises.
    pub fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError> {
        self.core.close(code, reason)
    }

    /// `outboundWireByteLength`.
    #[must_use]
    pub fn outbound_wire_byte_length(&self, data: &Data) -> u64 {
        self.core.outbound_wire_byte_length(data)
    }

    /// True while the client hello retry interval is armed.
    #[must_use]
    pub fn retry_active(&self) -> bool {
        self.core
            .client_hello
            .as_ref()
            .is_some_and(|hello| hello.retry_active)
    }

    /// The transport.
    pub const fn transport(&self) -> &T {
        &self.core.transport
    }

    /// The transport, mutably.
    pub const fn transport_mut(&mut self) -> &mut T {
        &mut self.core.transport
    }

    /// The event handler.
    pub const fn events(&self) -> &E {
        &self.events
    }

    /// The event handler, mutably.
    pub const fn events_mut(&mut self) -> &mut E {
        &mut self.events
    }

    /// `transport.onmessage`.
    pub fn handle_message(&mut self, message: TransportMessage) {
        match &mut self.core.phase {
            Phase::AwaitingHello => self.daemon_hello(&message),
            Phase::ReadyPending { buffered, .. } | Phase::Buffering { buffered } => {
                buffered.push(message);
            }
            Phase::Channel => match self.core.state {
                ChannelState::Handshaking => self.handshaking_message(&message),
                ChannelState::Open => self.open_message(message),
                ChannelState::Connecting | ChannelState::Closed => {}
            },
        }
    }

    /// `transport.onclose`.
    pub fn handle_close(&mut self, code: u16, reason: &str) {
        if matches!(self.core.phase, Phase::Channel) {
            self.core.state = ChannelState::Closed;
            self.events.on_close(&mut self.core, code, reason);
            if let Some(hello) = &mut self.core.client_hello {
                hello.retry_active = false;
            }
        } else {
            self.core
                .settle_handshake(Err(ChannelError::ClosedDuringHandshake {
                    code,
                    reason: reason.to_owned(),
                }));
        }
    }

    /// `transport.onerror`.
    pub fn handle_error(&mut self, error: TransportError) {
        let error = ChannelError::Transport(error);
        if matches!(self.core.phase, Phase::Channel) {
            self.events.on_error(&mut self.core, &error);
        } else {
            self.core.settle_handshake(Err(error));
        }
    }

    /// Settles a send the transport reported as pending and resumes the
    /// original continuation. Returns the result of an application send.
    pub fn settle_send(
        &mut self,
        id: SendId,
        result: Result<(), TransportError>,
    ) -> Option<Result<(), ChannelError>> {
        self.settle(id, result, true)
    }

    /// Settles a pending send that rejected with a value that is not an
    /// `Error` (see [`SendStatus::FailedNonError`]).
    pub fn settle_send_non_error(
        &mut self,
        id: SendId,
        message: String,
    ) -> Option<Result<(), ChannelError>> {
        self.settle(id, Err(TransportError(message)), false)
    }

    fn settle(
        &mut self,
        id: SendId,
        result: Result<(), TransportError>,
        is_error: bool,
    ) -> Option<Result<(), ChannelError>> {
        match self.core.continuations.remove(&id)? {
            Continuation::AppSend => return Some(result.map_err(ChannelError::Transport)),
            Continuation::ClientHello => {
                if let Err(error) = result {
                    self.events
                        .on_error(&mut self.core, &ChannelError::Transport(error));
                }
            }
            Continuation::Flush(queue) => match result {
                Ok(()) => self.run_flush(queue),
                Err(error) => self.fail_flush(&ChannelError::Transport(error)),
            },
            Continuation::Rehello(message) => {
                if let Err(error) = result
                    && !(is_error && self.core.rethrow_closes(&utf16(&error.0)))
                {
                    self.deliver_ciphertext(&message);
                }
            }
            Continuation::DaemonReady => {
                let phase = mem::replace(&mut self.core.phase, Phase::Channel);
                let Phase::ReadyPending {
                    shared_key,
                    binary_ciphertext,
                    buffered,
                } = phase
                else {
                    self.core.phase = phase;
                    return None;
                };
                match result {
                    Ok(()) => self.open_daemon_channel(shared_key, binary_ciphertext, buffered),
                    Err(error) => {
                        self.core.phase = Phase::Buffering { buffered };
                        self.core
                            .settle_handshake(Err(ChannelError::Transport(error)));
                    }
                }
            }
        }
        None
    }

    /// One firing of the client hello retry interval.
    pub fn retry_tick(&mut self) {
        let Some(hello) = &mut self.core.client_hello else {
            return;
        };
        if !hello.retry_active {
            return;
        }
        if self.core.state == ChannelState::Open {
            hello.retry_active = false;
            return;
        }
        self.send_hello();
    }

    fn send_hello(&mut self) {
        let Some(hello) = &self.core.client_hello else {
            return;
        };
        match self.core.transport.send(Data::Text(hello.text.clone())) {
            SendStatus::Sent => {}
            SendStatus::Failed(error) => {
                self.events
                    .on_error(&mut self.core, &ChannelError::Transport(error));
            }
            // `emitSendError` wraps any rejection value in an `Error`.
            SendStatus::FailedNonError(message) => {
                self.events.on_error(
                    &mut self.core,
                    &ChannelError::Transport(TransportError(message)),
                );
            }
            SendStatus::Pending(id) => {
                self.core
                    .continuations
                    .insert(id, Continuation::ClientHello);
            }
        }
    }

    /// `handleHello`.
    fn daemon_hello(&mut self, message: &TransportMessage) {
        // Only `createDaemonChannel` awaits a hello, and it has a key pair.
        let Some(daemon_key_pair) = self.core.options.daemon_key_pair.clone() else {
            return;
        };
        if message.is_binary {
            self.core
                .settle_handshake(Err(invalid_hello(&utf16("<binary frame>"), None)));
            return;
        }
        let text = decode_transport_text(&message.data);
        let Some(parsed) = js_json::parse(&text) else {
            self.core
                .settle_handshake(Err(invalid_hello(&utf16(&text), None)));
            return;
        };
        if !is_hello(&parsed) {
            self.core
                .settle_handshake(Err(invalid_hello(&utf16(&text), Some(&parsed))));
            return;
        }

        // From here the original buffers every frame (`bufferNext`).
        let shared_key = match hello_shared_key(&parsed, &daemon_key_pair) {
            Ok(shared_key) => shared_key,
            Err(error) => {
                self.core.phase = Phase::Buffering {
                    buffered: Vec::new(),
                };
                self.core.settle_handshake(Err(error));
                return;
            }
        };
        let binary_ciphertext = supports_binary_ciphertext(&parsed);
        match self
            .core
            .transport
            .send(Data::Text(ready_frame(binary_ciphertext)))
        {
            SendStatus::Sent => {
                self.open_daemon_channel(shared_key, binary_ciphertext, Vec::new());
            }
            SendStatus::Failed(error) => {
                self.core.phase = Phase::Buffering {
                    buffered: Vec::new(),
                };
                self.core
                    .settle_handshake(Err(ChannelError::Transport(error)));
            }
            SendStatus::FailedNonError(message) => {
                self.core.phase = Phase::Buffering {
                    buffered: Vec::new(),
                };
                self.core
                    .settle_handshake(Err(ChannelError::Transport(TransportError(message))));
            }
            SendStatus::Pending(id) => {
                self.core.phase = Phase::ReadyPending {
                    shared_key,
                    binary_ciphertext,
                    buffered: Vec::new(),
                };
                self.core
                    .continuations
                    .insert(id, Continuation::DaemonReady);
            }
        }
    }

    /// The tail of `handleHello` once the ready frame is sent.
    fn open_daemon_channel(
        &mut self,
        shared_key: SharedKey,
        binary_ciphertext: bool,
        buffered: Vec<TransportMessage>,
    ) {
        self.core.shared_key = shared_key;
        self.core.options.binary_ciphertext = binary_ciphertext;
        self.core.phase = Phase::Channel;
        self.core.state = ChannelState::Open;
        self.events.on_open(&mut self.core);
        for message in buffered {
            if !should_ignore_post_hello_plaintext(&message) {
                self.handle_message(message);
            }
        }
        self.core.settle_handshake(Ok(()));
    }

    /// `handleMessage` in the handshaking state.
    fn handshaking_message(&mut self, message: &TransportMessage) {
        if message.is_binary {
            return;
        }
        let text = decode_transport_text(&message.data);
        let Some(parsed) = js_json::parse(&text) else {
            return;
        };
        if !is_ready(&parsed) {
            return;
        }
        self.core.options.binary_ciphertext = supports_binary_ciphertext(&parsed);
        self.core.state = ChannelState::Open;
        self.events.on_open(&mut self.core);
        if let Some(hello) = &mut self.core.client_hello {
            hello.retry_active = false;
        }
        // `flushPendingSends`.
        if self.core.state != ChannelState::Open {
            return;
        }
        let queue = mem::take(&mut self.core.pending_sends);
        self.run_flush(queue);
    }

    fn run_flush(&mut self, mut queue: VecDeque<Data>) {
        while let Some(item) = queue.pop_front() {
            match self.core.send_data(item) {
                SendStep::Done => {}
                SendStep::Failed(error) => {
                    self.fail_flush(&error);
                    return;
                }
                SendStep::Pending(id) => {
                    self.core
                        .continuations
                        .insert(id, Continuation::Flush(queue));
                    return;
                }
            }
        }
    }

    fn fail_flush(&mut self, error: &ChannelError) {
        self.events.on_error(&mut self.core, error);
        self.core.state = ChannelState::Closed;
        self.core.close_for_error(error);
    }

    /// `handleMessage` in the open state.
    fn open_message(&mut self, message: TransportMessage) {
        let parsed = if message.is_binary {
            None
        } else {
            let text = decode_transport_text(&message.data);
            if trim(&utf16(&text)).first() == Some(&u16::from(b'{')) {
                match js_json::parse_detailed(&text) {
                    Ok(parsed) => Some(parsed),
                    Err(error) => {
                        // V8 quotes the source in some parse errors, so a
                        // frame can trigger the rethrow by its own text.
                        if let Some(reason) = error.unexpected_token_message()
                            && self.core.rethrow_closes(reason)
                        {
                            return;
                        }
                        None
                    }
                }
            } else {
                None
            }
        };
        if let Some(parsed) = parsed {
            if is_hello(&parsed) {
                if self.core.options.daemon_key_pair.is_none() {
                    return;
                }
                match self.core.rehello(&parsed) {
                    RehelloStep::Done => return,
                    RehelloStep::Pending(id) => {
                        self.core
                            .continuations
                            .insert(id, Continuation::Rehello(message));
                        return;
                    }
                    RehelloStep::Failed { message, is_error } => {
                        if is_error && self.core.rethrow_closes(&utf16(&message)) {
                            return;
                        }
                        // Otherwise a failed re-hello falls through to
                        // ciphertext decoding.
                    }
                }
            } else if is_ready(&parsed) {
                return;
            } else {
                self.core.close_for_error(&ChannelError::PlaintextFrame);
                return;
            }
        }
        self.deliver_ciphertext(&message);
    }

    fn deliver_ciphertext(&mut self, message: &TransportMessage) {
        match self.core.open_ciphertext(message) {
            Ok(plaintext) => self.events.on_message(&mut self.core, plaintext),
            Err(error) => self.core.close_for_error(&error),
        }
    }
}

/// The key checks of `handleHello`: `importPublicKey` then `deriveSharedKey`.
fn hello_shared_key(
    hello: &JsonValue,
    daemon_key_pair: &KeyPair,
) -> Result<SharedKey, ChannelError> {
    let key = match hello.get("key") {
        Some(JsonValue::String(key)) => String::from_utf16(key)
            .map_err(|_| ChannelError::Crypto(CryptoError::InvalidPublicKeyEncoding))?,
        _ => return Err(ChannelError::Crypto(CryptoError::InvalidPublicKeyEncoding)),
    };
    let client_public_key = import_public_key(&key).map_err(ChannelError::Crypto)?;
    derive_shared_key(&daemon_key_pair.secret_key, &client_public_key).map_err(ChannelError::Crypto)
}

fn decode_transport_text(data: &Data) -> String {
    match data {
        Data::Text(text) => text.clone(),
        Data::Binary(bytes) => decode_utf8_lossy(bytes),
    }
}

fn decode_base64(data: &Data) -> Result<Vec<u8>, ChannelError> {
    base64_to_array_buffer(&decode_transport_text(data)).map_err(ChannelError::Base64)
}

fn require_bytes(data: &Data) -> Result<Vec<u8>, ChannelError> {
    match data {
        Data::Binary(bytes) => Ok(bytes.clone()),
        Data::Text(_) => Err(ChannelError::BinaryFrameWithoutBytes),
    }
}

fn ready_frame(binary_ciphertext: bool) -> String {
    if binary_ciphertext {
        r#"{"type":"e2ee_ready","capabilities":{"binaryCiphertext":true}}"#.to_owned()
    } else {
        r#"{"type":"e2ee_ready"}"#.to_owned()
    }
}

fn is_string(value: Option<&JsonValue>, expected: &str) -> bool {
    matches!(value, Some(JsonValue::String(units)) if units.iter().copied().eq(expected.encode_utf16()))
}

/// `isE2EECapabilities`.
fn capabilities_valid(message: &JsonValue) -> bool {
    match message.get("capabilities") {
        None => true,
        Some(capabilities) => {
            capabilities.is_record()
                && matches!(
                    capabilities.get("binaryCiphertext"),
                    None | Some(JsonValue::Bool(_))
                )
        }
    }
}

/// `isE2EEHelloMessage`.
fn is_hello(value: &JsonValue) -> bool {
    value.is_record()
        && is_string(value.get("type"), "e2ee_hello")
        && matches!(value.get("key"), Some(JsonValue::String(key)) if !trim(key).is_empty())
        && capabilities_valid(value)
}

/// `isE2EEReadyMessage`.
fn is_ready(value: &JsonValue) -> bool {
    value.is_record() && is_string(value.get("type"), "e2ee_ready") && capabilities_valid(value)
}

/// `supportsBinaryCiphertext`.
fn supports_binary_ciphertext(message: &JsonValue) -> bool {
    matches!(
        message
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("binaryCiphertext")),
        Some(JsonValue::Bool(true))
    )
}

/// `shouldIgnorePostHelloPlaintext`.
fn should_ignore_post_hello_plaintext(message: &TransportMessage) -> bool {
    !message.is_binary
        && js_json::parse(&decode_transport_text(&message.data))
            .is_some_and(|parsed| is_hello(&parsed) || is_ready(&parsed))
}

/// `buildInvalidHelloError`.
fn invalid_hello(raw_text: &[u16], parsed: Option<&JsonValue>) -> ChannelError {
    let record = parsed.filter(|value| value.is_record());
    let received_type = match record.and_then(|value| value.get("type")) {
        Some(JsonValue::String(text)) => text.clone(),
        None => utf16("undefined"),
        Some(JsonValue::Bool(_)) => utf16("boolean"),
        Some(JsonValue::Number) => utf16("number"),
        Some(JsonValue::Null | JsonValue::Array | JsonValue::Object(_)) => utf16("object"),
    };
    let has_key = matches!(
        record.and_then(|value| value.get("key")),
        Some(JsonValue::String(key)) if !trim(key).is_empty()
    );
    let collapsed = collapse_whitespace(raw_text);
    let compact = trim(&collapsed);
    let preview = if compact.len() > INVALID_HELLO_PREVIEW_LIMIT {
        let mut preview = compact[..INVALID_HELLO_PREVIEW_KEEP].to_vec();
        preview.extend(utf16("..."));
        json_quote(&preview)
    } else {
        json_quote(compact)
    };
    ChannelError::InvalidHello {
        received_type,
        has_key,
        preview,
    }
}

/// `keysEqual`: a constant-time comparison.
fn keys_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_length_helpers_match_the_original_formulas() {
        assert_eq!(base64_encrypted_wire_byte_length(0), 56);
        assert_eq!(base64_encrypted_wire_byte_length(2), 56);
        assert_eq!(base64_encrypted_wire_byte_length(3), 60);
        assert_eq!(max_base64_encrypted_plaintext_byte_length(56), 2);
        assert_eq!(max_base64_encrypted_plaintext_byte_length(59), 2);
        assert_eq!(max_base64_encrypted_plaintext_byte_length(0), -40);
        for plaintext in 0..64 {
            let wire = base64_encrypted_wire_byte_length(plaintext);
            assert!(max_base64_encrypted_plaintext_byte_length(wire) >= i128::from(plaintext));
        }
    }

    #[test]
    fn invalid_hello_matches_build_invalid_hello_error() {
        let text = utf16(r#"{"type":"invalid"}"#);
        let parsed = js_json::parse(r#"{"type":"invalid"}"#);
        assert_eq!(
            invalid_hello(&text, parsed.as_ref()).to_string(),
            r#"Invalid hello message (receivedType=invalid, hasKey=false, preview="{\"type\":\"invalid\"}")"#
        );
        let long = utf16(&format!("  a\n\n{}  ", "b".repeat(200)));
        let message = invalid_hello(&long, None).to_string();
        assert_eq!(
            message,
            format!(
                "Invalid hello message (receivedType=undefined, hasKey=false, preview=\"a {}...\")",
                "b".repeat(155)
            )
        );
        let surrogate = invalid_hello(
            &[0x7b],
            js_json::parse(r#"{"type":"\ud800","key":" k "}"#).as_ref(),
        );
        let mut expected = utf16("Invalid hello message (receivedType=");
        expected.push(0xd800);
        expected.extend(utf16(", hasKey=true, preview=\"{\")"));
        assert_eq!(surrogate.message(), expected);
    }

    #[test]
    fn handshake_predicates_follow_the_type_guards() {
        let hello = |text: &str| is_hello(&js_json::parse(text).unwrap());
        assert!(hello(r#"{"type":"e2ee_hello","key":"k"}"#));
        assert!(hello(
            r#"{"type":"e2ee_hello","key":"k","capabilities":{}}"#
        ));
        assert!(!hello(r#"{"type":"e2ee_hello","key":" \t"}"#));
        assert!(!hello(
            r#"{"type":"e2ee_hello","key":"k","capabilities":null}"#
        ));
        assert!(!hello(
            r#"{"type":"e2ee_hello","key":"k","capabilities":{"binaryCiphertext":1}}"#
        ));
        assert!(!hello(r#"["e2ee_hello"]"#));
        let ready =
            js_json::parse(r#"{"type":"e2ee_ready","capabilities":{"binaryCiphertext":false}}"#)
                .unwrap();
        assert!(is_ready(&ready));
        assert!(!supports_binary_ciphertext(&ready));
        assert!(keys_equal(&[1, 2], &[1, 2]));
        assert!(!keys_equal(&[1, 2], &[1, 3]));
        assert!(!keys_equal(&[1], &[1, 2]));
    }
}
