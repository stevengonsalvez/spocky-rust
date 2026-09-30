use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::http::{HubHttpService, serve_one};
use paseo_hub_pilot::{
    AccountId, Bootstrap, BrowserAccountStatus, EmbeddedFileStore, HubPilot, OrganizationId,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("paseo-hub-http-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

#[test]
fn packet_level_auth_gate_matches_status_body_cookie_and_restart_state() {
    let root = TestDir::new();
    let path = root.0.join("hub.json");
    let mut hub =
        HubPilot::open(EmbeddedFileStore::open(&path).expect("open store")).expect("open hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "http-runtime-secret-at-least-32-characters".into(),
        owner: AccountId::from("owner@example.test"),
        organization: OrganizationId::from("organization-1"),
        temporary_password: "temporary-password".into(),
    })
    .expect("bootstrap");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind random local port");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let mut service = HubHttpService::new(hub);
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("bound read");
            serve_one(&mut stream, &mut service).expect("serve request");
        }
    });

    let signed_out = request(address, "GET", "/api/auth/paseo/state", None, None);
    assert!(signed_out.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(signed_out.ends_with(r#"{"status":"signedOut"}"#));

    let signed_in = request(
        address,
        "POST",
        "/api/auth/sign-in/email",
        None,
        Some(r#"{"email":"owner@example.test","password":"temporary-password"}"#),
    );
    assert!(signed_in.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(
        signed_in.contains(
            "set-cookie: paseo_session=hub-session-1; Path=/; HttpOnly; SameSite=Lax\r\n"
        )
    );
    assert!(signed_in.ends_with(r#"{"status":"passwordChangeRequired"}"#));
    let cookie = "paseo_session=hub-session-1";

    let gated = request(
        address,
        "POST",
        "/api/auth/paseo/complete-app-setup",
        Some(cookie),
        Some("{}"),
    );
    assert!(gated.starts_with("HTTP/1.1 403 Forbidden\r\n"));
    assert!(gated.ends_with(r#"{"error":"password_change_required"}"#));

    let changed = request(
        address,
        "POST",
        "/api/auth/paseo/change-password",
        Some(cookie),
        Some(r#"{"currentPassword":"temporary-password","newPassword":"replacement-password"}"#),
    );
    assert!(changed.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(changed.ends_with(r#"{"status":"appSetupRequired"}"#));

    let completed = request(
        address,
        "POST",
        "/api/auth/paseo/complete-app-setup",
        Some(cookie),
        Some("{}"),
    );
    assert!(completed.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(completed.ends_with(r#"{"status":"active"}"#));
    server.join().expect("server thread");

    let restarted =
        HubPilot::open(EmbeddedFileStore::open(path).expect("reopen store")).expect("restart hub");
    assert_eq!(
        restarted
            .browser_account_status(Some(&paseo_hub_pilot::SessionToken::from("hub-session-1"))),
        BrowserAccountStatus::Active
    );
}

fn request(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> String {
    let body = body.unwrap_or_default();
    let mut stream = TcpStream::connect(address).expect("connect to local Hub pilot");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("bound response read");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: {address}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
        body.len()
    )
    .expect("write request headers");
    if let Some(cookie) = cookie {
        write!(stream, "cookie: {cookie}\r\n").expect("write cookie");
    }
    write!(stream, "connection: close\r\n\r\n{body}").expect("write request body");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}
