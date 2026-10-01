//! Starting and stopping the daemon in-process against disposable homes.
//! Listeners bind port 0; the reserved daemon ports are only used to prove they
//! are refused.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use spocky_daemon::daemon::{DaemonEnv, NoSessionBackend, resolve_paseo_home, start};
use spocky_daemon::log::{Logger, NullLogger};
use tungstenite::Message;
use tungstenite::client::IntoClientRequest;

fn env(home: &Path, extra: &[(&str, &str)]) -> DaemonEnv {
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("PASEO_HOME".to_owned(), home.display().to_string());
    for (name, value) in extra {
        vars.insert((*name).to_owned(), (*value).to_owned());
    }
    DaemonEnv::new(
        vars,
        PathBuf::from("/"),
        Some(PathBuf::from("/Users/example")),
    )
}

fn write_config(home: &Path, daemon: &Value) {
    fs::create_dir_all(home).unwrap();
    fs::write(
        home.join("config.json"),
        json!({ "daemon": daemon }).to_string(),
    )
    .unwrap();
}

fn start_daemon(env: &DaemonEnv) -> Result<spocky_daemon::daemon::RunningDaemon, String> {
    let logger: Arc<dyn Logger> = Arc::new(NullLogger);
    start(env, Arc::new(NoSessionBackend), &logger).map_err(|error| error.0)
}

fn get(listen: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(listen).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {listen}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
}

#[test]
fn a_started_daemon_publishes_its_identity_and_serves_hello() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(
        &home,
        &json!({"listen": "127.0.0.1:0", "relay": {"enabled": false}}),
    );
    let daemon = start_daemon(&env(&home, &[])).unwrap();
    let listen = daemon.listen().to_owned();
    let port: u16 = listen.rsplit(':').next().unwrap().parse().unwrap();
    assert!(port != 0 && port != 6767 && port != 6768);

    for file in [
        "config.json",
        "paseo.pid",
        "server-id",
        "daemon-keypair.json",
        "local-credential",
    ] {
        assert!(home.join(file).exists(), "{file}");
    }
    let lock: Value =
        serde_json::from_str(&fs::read_to_string(home.join("paseo.pid")).unwrap()).unwrap();
    assert_eq!(lock["listen"], listen.as_str());
    assert_eq!(lock["serverId"], daemon.server_id());
    assert_eq!(lock["pid"], std::process::id());
    assert_eq!(
        fs::read_to_string(home.join("server-id")).unwrap(),
        format!("{}\n", daemon.server_id())
    );
    assert!(daemon.has_local_credential());

    let status = get(&listen, "/api/status");
    assert!(
        status.contains(&format!("\"serverId\":\"{}\"", daemon.server_id())),
        "{status}"
    );
    assert!(status.contains(&format!("\"listen\":\"{listen}\"")));

    let request = format!("ws://{listen}/ws").into_client_request().unwrap();
    let stream = TcpStream::connect(&listen).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (mut ws, _) = tungstenite::client(request, stream).unwrap();
    ws.send(Message::text(
        json!({"type":"hello","clientId":"c","clientType":"cli","protocolVersion":1}).to_string(),
    ))
    .unwrap();
    let Message::Text(text) = ws.read().unwrap() else {
        panic!("expected text");
    };
    let frame: Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(frame["message"]["payload"]["serverId"], daemon.server_id());
    assert_eq!(frame["message"]["payload"]["version"], "0.10.0");
    drop(ws);

    daemon.stop();
    assert!(!home.join("local-credential").exists());
    assert!(!home.join("paseo.pid").exists());
    assert!(home.join("server-id").exists());
}

#[test]
fn a_home_held_by_another_live_process_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let mut holder = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let lock = json!({
        "pid": holder.id(),
        "startedAt": spocky_daemon::iso_time::to_iso_string(i64::try_from(started).unwrap()),
        "hostname": "x", "uid": 1, "listen": "127.0.0.1:9", "heartbeat": true
    });
    fs::write(home.join("paseo.pid"), lock.to_string()).unwrap();
    let error = start_daemon(&env(&home, &[])).err().expect("must refuse");
    let _ = holder.kill();
    holder.wait().unwrap();
    assert!(
        error.starts_with(&format!(
            "Another Paseo daemon is already running (PID {}, started ",
            holder.id()
        )),
        "{error}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(home.join("paseo.pid")).unwrap())
            .unwrap(),
        lock,
        "the holder's lock is untouched"
    );
}

#[test]
fn the_listen_address_precedence_is_env_then_config_then_port() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:1"}));
    let daemon = start_daemon(&env(&home, &[("PASEO_LISTEN", "127.0.0.1:0")])).unwrap();
    assert!(daemon.listen().starts_with("127.0.0.1:"));
    assert_ne!(daemon.listen(), "127.0.0.1:1");
    daemon.stop();
}

#[test]
fn the_production_ports_are_refused_and_the_lock_is_released() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    for listen in ["127.0.0.1:6767", "127.0.0.1:6768", "6767"] {
        write_config(&home, &json!({ "listen": listen }));
        let error = start_daemon(&env(&home, &[])).err().expect("must refuse");
        assert!(error.starts_with("Refusing to listen on "), "{error}");
        assert!(error.contains("production daemon"));
        assert!(
            !home.join("paseo.pid").exists(),
            "lock released for {listen}"
        );
        assert!(!home.join("local-credential").exists());
    }
}

#[test]
fn a_fresh_home_with_the_default_config_is_refused_not_bound() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("fresh");
    let error = start_daemon(&env(&home, &[])).err().expect("must refuse");
    assert!(error.contains("127.0.0.1:6767"), "{error}");
    assert!(
        home.join("config.json").exists(),
        "first-run config is still written"
    );
}

#[test]
fn a_configured_password_is_refused_until_bcrypt_is_available() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(
        &home,
        &json!({"listen": "127.0.0.1:0", "auth": {"password": "$2a$12$x"}}),
    );
    let error = start_daemon(&env(&home, &[])).err().expect("must refuse");
    assert!(error.contains("cannot verify bcrypt"));
    assert!(!home.join("paseo.pid").exists());
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let error = start_daemon(&env(&home, &[("PASEO_PASSWORD", " secret ")]))
        .err()
        .expect("must refuse");
    assert!(error.contains("cannot verify bcrypt"));
}

#[test]
fn bad_listen_strings_fail_with_the_pinned_messages() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "nonsense"}));
    assert_eq!(
        start_daemon(&env(&home, &[])).err().unwrap(),
        "Invalid listen string: nonsense"
    );
    write_config(&home, &json!({"listen": "127.0.0.1:70000"}));
    assert!(
        start_daemon(&env(&home, &[]))
            .err()
            .unwrap()
            .starts_with("options.port should be >= 0 and < 65536")
    );
    assert!(!home.join("paseo.pid").exists());
    assert!(!home.join("local-credential").exists());
}

#[test]
fn a_port_already_in_use_fails_and_cleans_up() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    write_config(&home, &json!({ "listen": format!("127.0.0.1:{port}") }));
    let error = start_daemon(&env(&home, &[])).err().unwrap();
    assert!(error.contains("EADDRINUSE"), "{error}");
    assert!(!home.join("paseo.pid").exists());
    assert!(!home.join("local-credential").exists());
}

#[test]
fn a_stale_lock_is_replaced_on_start() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    fs::write(
        home.join("paseo.pid"),
        json!({"pid": dead, "startedAt": "2026-10-01T10:00:00.000Z", "hostname": "x", "uid": 1, "listen": null})
            .to_string(),
    )
    .unwrap();
    let daemon = start_daemon(&env(&home, &[])).unwrap();
    let lock: Value =
        serde_json::from_str(&fs::read_to_string(home.join("paseo.pid")).unwrap()).unwrap();
    assert_eq!(lock["pid"], std::process::id());
    daemon.stop();
}

#[cfg(unix)]
#[test]
fn a_unix_socket_listen_address_is_served_and_removed_on_stop() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let socket = root.path().join("d.sock");
    write_config(&home, &json!({ "listen": socket.display().to_string() }));
    let daemon = start_daemon(&env(&home, &[])).unwrap();
    assert_eq!(daemon.listen(), socket.display().to_string());
    assert!(socket.exists());
    daemon.stop();
    assert!(!socket.exists());
}

#[test]
fn the_home_resolves_like_path_resolve() {
    let make = |home: Option<&str>| {
        let mut vars = HashMap::new();
        if let Some(home) = home {
            vars.insert("PASEO_HOME".to_owned(), home.to_owned());
        }
        DaemonEnv::new(
            vars,
            PathBuf::from("/work/dir"),
            Some(PathBuf::from("/Users/me")),
        )
    };
    assert_eq!(
        resolve_paseo_home(&make(None)),
        PathBuf::from("/Users/me/.paseo")
    );
    assert_eq!(
        resolve_paseo_home(&make(Some("~"))),
        PathBuf::from("/Users/me")
    );
    assert_eq!(
        resolve_paseo_home(&make(Some("~/x/../y"))),
        PathBuf::from("/Users/me/y")
    );
    assert_eq!(
        resolve_paseo_home(&make(Some("rel/./h/"))),
        PathBuf::from("/work/dir/rel/h")
    );
    assert_eq!(
        resolve_paseo_home(&make(Some("/a/b/../c"))),
        PathBuf::from("/a/c")
    );
    assert_eq!(
        resolve_paseo_home(&make(Some("~other"))),
        PathBuf::from("/work/dir/~other")
    );
}
