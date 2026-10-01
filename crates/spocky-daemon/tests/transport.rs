//! The daemon transport over real loopback sockets: upgrade admission, hello,
//! `server_info`, ping, close codes, session attach and reconnect, and plain
//! HTTP. Listeners bind port 0; the daemon's reserved ports are never used.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spocky_contracts::ws::{DaemonPermission, ServerId};
use spocky_daemon::admission::PasswordVerifier;
use spocky_daemon::hostnames::Hostnames;
use spocky_daemon::log::NullLogger;
use spocky_daemon::server::{ListenHandle, Server, ServerConfig, ServerDeps, Timeouts};
use spocky_daemon::session_api::{
    ProtocolFailure, SessionBackend, SessionHandle, SessionOpen, SessionSink, SocketId,
};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::CloseFrame;
use tungstenite::{Message, WebSocket};

const HASH: &str = "hash";
const LOCAL: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

struct Exact;

impl PasswordVerifier for Exact {
    fn verify(&self, password: &str, password_hash: &str) -> bool {
        password == "secret" && password_hash == HASH
    }
}

#[derive(Default)]
struct Calls {
    opens: Mutex<Vec<(String, Option<Value>)>>,
    capability_updates: AtomicUsize,
    messages: Mutex<Vec<(SocketId, Value)>>,
    failures: Mutex<Vec<(SocketId, ProtocolFailure)>>,
    detached: Mutex<Vec<SocketId>>,
    cleanups: AtomicUsize,
    app_version_acks: AtomicUsize,
    /// Calls that reached a session after it was cleaned up.
    use_after_cleanup: AtomicUsize,
    sinks: Mutex<Vec<Arc<dyn SessionSink>>>,
}

struct Backend(Arc<Calls>);

struct Handle {
    calls: Arc<Calls>,
    cleaned: std::sync::atomic::AtomicBool,
    sink: Arc<dyn SessionSink>,
}

impl Handle {
    fn used(&self) {
        if self.cleaned.load(Ordering::SeqCst) {
            self.calls.use_after_cleanup.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl SessionHandle for Handle {
    fn session_id(&self) -> String {
        "session-1".to_owned()
    }
    fn permissions(&self) -> Vec<DaemonPermission> {
        DaemonPermission::ALL.to_vec()
    }
    fn update_client_capabilities(&self, _: Option<&Value>, _: SocketId, _: Option<&str>) {
        self.used();
        self.calls.capability_updates.fetch_add(1, Ordering::SeqCst);
    }
    fn update_app_version(&self, _: &str) {
        // A backend may answer through the sink while it is told about a new
        // app version; the sink takes the connection state lock.
        self.sink
            .send_to_connection(&json!({"type": "app_version_ack"}));
        self.calls.app_version_acks.fetch_add(1, Ordering::SeqCst);
    }
    fn handle_message(&self, message: Value, source: SocketId) {
        self.used();
        self.calls.messages.lock().unwrap().push((source, message));
    }
    fn protocol_failure(&self, source: SocketId, failure: ProtocolFailure) {
        self.calls.failures.lock().unwrap().push((source, failure));
    }
    fn socket_detached(&self, source: SocketId) {
        self.calls.detached.lock().unwrap().push(source);
    }
    fn cleanup(&self) {
        self.cleaned.store(true, Ordering::SeqCst);
        self.calls.cleanups.fetch_add(1, Ordering::SeqCst);
    }
}

impl SessionBackend for Backend {
    fn open(&self, open: SessionOpen) -> Arc<dyn SessionHandle> {
        self.0
            .opens
            .lock()
            .unwrap()
            .push((open.client_id, open.client_capabilities));
        self.0.sinks.lock().unwrap().push(Arc::clone(&open.sink));
        Arc::new(Handle {
            sink: open.sink,
            calls: Arc::clone(&self.0),
            cleaned: std::sync::atomic::AtomicBool::new(false),
        })
    }
    fn validate_inbound(&self, message: &Value) -> Result<(), String> {
        match message.get("type").and_then(Value::as_str) {
            Some("fetch_agents_request" | "known_request") => Ok(()),
            Some(_) => Err("Invalid discriminator value".to_owned()),
            None => Err("Invalid input".to_owned()),
        }
    }
}

struct Harness {
    server: Server,
    listener: ListenHandle,
    calls: Arc<Calls>,
    port: u16,
}

fn config() -> ServerConfig {
    ServerConfig {
        server_id: ServerId::new("srv_test").unwrap(),
        daemon_version: "0.10.0".to_owned(),
        hostname: "box".to_owned(),
        hostnames: None,
        allowed_origins: HashSet::new(),
        password_hash: None,
        desktop_managed: false,
        workspace_labels: false,
        advertise_daemon_status_rpc: true,
        advertise_relay_config: true,
        start_paused: false,
        capabilities: None,
        timeouts: Timeouts {
            hello: Duration::from_millis(400),
            reconnect_grace: Duration::from_millis(400),
            application_lease: Duration::from_millis(300),
            close: Duration::from_millis(300),
        },
    }
}

fn start(config: ServerConfig) -> Harness {
    let calls = Arc::new(Calls::default());
    let server = Server::new(
        config,
        ServerDeps {
            backend: Arc::new(Backend(Arc::clone(&calls))),
            verifier: Arc::new(Exact),
            local_credential: Arc::new(|| Some(LOCAL.to_owned())),
            logger: Arc::new(NullLogger),
        },
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let handle = server.serve_tcp(listener).unwrap();
    let port = handle.local_addr().unwrap().port();
    assert!(port != 6767 && port != 6768);
    server.set_listen(&format!("127.0.0.1:{port}"), true);
    Harness {
        server,
        listener: handle,
        calls,
        port,
    }
}

impl Harness {
    fn connect(&self, headers: &[(&str, &str)]) -> WebSocket<TcpStream> {
        let mut request = format!("ws://127.0.0.1:{}/ws", self.port)
            .into_client_request()
            .unwrap();
        for (name, value) in headers {
            request.headers_mut().insert(
                tungstenite::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        tungstenite::client(request, stream).unwrap().0
    }

    fn raw(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        String::from_utf8_lossy(&response).into_owned()
    }

    fn finish(self) {
        self.server.close();
        self.listener.stop();
    }
}

fn hello(client_id: &str) -> Value {
    json!({"type": "hello", "clientId": client_id, "clientType": "cli", "protocolVersion": 1})
}

fn send(ws: &mut WebSocket<TcpStream>, value: &Value) {
    ws.send(Message::text(value.to_string())).unwrap();
}

fn next_json(ws: &mut WebSocket<TcpStream>) -> Value {
    loop {
        match ws.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Close(frame) => panic!("closed: {frame:?}"),
            _ => {}
        }
    }
}

/// Reads until the close frame and returns `(code, reason)`.
fn next_close(ws: &mut WebSocket<TcpStream>) -> (u16, String) {
    loop {
        match ws.read() {
            Ok(Message::Close(Some(CloseFrame { code, reason }))) => {
                return (u16::from(code), reason.to_string());
            }
            Ok(Message::Close(None)) => return (1005, String::new()),
            Ok(_) => {}
            Err(error) => panic!("expected a close frame: {error}"),
        }
    }
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn hello_gets_server_info_and_ping_gets_pong() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("client-a"));
    let frame = next_json(&mut ws);
    assert_eq!(frame["type"], "session");
    assert_eq!(frame["message"]["type"], "status");
    let payload = &frame["message"]["payload"];
    assert_eq!(payload["status"], "server_info");
    assert_eq!(payload["serverId"], "srv_test");
    assert_eq!(payload["version"], "0.10.0");
    assert_eq!(payload["protocolVersion"], 1);
    assert_eq!(payload["permissions"].as_array().unwrap().len(), 9);
    send(&mut ws, &json!({"type": "ping"}));
    assert_eq!(next_json(&mut ws), json!({"type": "pong"}));
    assert_eq!(harness.calls.opens.lock().unwrap().len(), 1);
    harness.finish();
}

#[test]
fn a_ping_or_session_message_before_hello_closes_with_4002() {
    let harness = start(config());
    for frame in [
        json!({"type": "ping"}),
        json!({"type": "session", "message": {"type": "fetch_agents_request"}}),
        json!({"type": "recording_state", "isRecording": true}),
    ] {
        let mut ws = harness.connect(&[]);
        send(&mut ws, &frame);
        assert_eq!(
            next_close(&mut ws),
            (4002, "Session message before hello".to_owned()),
            "{frame}"
        );
    }
    harness.finish();
}

#[test]
fn an_invalid_first_frame_closes_with_invalid_hello() {
    let harness = start(config());
    for text in [
        "not json".to_owned(),
        json!({"type": "hello"}).to_string(),
        json!({"type": "hello", "clientId": "", "clientType": "cli", "protocolVersion": 1})
            .to_string(),
        json!({"type": "nope"}).to_string(),
        json!({"type": "session", "message": {"type": "bogus"}}).to_string(),
    ] {
        let mut ws = harness.connect(&[]);
        ws.send(Message::text(text.clone())).unwrap();
        assert_eq!(
            next_close(&mut ws),
            (4002, "Invalid hello".to_owned()),
            "{text}"
        );
    }
    harness.finish();
}

#[test]
fn no_hello_in_time_closes_with_4001() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    assert_eq!(next_close(&mut ws), (4001, "Hello timeout".to_owned()));
    harness.finish();
}

#[test]
fn an_old_protocol_is_rejected_with_4003_and_a_frame_only_for_new_clients() {
    let harness = start(config());
    let mut old = hello("c");
    old["protocolVersion"] = json!(0);

    let mut plain = harness.connect(&[]);
    send(&mut plain, &old);
    assert_eq!(
        next_close(&mut plain),
        (4003, "Incompatible protocol version".to_owned())
    );

    let mut aware = harness.connect(&[]);
    old["capabilities"] = json!({"hello_rejection": true});
    send(&mut aware, &old);
    assert_eq!(
        next_json(&mut aware),
        json!({"type": "hello.rejected", "reason": "incompatible_protocol", "accepts": ["password"]})
    );
    assert_eq!(
        next_close(&mut aware),
        (4003, "Incompatible protocol version".to_owned())
    );
    harness.finish();
}

#[test]
fn a_newer_protocol_is_accepted() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    let mut newer = hello("c");
    newer["protocolVersion"] = json!(7);
    send(&mut ws, &newer);
    assert_eq!(
        next_json(&mut ws)["message"]["payload"]["status"],
        "server_info"
    );
    harness.finish();
}

fn with_password() -> ServerConfig {
    ServerConfig {
        password_hash: Some(HASH.to_owned()),
        ..config()
    }
}

#[test]
fn a_password_daemon_rejects_missing_and_wrong_credentials_with_4401() {
    let harness = start(with_password());
    let cases: [(Option<Value>, &str, &str); 3] = [
        (None, "password_required", "Password required"),
        (
            Some(json!({"kind": "password", "password": "nope"})),
            "incorrect_password",
            "Incorrect password",
        ),
        (
            Some(json!({"kind": "localCredential", "token": "short"})),
            "incorrect_password",
            "Incorrect password",
        ),
    ];
    for (auth, reason, close_reason) in cases {
        let mut ws = harness.connect(&[]);
        let mut message = hello("c");
        message["capabilities"] = json!({"hello_rejection": true});
        if let Some(auth) = auth {
            message["auth"] = auth;
        }
        send(&mut ws, &message);
        assert_eq!(
            next_json(&mut ws),
            json!({"type": "hello.rejected", "reason": reason, "accepts": ["password"]})
        );
        assert_eq!(next_close(&mut ws), (4401, close_reason.to_owned()));
    }
    assert!(harness.calls.opens.lock().unwrap().is_empty());
    harness.finish();
}

#[test]
fn a_password_daemon_sends_no_rejection_frame_to_clients_that_cannot_read_it() {
    let harness = start(with_password());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    assert_eq!(next_close(&mut ws), (4401, "Password required".to_owned()));
    harness.finish();
}

#[test]
fn the_right_password_or_local_credential_is_admitted() {
    let harness = start(with_password());
    for (index, auth) in [
        json!({"kind": "password", "password": "secret"}),
        json!({"kind": "localCredential", "token": LOCAL}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut ws = harness.connect(&[]);
        let mut message = hello(&format!("c{index}"));
        message["auth"] = auth;
        send(&mut ws, &message);
        assert_eq!(
            next_json(&mut ws)["message"]["payload"]["status"],
            "server_info"
        );
    }
    harness.finish();
}

#[test]
fn a_bearer_subprotocol_admits_without_a_hello_credential_and_a_bad_one_closes_with_4401() {
    let harness = start(with_password());
    let mut good = harness.connect(&[("Sec-WebSocket-Protocol", "paseo.bearer.secret")]);
    send(&mut good, &hello("c"));
    assert_eq!(
        next_json(&mut good)["message"]["payload"]["status"],
        "server_info"
    );

    let mut bad = harness.connect(&[("Sec-WebSocket-Protocol", "paseo.bearer.wrong")]);
    assert_eq!(
        next_close(&mut bad),
        (4401, "Incorrect password".to_owned())
    );

    let mut header = harness.connect(&[("Authorization", "Bearer wrong")]);
    assert_eq!(
        next_close(&mut header),
        (4401, "Incorrect password".to_owned())
    );
    harness.finish();
}

#[test]
fn a_bearer_header_is_accepted_even_when_no_password_is_set() {
    let harness = start(config());
    let mut ws = harness.connect(&[("Authorization", "Bearer anything")]);
    send(&mut ws, &hello("c"));
    assert_eq!(
        next_json(&mut ws)["message"]["payload"]["status"],
        "server_info"
    );
    harness.finish();
}

#[test]
fn session_messages_reach_the_backend_and_replies_reach_the_socket() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    next_json(&mut ws);
    send(
        &mut ws,
        &json!({"type": "session", "message": {"type": "fetch_agents_request", "requestId": "r1"}}),
    );
    wait_for("the message", || {
        !harness.calls.messages.lock().unwrap().is_empty()
    });
    let (source, message) = harness.calls.messages.lock().unwrap()[0].clone();
    assert_eq!(
        message,
        json!({"type": "fetch_agents_request", "requestId": "r1"})
    );

    let sink = Arc::clone(&harness.calls.sinks.lock().unwrap()[0]);
    sink.send_to_source(source, &json!({"type": "fetch_agents_response", "n": 1}));
    assert_eq!(
        next_json(&mut ws),
        json!({"type": "session", "message": {"type": "fetch_agents_response", "n": 1}})
    );
    sink.send_to_connection(&json!({"type": "agent_update"}));
    assert_eq!(next_json(&mut ws)["message"]["type"], "agent_update");
    sink.send_to_source(source + 100, &json!({"type": "never"}));
    harness.finish();
}

#[test]
fn an_invalid_session_message_becomes_a_protocol_failure() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    next_json(&mut ws);
    send(
        &mut ws,
        &json!({"type": "session", "message": {"type": "bogus", "requestId": "r9"}}),
    );
    send(&mut ws, &json!({"type": "nope", "requestId": "r10"}));
    send(&mut ws, &json!({"type": "nope"}));
    wait_for("failures", || {
        harness.calls.failures.lock().unwrap().len() == 3
    });
    let failures = harness.calls.failures.lock().unwrap().clone();
    assert_eq!(failures[0].1.code, "unknown_schema");
    assert_eq!(failures[0].1.request_id.as_deref(), Some("r9"));
    assert_eq!(failures[0].1.request_type.as_deref(), Some("bogus"));
    assert_eq!(
        failures[0].1.error,
        "Unknown request, try upgrading the daemon (currently v0.10.0)"
    );
    assert_eq!(failures[1].1.code, "invalid_message");
    assert_eq!(failures[1].1.request_id.as_deref(), Some("r10"));
    assert!(failures[1].1.error.starts_with("Invalid message: "));
    assert_eq!(failures[2].1.request_id, None);
    harness.finish();
}

#[test]
fn a_second_hello_on_an_active_socket_closes_with_4002() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    next_json(&mut ws);
    send(&mut ws, &hello("c"));
    assert_eq!(next_close(&mut ws), (4002, "Unexpected hello".to_owned()));
    harness.finish();
}

#[test]
fn a_plugin_client_id_is_reserved() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("plugin:thing"));
    assert_eq!(
        next_close(&mut ws),
        (4002, "Invalid plugin clientId".to_owned())
    );
    harness.finish();
}

#[test]
fn a_client_that_reconnects_in_time_resumes_its_session() {
    let mut cfg = config();
    cfg.timeouts.reconnect_grace = Duration::from_secs(5);
    let harness = start(cfg);
    let mut first = harness.connect(&[]);
    send(&mut first, &hello("same"));
    next_json(&mut first);
    drop(first);
    wait_for("detach", || {
        !harness.calls.detached.lock().unwrap().is_empty()
    });

    let mut second = harness.connect(&[]);
    send(&mut second, &hello("same"));
    assert_eq!(
        next_json(&mut second)["message"]["payload"]["status"],
        "server_info"
    );
    assert_eq!(harness.calls.opens.lock().unwrap().len(), 1, "same session");
    assert_eq!(harness.calls.capability_updates.load(Ordering::SeqCst), 2);
    assert_eq!(harness.calls.cleanups.load(Ordering::SeqCst), 0);

    let mut other = harness.connect(&[]);
    send(&mut other, &hello("different"));
    next_json(&mut other);
    assert_eq!(harness.calls.opens.lock().unwrap().len(), 2);
    harness.finish();
}

#[test]
fn a_session_is_cleaned_up_after_the_reconnect_grace() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("gone"));
    next_json(&mut ws);
    drop(ws);
    wait_for("cleanup", || {
        harness.calls.cleanups.load(Ordering::SeqCst) == 1
    });
    let mut again = harness.connect(&[]);
    send(&mut again, &hello("gone"));
    next_json(&mut again);
    assert_eq!(
        harness.calls.opens.lock().unwrap().len(),
        2,
        "a new session"
    );
    harness.finish();
}

#[test]
fn two_sockets_share_one_session_and_both_get_messages() {
    let mut cfg = config();
    cfg.timeouts.reconnect_grace = Duration::from_secs(5);
    let harness = start(cfg);
    let mut a = harness.connect(&[]);
    send(&mut a, &hello("shared"));
    next_json(&mut a);
    let mut b = harness.connect(&[]);
    send(&mut b, &hello("shared"));
    next_json(&mut b);
    let sink = Arc::clone(&harness.calls.sinks.lock().unwrap()[0]);
    sink.send_to_connection(&json!({"type": "both"}));
    assert_eq!(next_json(&mut a)["message"]["type"], "both");
    assert_eq!(next_json(&mut b)["message"]["type"], "both");
    drop(a);
    wait_for("detach", || {
        harness.calls.detached.lock().unwrap().len() == 1
    });
    assert_eq!(harness.calls.cleanups.load(Ordering::SeqCst), 0);
    harness.finish();
}

#[test]
fn an_idle_application_socket_is_dropped_after_its_lease() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    next_json(&mut ws);
    send(&mut ws, &json!({"type": "ping"}));
    assert_eq!(next_json(&mut ws), json!({"type": "pong"}));
    std::thread::sleep(Duration::from_millis(600));
    assert!(ws.read().is_err(), "socket should have been terminated");
    harness.finish();
}

#[test]
fn pings_renew_the_lease() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    next_json(&mut ws);
    for _ in 0..6 {
        send(&mut ws, &json!({"type": "ping"}));
        assert_eq!(next_json(&mut ws), json!({"type": "pong"}));
        std::thread::sleep(Duration::from_millis(100));
    }
    harness.finish();
}

fn upgrade_request(extra: &str) -> String {
    format!(
        "GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n"
    )
}

#[test]
fn upgrades_enforce_host_and_origin_with_the_pinned_bodies() {
    let mut cfg = config();
    cfg.hostnames = Some(Hostnames::Patterns(vec![]));
    let harness = start(cfg);
    let evil_host = harness.raw(&upgrade_request("Host: evil.example\r\n"));
    assert!(
        evil_host.starts_with("HTTP/1.1 403 Forbidden\r\n"),
        "{evil_host}"
    );
    assert!(evil_host.ends_with("\r\n\r\nHost not allowed"));
    let evil_origin = harness.raw(&upgrade_request(&format!(
        "Host: 127.0.0.1:{}\r\nOrigin: https://evil.example\r\n",
        harness.port
    )));
    assert!(
        evil_origin.ends_with("\r\n\r\nOrigin not allowed"),
        "{evil_origin}"
    );
    let same_origin = harness.raw(&upgrade_request(&format!(
        "Host: 127.0.0.1:{0}\r\nOrigin: http://localhost:{0}\r\n",
        harness.port
    )));
    assert!(
        same_origin.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{same_origin}"
    );
    let no_host = harness.raw(&upgrade_request(""));
    assert!(
        no_host.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{no_host}"
    );
    harness.finish();
}

#[test]
fn a_daemon_that_is_not_ready_answers_503_until_it_begins_accepting() {
    let mut cfg = config();
    cfg.start_paused = true;
    let harness = start(cfg);
    let response = harness.raw(&upgrade_request(""));
    assert!(
        response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
        "{response}"
    );
    assert!(response.ends_with("\r\n\r\nServer not ready"));
    harness.server.begin_accepting_connections();
    let ready = harness.raw(&upgrade_request(""));
    assert!(ready.starts_with("HTTP/1.1 101"), "{ready}");
    harness.finish();
}

#[test]
fn malformed_upgrades_get_the_ws_library_answers() {
    let harness = start(config());
    let wrong_path = harness.raw(&upgrade_request("").replace("GET /ws", "GET /other"));
    assert!(
        wrong_path.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "{wrong_path}"
    );
    assert!(wrong_path.ends_with("\r\n\r\nBad Request"));
    let bad_key = harness.raw(&upgrade_request("").replace("dGhlIHNhbXBsZSBub25jZQ==", "short"));
    assert!(bad_key.ends_with("Missing or invalid Sec-WebSocket-Key header"));
    let bad_protocol = harness.raw(&upgrade_request("Sec-WebSocket-Protocol: a;b\r\n"));
    assert!(bad_protocol.ends_with("Invalid Sec-WebSocket-Protocol header"));
    harness.finish();
}

#[test]
fn the_selected_subprotocol_is_echoed() {
    let harness = start(config());
    let response = harness.raw(&upgrade_request("Sec-WebSocket-Protocol: chat, other\r\n"));
    assert!(
        response.contains("Sec-WebSocket-Protocol: chat\r\n"),
        "{response}"
    );
    harness.finish();
}

#[test]
fn plain_http_serves_health_and_status() {
    let harness = start(config());
    let health =
        harness.raw("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert!(health.starts_with("HTTP/1.1 200 OK\r\n"), "{health}");
    assert!(health.contains("\"status\":\"ok\""));
    let status =
        harness.raw("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert!(status.ends_with(&format!(
        "{{\"status\":\"server_info\",\"serverId\":\"srv_test\",\"hostname\":\"box\",\"version\":\"0.10.0\",\"listen\":\"127.0.0.1:{}\"}}",
        harness.port
    )));
    let bad_host =
        harness.raw("GET /api/status HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
    assert!(bad_host.starts_with("HTTP/1.1 403 Forbidden\r\n"));
    let garbage = harness.raw("\x16\x03\x01 not http\r\n\r\n");
    assert!(
        garbage.starts_with("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n"),
        "{garbage}"
    );
    harness.finish();
}

#[test]
fn closing_the_server_closes_open_sockets_and_cleans_up_sessions() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("c"));
    next_json(&mut ws);
    let server = harness.server.clone();
    let closer = std::thread::spawn(move || server.close());
    let (code, _) = next_close(&mut ws);
    assert_eq!(code, 1005);
    closer.join().unwrap();
    assert!(harness.calls.cleanups.load(Ordering::SeqCst) >= 1);
    harness.listener.stop();
}

#[cfg(unix)]
#[test]
fn a_unix_socket_listener_serves_the_same_protocol() {
    use std::os::unix::net::{UnixListener, UnixStream};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.sock");
    let calls = Arc::new(Calls::default());
    let server = Server::new(
        config(),
        ServerDeps {
            backend: Arc::new(Backend(Arc::clone(&calls))),
            verifier: Arc::new(Exact),
            local_credential: Arc::new(|| None),
            logger: Arc::new(NullLogger),
        },
    );
    let handle = server
        .serve_unix(UnixListener::bind(&path).unwrap())
        .unwrap();
    let stream = UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = "ws://localhost/ws".into_client_request().unwrap();
    let (mut ws, _) = tungstenite::client(request, stream).unwrap();
    ws.send(Message::text(hello("unix").to_string())).unwrap();
    let Message::Text(text) = ws.read().unwrap() else {
        panic!("expected text");
    };
    assert!(text.as_str().contains("server_info"));
    server.close();
    handle.stop();
}

#[test]
fn connections_past_the_cap_are_closed_and_the_slot_frees_up() {
    let harness = start(config());
    harness.server.set_max_connections(2);
    let hold = || TcpStream::connect(("127.0.0.1", harness.port)).unwrap();
    let first = hold();
    let second = hold();
    std::thread::sleep(Duration::from_millis(150));
    let started = Instant::now();
    let mut third = hold();
    third
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut byte = [0_u8; 1];
    // A closed connection reads EOF or a reset; a timeout means it stayed open.
    match third.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("the excess connection was not closed: {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "closed after {:?}",
        started.elapsed()
    );

    drop(first);
    wait_for("a free slot", || {
        harness
            .raw("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .starts_with("HTTP/1.1 200")
    });
    drop(second);
    harness.finish();
}

#[test]
fn a_flood_of_connections_does_not_stop_the_accept_loop() {
    let harness = start(config());
    harness.server.set_max_connections(8);
    let flood: Vec<TcpStream> = (0..200)
        .filter_map(|_| TcpStream::connect(("127.0.0.1", harness.port)).ok())
        .collect();
    drop(flood);
    wait_for("the flood to drain", || {
        harness
            .raw("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .starts_with("HTTP/1.1 200")
    });
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("after-flood"));
    assert_eq!(
        next_json(&mut ws)["message"]["payload"]["status"],
        "server_info"
    );
    harness.finish();
}

#[test]
fn a_peer_that_stops_reading_is_terminated_at_the_high_water_mark() {
    let harness = start(config());
    harness.server.set_max_buffered_bytes(256 * 1024);
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("stalled"));
    next_json(&mut ws);
    let sink = Arc::clone(&harness.calls.sinks.lock().unwrap()[0]);
    // Many small frames to a peer that never reads. The kernel buffers absorb the
    // first megabytes; after that every write times out, so the socket must be
    // dropped by the high-water mark, not by a single oversized frame.
    let message = json!({"type": "chunk", "data": "x".repeat(16 * 1024)});
    let started = Instant::now();
    let mut peak = 0;
    let mut sent = 0_u64;
    while harness.calls.detached.lock().unwrap().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the stalled socket was still attached after {sent} frames"
        );
        sink.send_to_connection(&message);
        sent += 1;
        peak = peak.max(sink.buffered_amount(None).unwrap_or(0));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        sent > 20,
        "the mark was hit by the first frames ({sent}), not by a stall"
    );
    assert!(peak <= 256 * 1024 + 16 * 1024, "buffered {peak} bytes");
    harness.finish();
}

#[test]
fn a_slow_reader_below_the_mark_still_receives_every_frame_in_order() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("slow"));
    next_json(&mut ws);
    let sink = Arc::clone(&harness.calls.sinks.lock().unwrap()[0]);
    for index in 0..40 {
        sink.send_to_connection(
            &json!({"type": "chunk", "index": index, "data": "y".repeat(200_000)}),
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    for index in 0..40 {
        let frame = next_json(&mut ws);
        assert_eq!(frame["message"]["index"], index);
    }
    harness.finish();
}

#[test]
fn concurrent_hellos_for_one_client_share_a_single_session() {
    for round in 0..5 {
        let mut cfg = config();
        cfg.timeouts.reconnect_grace = Duration::from_secs(5);
        let harness = start(cfg);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let mut ws = harness.connect(&[]);
                    barrier.wait();
                    send(&mut ws, &hello("racer"));
                    assert_eq!(
                        next_json(&mut ws)["message"]["payload"]["status"],
                        "server_info"
                    );
                });
            }
        });
        assert_eq!(
            harness.calls.opens.lock().unwrap().len(),
            1,
            "round {round}: more than one session for one client"
        );
        harness.finish();
    }
}

#[test]
fn a_hello_never_lands_on_a_session_that_is_being_cleaned_up() {
    let mut cfg = config();
    cfg.timeouts.reconnect_grace = Duration::from_millis(3);
    let harness = start(cfg);
    for round in 0..80_u64 {
        let mut ws = harness.connect(&[]);
        send(&mut ws, &hello("edge"));
        next_json(&mut ws);
        send(
            &mut ws,
            &json!({"type": "session", "message": {"type": "known_request", "round": round}}),
        );
        drop(ws);
        std::thread::sleep(Duration::from_millis(round % 7));
    }
    let calls = Arc::clone(&harness.calls);
    harness.finish();
    assert_eq!(
        calls.use_after_cleanup.load(Ordering::SeqCst),
        0,
        "a session was used after cleanup"
    );
}

#[test]
fn a_backend_may_send_while_it_is_told_a_new_app_version_on_resume() {
    let mut cfg = config();
    cfg.timeouts.reconnect_grace = Duration::from_secs(5);
    let harness = start(cfg);
    let mut first = harness.connect(&[]);
    let mut hello_v1 = hello("versioned");
    hello_v1["appVersion"] = json!("1.0.0");
    send(&mut first, &hello_v1);
    next_json(&mut first);
    drop(first);
    wait_for("detach", || {
        !harness.calls.detached.lock().unwrap().is_empty()
    });

    let mut second = harness.connect(&[]);
    let mut hello_v2 = hello("versioned");
    hello_v2["appVersion"] = json!("2.0.0");
    send(&mut second, &hello_v2);
    // A deadlock would leave the hello unanswered and the read would time out.
    // The ack goes to the sockets attached at that moment, and the new one is not
    // attached yet, so only server_info reaches it.
    assert_eq!(
        next_json(&mut second)["message"]["payload"]["status"],
        "server_info"
    );
    wait_for("the ack", || {
        harness.calls.app_version_acks.load(Ordering::SeqCst) == 1
    });
    harness.finish();
}

/// `JSON.parse` error messages from Node 22.20.0, the runtime the pinned daemon
/// runs on. The wire text is "Invalid message: " followed by these.
const V8_PARSE_ERRORS: &[(&str, &str)] = &[
    (
        "not json",
        "Unexpected token 'o', \"not json\" is not valid JSON",
    ),
    (
        "{bad",
        "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
    ),
    ("", "Unexpected end of JSON input"),
    (
        "{\"a\":1,}",
        "Expected double-quoted property name in JSON at position 7 (line 1 column 8)",
    ),
    (
        "[1,2",
        "Expected ',' or ']' after array element in JSON at position 4 (line 1 column 5)",
    ),
    ("{\"a\":", "Unexpected end of JSON input"),
    (
        "\"unterminated",
        "Unterminated string in JSON at position 13 (line 1 column 14)",
    ),
    (
        "{\"type\":\"ping\"} x",
        "Unexpected non-whitespace character after JSON at position 16 (line 1 column 17)",
    ),
    ("[,]", "Unexpected token ',', \"[,]\" is not valid JSON"),
    (
        "{\"a\" 1}",
        "Expected ':' after property name in JSON at position 5 (line 1 column 6)",
    ),
];

#[test]
fn unparsable_text_before_hello_closes_with_4002_invalid_hello_and_no_frame() {
    let harness = start(config());
    for (text, _) in V8_PARSE_ERRORS {
        let mut ws = harness.connect(&[]);
        ws.send(Message::text(*text)).unwrap();
        match ws.read() {
            Ok(Message::Close(Some(frame))) => {
                assert_eq!(u16::from(frame.code), 4002, "{text:?}");
                assert_eq!(frame.reason.as_str(), "Invalid hello", "{text:?}");
            }
            other => panic!("{text:?}: the first thing sent must be the close, got {other:?}"),
        }
    }
    assert!(harness.calls.opens.lock().unwrap().is_empty());
    harness.finish();
}

#[test]
fn unparsable_text_after_hello_is_a_protocol_failure_with_the_v8_message() {
    let harness = start(config());
    let mut ws = harness.connect(&[]);
    send(&mut ws, &hello("parser"));
    next_json(&mut ws);
    for (text, _) in V8_PARSE_ERRORS {
        ws.send(Message::text(*text)).unwrap();
    }
    wait_for("the failures", || {
        harness.calls.failures.lock().unwrap().len() == V8_PARSE_ERRORS.len()
    });
    let failures = harness.calls.failures.lock().unwrap().clone();
    for ((text, message), (_, failure)) in V8_PARSE_ERRORS.iter().zip(&failures) {
        assert_eq!(
            failure.error,
            format!("Invalid message: {message}"),
            "{text:?}"
        );
        assert_eq!(failure.code, "invalid_message", "{text:?}");
        assert_eq!(failure.request_id, None, "{text:?}");
        assert_eq!(failure.request_type, None, "{text:?}");
    }
    // The socket stays attached: a parse failure after hello does not close it.
    send(&mut ws, &json!({"type": "ping"}));
    assert_eq!(next_json(&mut ws), json!({"type": "pong"}));
    harness.finish();
}
