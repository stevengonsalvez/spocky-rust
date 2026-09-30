use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::daemon_socket::{
    DaemonEnrollment, DaemonPresence, DaemonRequestError, DaemonStatus, HubDaemonRuntime,
    credential_verifier,
};
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderValue, StatusCode};
use tungstenite::protocol::{CloseFrame, Message};

const DEADLINE: Duration = Duration::from_secs(3);
const DAEMON_ID: &str = "daemon-runtime-1";
const CREDENTIAL: &str = "private-daemon-credential";

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-hub-daemon-runtime-{}-{nonce}-{}",
            std::process::id(),
            TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn state_path(&self) -> PathBuf {
        self.0.join("daemon-runtime.json")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

#[test]
fn credential_is_private_and_standard_session_requires_permission_agreement() {
    let root = TestDir::new();
    let runtime = enrolled_runtime(&root.state_path(), &["hub.execute"]);
    let (mut daemon, response) = connect(runtime.address(), CREDENTIAL, true).expect("connect");

    assert_eq!(response.headers()["x-paseo-session-protocol"], "1");
    assert_eq!(read_json(&mut daemon)["type"], "hello");
    assert!(!runtime.is_ready(DAEMON_ID));

    send_server_info(&mut daemon, &["hub.execute"]);
    wait_until(|| runtime.is_ready(DAEMON_ID));
    let record = runtime.daemon(DAEMON_ID).expect("registered daemon");
    assert_eq!(record.credential_verifier, credential_verifier(CREDENTIAL));
    assert!(!runtime.snapshot_contains(CREDENTIAL));
    assert_eq!(record.presence, DaemonPresence::Connected);
}

#[test]
fn fragmented_upgrade_waits_for_complete_headers() {
    let root = TestDir::new();
    let runtime = enrolled_runtime(&root.state_path(), &["hub.execute"]);
    let mut stream = TcpStream::connect(runtime.address()).expect("connect TCP");
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("set read timeout");
    stream
        .write_all(b"GET /api/daemons/socket HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .expect("write first header fragment");
    thread::sleep(Duration::from_millis(100));
    stream
        .write_all(
            format!(
                "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nAuthorization: Bearer {CREDENTIAL}\r\nX-Paseo-Daemon-Id: {DAEMON_ID}\r\nX-Paseo-Session-Protocol: 1\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("write remaining headers");

    let mut response = [0_u8; 4096];
    let read = stream.read(&mut response).expect("read upgrade response");
    let response = String::from_utf8_lossy(&response[..read]);
    assert!(
        response.starts_with("HTTP/1.1 101"),
        "unexpected upgrade response: {response}"
    );
}

#[test]
fn legacy_session_connects_without_hello() {
    let root = TestDir::new();
    let runtime = enrolled_runtime(&root.state_path(), &[]);
    let (daemon, response) = connect(runtime.address(), CREDENTIAL, false).expect("connect");

    assert!(response.headers().get("x-paseo-session-protocol").is_none());
    wait_until(|| runtime.is_ready(DAEMON_ID));
    assert_eq!(
        runtime.daemon(DAEMON_ID).expect("daemon").presence,
        DaemonPresence::Connected
    );
    drop(daemon);
}

#[test]
fn permission_mismatch_closes_standard_session() {
    let root = TestDir::new();
    let runtime = enrolled_runtime(&root.state_path(), &["hub.execute"]);
    let (mut daemon, _) = connect(runtime.address(), CREDENTIAL, true).expect("connect");
    assert_eq!(read_json(&mut daemon)["type"], "hello");

    send_server_info(&mut daemon, &[]);

    assert_eq!(u16::from(wait_for_close(&mut daemon).code), 4403);
    assert!(!runtime.is_ready(DAEMON_ID));
    assert_eq!(
        runtime.daemon(DAEMON_ID).expect("daemon").presence,
        DaemonPresence::Offline
    );
}

#[test]
fn reconnect_supersedes_generation_without_stale_offline_write() {
    let root = TestDir::new();
    let state_path = root.state_path();
    let runtime = enrolled_runtime(&state_path, &["hub.execute"]);
    let (mut first, _) = connect_ready(&runtime, CREDENTIAL);

    let (mut replacement, _) = connect_ready(&runtime, CREDENTIAL);
    assert_eq!(u16::from(wait_for_close(&mut first).code), 4001);
    assert_eq!(
        runtime.daemon(DAEMON_ID).expect("daemon").presence,
        DaemonPresence::Connected
    );
    assert_eq!(runtime.daemon(DAEMON_ID).expect("daemon").generation, 2);

    replacement.close(None).expect("close replacement");
    wait_until(|| runtime.daemon(DAEMON_ID).expect("daemon").presence == DaemonPresence::Offline);
    runtime.stop().expect("stop runtime");

    let restarted = HubDaemonRuntime::bind(&state_path).expect("restart runtime");
    let record = restarted.daemon(DAEMON_ID).expect("durable daemon");
    assert_eq!(record.presence, DaemonPresence::Offline);
    assert_eq!(record.generation, 2);
}

#[test]
fn supersession_rejects_stale_request_and_replacement_remains_usable() {
    let root = TestDir::new();
    let runtime = enrolled_runtime(&root.state_path(), &["hub.execute"]);
    let (mut first, _) = connect_ready(&runtime, CREDENTIAL);
    let stale = runtime
        .request(DAEMON_ID, json!({"operation": "old"}))
        .expect("send request");
    let stale_request = read_json(&mut first);

    let (mut replacement, _) = connect_ready(&runtime, CREDENTIAL);
    assert_eq!(stale.wait(DEADLINE), Err(DaemonRequestError::Superseded));
    let _ = first.send(Message::Text(
        json!({
            "type": "session",
            "message": {
                "type": "response",
                "requestId": stale_request["message"]["requestId"],
                "result": "stale"
            }
        })
        .to_string()
        .into(),
    ));

    let current = runtime
        .request(DAEMON_ID, json!({"operation": "new"}))
        .expect("send replacement request");
    let current_request = read_json(&mut replacement);
    replacement
        .send(Message::Text(
            json!({
                "type": "session",
                "message": {
                    "type": "response",
                    "requestId": current_request["message"]["requestId"],
                    "result": "current"
                }
            })
            .to_string()
            .into(),
        ))
        .expect("send current response");
    assert_eq!(current.wait(DEADLINE), Ok(json!("current")));
}

#[test]
fn invalid_and_revoked_credentials_cannot_reconnect() {
    let root = TestDir::new();
    let runtime = enrolled_runtime(&root.state_path(), &["hub.execute"]);

    assert_eq!(
        rejected_status(connect(runtime.address(), "wrong", true)),
        StatusCode::FORBIDDEN
    );
    let (mut daemon, _) = connect_ready(&runtime, CREDENTIAL);
    runtime.revoke(DAEMON_ID).expect("revoke daemon");
    assert_eq!(u16::from(wait_for_close(&mut daemon).code), 4403);
    let record = runtime.daemon(DAEMON_ID).expect("daemon");
    assert_eq!(record.status, DaemonStatus::Revoked);
    assert_eq!(record.presence, DaemonPresence::Offline);
    assert_eq!(
        rejected_status(connect(runtime.address(), CREDENTIAL, true)),
        StatusCode::FORBIDDEN
    );
}

fn enrolled_runtime(path: &Path, permissions: &[&str]) -> HubDaemonRuntime {
    let runtime = HubDaemonRuntime::bind(path).expect("bind runtime");
    runtime
        .enroll(DaemonEnrollment {
            daemon_id: DAEMON_ID.to_owned(),
            credential: CREDENTIAL.to_owned(),
            permissions: permissions
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        })
        .expect("enroll daemon");
    runtime
}

fn connect_ready(
    runtime: &HubDaemonRuntime,
    credential: &str,
) -> (
    tungstenite::WebSocket<TcpStream>,
    tungstenite::handshake::client::Response,
) {
    let (mut daemon, response) = connect(runtime.address(), credential, true).expect("connect");
    assert_eq!(read_json(&mut daemon)["type"], "hello");
    send_server_info(&mut daemon, &["hub.execute"]);
    wait_until(|| runtime.is_ready(DAEMON_ID));
    (daemon, response)
}

#[allow(clippy::result_large_err)]
fn connect(
    address: SocketAddr,
    credential: &str,
    standard: bool,
) -> Result<
    (
        tungstenite::WebSocket<TcpStream>,
        tungstenite::handshake::client::Response,
    ),
    tungstenite::Error,
> {
    let stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    let mut request = format!("ws://{address}/api/daemons/socket")
        .into_client_request()
        .expect("client request");
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {credential}")).expect("credential header"),
    );
    request
        .headers_mut()
        .insert("x-paseo-daemon-id", HeaderValue::from_static(DAEMON_ID));
    if standard {
        request
            .headers_mut()
            .insert("x-paseo-session-protocol", HeaderValue::from_static("1"));
    }
    match tungstenite::client(request, stream) {
        Ok(connected) => Ok(connected),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(error),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            panic!("blocking handshake cannot be interrupted")
        }
    }
}

fn rejected_status(
    result: Result<
        (
            tungstenite::WebSocket<TcpStream>,
            tungstenite::handshake::client::Response,
        ),
        tungstenite::Error,
    >,
) -> StatusCode {
    let error = result.expect_err("upgrade rejected");
    let tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP rejection, got {error:?}");
    };
    response.status()
}

fn send_server_info(socket: &mut tungstenite::WebSocket<TcpStream>, permissions: &[&str]) {
    socket
        .send(Message::Text(
            json!({
                "type": "session",
                "message": {
                    "type": "status",
                    "payload": {
                        "status": "server_info",
                        "permissions": permissions
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .expect("send server_info");
}

fn read_json(socket: &mut tungstenite::WebSocket<TcpStream>) -> Value {
    loop {
        match socket.read().expect("read websocket message") {
            Message::Text(text) => return serde_json::from_str(&text).expect("JSON message"),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).expect("pong"),
            message => assert!(!message.is_close(), "socket closed before JSON message"),
        }
    }
}

fn wait_for_close(socket: &mut tungstenite::WebSocket<TcpStream>) -> CloseFrame {
    loop {
        match socket.read().expect("read close") {
            Message::Close(Some(close)) => return close,
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).expect("pong"),
            _ => {}
        }
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition missed deadline");
        thread::sleep(Duration::from_millis(10));
    }
}
