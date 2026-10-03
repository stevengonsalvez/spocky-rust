//! `websocket/encrypted-relay-socket.ts`: the socket the daemon hands to the application
//! after the end-to-end handshake. It reports readiness, bounds the bytes queued on the
//! physical relay socket and terminates that socket when the bound would be crossed.
//!
//! The socket owns only its ready state. The channel, the physical transport and the
//! event emitter stay with the caller and are passed in, so the original's closures map
//! to explicit arguments.

use spocky_crypto::channel::{AppSend, Data};
use std::{error::Error, fmt};

/// `MAX_PHYSICAL_SOCKET_BUFFERED_BYTES`: 64 MiB.
pub const MAX_PHYSICAL_SOCKET_BUFFERED_BYTES: u64 = 64 * 1024 * 1024;

/// WebSocket `readyState` of an open socket.
pub const READY_STATE_OPEN: u8 = 1;
/// WebSocket `readyState` of a closed socket.
pub const READY_STATE_CLOSED: u8 = 3;

/// Everything the socket reaches: the channel (`EncryptedRelayChannel`) and the physical
/// relay socket underneath (`getTransportBufferedAmount`, `terminateTransport`). One trait
/// keeps both behind one borrow.
pub trait EncryptedRelayEnv {
    fn set_state_open(&mut self);
    fn channel_send(&mut self, data: &Data) -> AppSend;
    fn outbound_wire_byte_length(&self, data: &Data) -> u64;
    fn channel_close(&mut self, code: Option<u16>, reason: Option<&str>);
    fn transport_buffered_amount(&self) -> Option<u64>;
    fn terminate_transport(&mut self);
}

/// Why a send was rejected before it reached the channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelaySocketError {
    NotOpen,
    HighWaterMark,
}

impl fmt::Display for RelaySocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotOpen => "Encrypted relay socket is not open",
            Self::HighWaterMark => "Encrypted relay socket exceeded its outbound high-water mark",
        })
    }
}

impl Error for RelaySocketError {}

/// What `send` produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SendOutcome {
    /// The returned promise rejects with this error.
    Rejected(RelaySocketError),
    /// The channel took the frame; its result settles the returned promise.
    Channel(AppSend),
}

/// How a settled channel send resolves the promise `send` returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Settlement<E> {
    Resolved,
    /// Emit `error` to the socket's listeners, then reject with the same error.
    EmitErrorThenReject(E),
}

#[derive(Debug)]
pub struct EncryptedRelaySocket {
    ready_state: u8,
}

impl EncryptedRelaySocket {
    /// `createEncryptedRelaySocket`: marks the channel open.
    pub fn new(env: &mut dyn EncryptedRelayEnv) -> Self {
        env.set_state_open();
        Self {
            ready_state: READY_STATE_OPEN,
        }
    }

    #[must_use]
    pub const fn ready_state(&self) -> u8 {
        self.ready_state
    }

    /// The `bufferedAmount` getter: the physical socket's queue, never double counted.
    #[must_use]
    pub fn buffered_amount(&self, env: &dyn EncryptedRelayEnv) -> u64 {
        env.transport_buffered_amount().unwrap_or(0)
    }

    pub fn send(&mut self, env: &mut dyn EncryptedRelayEnv, data: &Data) -> SendOutcome {
        if self.ready_state != READY_STATE_OPEN {
            return SendOutcome::Rejected(RelaySocketError::NotOpen);
        }
        let outbound_bytes = env.outbound_wire_byte_length(data);
        let queued_bytes = env.transport_buffered_amount().unwrap_or(0);
        if queued_bytes.saturating_add(outbound_bytes) > MAX_PHYSICAL_SOCKET_BUFFERED_BYTES {
            self.terminate(env);
            return SendOutcome::Rejected(RelaySocketError::HighWaterMark);
        }
        SendOutcome::Channel(env.channel_send(data))
    }

    /// Resolves the promise `send` returned once the channel's send settles.
    #[must_use]
    pub fn settle<E>(result: Result<(), E>) -> Settlement<E> {
        match result {
            Ok(()) => Settlement::Resolved,
            Err(error) => Settlement::EmitErrorThenReject(error),
        }
    }

    pub fn close(
        &mut self,
        env: &mut dyn EncryptedRelayEnv,
        code: Option<u16>,
        reason: Option<&str>,
    ) {
        if self.ready_state == READY_STATE_CLOSED {
            return;
        }
        self.ready_state = READY_STATE_CLOSED;
        env.channel_close(code, reason);
    }

    pub fn terminate(&mut self, env: &mut dyn EncryptedRelayEnv) {
        if self.ready_state == READY_STATE_CLOSED {
            return;
        }
        self.ready_state = READY_STATE_CLOSED;
        env.terminate_transport();
    }

    /// The `emitter.on("close")` listener the constructor installs: it runs before any
    /// application listener.
    pub fn on_emitter_close(&mut self) {
        self.ready_state = READY_STATE_CLOSED;
    }
}
