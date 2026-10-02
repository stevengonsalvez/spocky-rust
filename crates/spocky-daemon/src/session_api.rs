//! The boundary between the transport and the session layer.
//!
//! In the baseline `websocket-server.ts` constructs a `Session` per connected
//! client and hands it a set of callbacks (`onMessage`, `onMessageToSource`,
//! ...). The session layer lives in `spocky-session`; this module is the
//! narrow surface the transport needs from it, so that `spocky-session` can
//! implement it without the transport knowing session internals. The session
//! decides what an inbound session message means, what is valid, and which
//! outbound messages a given client may receive.

use std::sync::Arc;

use serde_json::Value;

use spocky_contracts::ws::DaemonPermission;

use crate::listen::ListenTarget;

/// One physical WebSocket. A reconnecting client gets a new id.
pub type SocketId = u64;

/// How a session reaches its client. Messages are the inner session message;
/// the transport wraps them as `{ "type": "session", "message": ... }`.
pub trait SessionSink: Send + Sync {
    /// `onMessage`: every socket attached to the session.
    fn send_to_connection(&self, message: &Value);
    /// `onMessageToSource`: one socket, dropped when it is no longer attached.
    fn send_to_source(&self, source: SocketId, message: &Value);
    /// `getTransportBufferedAmount`: bytes queued and not yet written, for one
    /// socket or the worst attached socket; `None` when there is no signal.
    fn buffered_amount(&self, source: Option<SocketId>) -> Option<usize>;
}

/// What the transport knows when a hello is accepted
/// (`createSocketSession` options).
pub struct SessionOpen {
    pub client_id: String,
    pub app_version: Option<String>,
    /// The hello `capabilities` object as the client sent it.
    pub client_capabilities: Option<Value>,
    pub permissions: Vec<DaemonPermission>,
    pub sink: Arc<dyn SessionSink>,
}

/// The body of a protocol failure sent to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolFailure {
    /// `requestId` of the offending request, when the frame carried one.
    pub request_id: Option<String>,
    /// `requestType`, when the frame carried one.
    pub request_type: Option<String>,
    pub error: String,
    /// `invalid_message` or `unknown_schema`.
    pub code: &'static str,
}

/// A live session. All methods may be called from any connection thread.
pub trait SessionHandle: Send + Sync {
    /// `getSessionId`.
    fn session_id(&self) -> String;
    /// `getPermissions`, for `server_info`.
    fn permissions(&self) -> Vec<DaemonPermission>;
    /// `updateClientCapabilities(capabilities, socket, appVersion)`, called on
    /// the first hello and on every resumed hello.
    fn update_client_capabilities(
        &self,
        capabilities: Option<&Value>,
        source: SocketId,
        app_version: Option<&str>,
    );
    /// `updateAppVersion`.
    fn update_app_version(&self, app_version: &str);
    /// `handleMessage`: the inner session message, already validated by
    /// [`SessionBackend::validate_inbound`]. It must return promptly, starting any long
    /// work in the background: the transport calls it on the thread that reads
    /// the socket, and must keep answering `ping` and later requests.
    fn handle_message(&self, message: Value, source: SocketId);
    /// `delivery.protocolFailure(socket, { requestId, requestType, error, code })`:
    /// tell one socket its frame was rejected. The transport builds the text
    /// and code from the pinned rules (`invalid_message`, `unknown_schema`).
    fn protocol_failure(&self, source: SocketId, failure: ProtocolFailure);
    /// A socket that was part of the session closed
    /// (`clearAgentTimelineSubscription`).
    fn socket_detached(&self, source: SocketId);
    /// `cleanup`: the session is over.
    fn cleanup(&self);
}

/// Creates sessions and judges session messages that arrive before a session
/// exists.
pub trait SessionBackend: Send + Sync {
    /// `new Session(...)`.
    fn open(&self, open: SessionOpen) -> Arc<dyn SessionHandle>;
    /// Checks `{ "type": "session", "message": message }` against the inbound
    /// schema. `Err` carries the schema error message that follows
    /// "Invalid message: " in a protocol failure. Before a hello the transport
    /// only needs the verdict: a valid session message closes the socket with
    /// "Session message before hello", an invalid one with "Invalid hello".
    ///
    /// # Errors
    ///
    /// The schema validation message.
    fn validate_inbound(&self, message: &Value) -> Result<(), String>;
    /// The agent steps of `bootstrap.ts` `stop()`, which run after
    /// `wsServer.prepareForShutdown()` and before `wsServer.close()`:
    /// `agentManager.prepareForShutdown()`, `closeAllAgents`,
    /// `agentManager.flushForShutdown()` and `agentStorage.flush()`. A backend
    /// without agents has nothing to do.
    fn stop_agents(&self) {}
    /// The `httpServer` `'listening'` handler of `bootstrap.ts`: the target the
    /// listener actually bound, as `resolveBoundListenTarget` returns it
    /// (`Tcp { host: "127.0.0.1", port }` after a `:0` bind, not the configured
    /// port). The structure is passed, not its formatted text, because
    /// `createAgentMcpBaseUrl` needs to tell a TCP target from a socket or pipe
    /// path, resolve the client host, and bracket an IPv6 host. Called once,
    /// after the server knows its address and before the lock is published. A
    /// backend that derives URLs from the address (the agent MCP base url)
    /// records it; any other has nothing to do.
    fn listening(&self, _bound: &ListenTarget) {}
}
