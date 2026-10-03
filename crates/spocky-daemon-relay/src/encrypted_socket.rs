//! `websocket/encrypted-relay-socket.ts`: the socket the daemon hands to the application
//! after the end-to-end handshake. It reports readiness, bounds the bytes queued on the
//! physical relay socket and terminates that socket when the bound would be crossed.
//!
//! The socket owns only its ready state. The channel, the physical transport and the
//! event emitter stay with the caller and are passed in, so the original's closures map
//! to explicit arguments.

use spocky_crypto::channel::{AppSend, Data};
use std::{error::Error, fmt};

/// `MAX_PHYSICAL_SOCKET_BUFFERED_BYTES`: 64 MiB. `spocky-daemon` holds the same constant for the
/// daemon's own sockets; the differential compares this one with the pinned TypeScript value.
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
    /// # Errors
    ///
    /// `channel.close` threw.
    fn channel_close(&mut self, code: Option<u16>, reason: Option<&str>) -> Result<(), EnvFailure>;
    fn transport_buffered_amount(&self) -> Option<u64>;
    /// # Errors
    ///
    /// `socket.terminate()` threw.
    fn terminate_transport(&mut self) -> Result<(), EnvFailure>;
}

/// A throw from the channel or the physical socket, which the original lets propagate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvFailure(pub String);

impl fmt::Display for EnvFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for EnvFailure {}

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

    /// # Errors
    ///
    /// The terminate at the high-water mark threw; the original's `send` throws it.
    pub fn send(
        &mut self,
        env: &mut dyn EncryptedRelayEnv,
        data: &Data,
    ) -> Result<SendOutcome, EnvFailure> {
        if self.ready_state != READY_STATE_OPEN {
            return Ok(SendOutcome::Rejected(RelaySocketError::NotOpen));
        }
        let outbound_bytes = env.outbound_wire_byte_length(data);
        let queued_bytes = env.transport_buffered_amount().unwrap_or(0);
        if queued_bytes.saturating_add(outbound_bytes) > MAX_PHYSICAL_SOCKET_BUFFERED_BYTES {
            self.terminate(env)?;
            return Ok(SendOutcome::Rejected(RelaySocketError::HighWaterMark));
        }
        Ok(SendOutcome::Channel(env.channel_send(data)))
    }

    /// Resolves the promise `send` returned once the channel's send settles.
    #[must_use]
    pub fn settle<E>(result: Result<(), E>) -> Settlement<E> {
        match result {
            Ok(()) => Settlement::Resolved,
            Err(error) => Settlement::EmitErrorThenReject(error),
        }
    }

    /// # Errors
    ///
    /// `channel.close` threw after the state became closed.
    pub fn close(
        &mut self,
        env: &mut dyn EncryptedRelayEnv,
        code: Option<u16>,
        reason: Option<&str>,
    ) -> Result<(), EnvFailure> {
        if self.ready_state == READY_STATE_CLOSED {
            return Ok(());
        }
        self.ready_state = READY_STATE_CLOSED;
        env.channel_close(code, reason)
    }

    /// # Errors
    ///
    /// `socket.terminate()` threw after the state became closed.
    pub fn terminate(&mut self, env: &mut dyn EncryptedRelayEnv) -> Result<(), EnvFailure> {
        if self.ready_state == READY_STATE_CLOSED {
            return Ok(());
        }
        self.ready_state = READY_STATE_CLOSED;
        env.terminate_transport()
    }

    /// The `emitter.on("close")` listener the constructor installs: it runs before any
    /// application listener.
    pub fn on_emitter_close(&mut self) {
        self.ready_state = READY_STATE_CLOSED;
    }
}
