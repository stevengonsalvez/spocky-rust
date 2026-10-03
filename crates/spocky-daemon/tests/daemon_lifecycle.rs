//! Starting and stopping the daemon in-process against disposable homes.
//! Listeners bind port 0. No test starts a daemon on 6767 or 6768.

mod common;

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use spocky_daemon::daemon::{DaemonEnv, NoSessionBackend, resolve_paseo_home, start};
use spocky_daemon::listen::resolve_listen_address;
use spocky_daemon::listen::{ListenTarget, format_listen_target};
use spocky_daemon::log::{JsonLineLogger, Logger, NullLogger};
use spocky_daemon::session_api::{SessionBackend, SessionHandle, SessionOpen};
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

/// The listen address `env` resolves to, as the daemon resolves it. A home
/// with no `daemon.listen` and no override resolves to the production port.
fn effective_listen(env: &DaemonEnv) -> String {
    let persisted = fs::read_to_string(resolve_paseo_home(env).join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|config| config["daemon"]["listen"].as_str().map(str::to_owned));
    resolve_listen_address(
        None,
        env.get("PASEO_LISTEN"),
        persisted.as_deref().or(Some("127.0.0.1:6767")),
        env.get("PORT"),
    )
}

fn start_daemon(env: &DaemonEnv) -> Result<spocky_daemon::daemon::RunningDaemon, String> {
    common::assert_disposable_listen(&effective_listen(env));
    let logger: Arc<dyn Logger> = Arc::new(NullLogger);
    start(env, Arc::new(NoSessionBackend), &logger).map_err(|error| error.0)
}

/// Kills and reaps the child when the test ends, however it ends.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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

/// Records what `SessionBackend::listening` is told, and otherwise does what
/// `NoSessionBackend` does.
#[derive(Default)]
struct ListeningBackend {
    told: std::sync::Mutex<Vec<ListenTarget>>,
    /// Makes `listening` fail with this message.
    fail: Option<&'static str>,
}

impl SessionBackend for ListeningBackend {
    fn open(&self, open: SessionOpen) -> Arc<dyn SessionHandle> {
        NoSessionBackend.open(open)
    }
    fn validate_inbound(&self, message: &Value) -> Result<(), String> {
        NoSessionBackend.validate_inbound(message)
    }
    fn listening(&self, bound: &ListenTarget) -> Result<(), String> {
        self.told.lock().unwrap().push(bound.clone());
        self.fail.map_or(Ok(()), |message| Err(message.to_owned()))
    }
}

/// `bootstrap.ts` sets the agent MCP base url from the bound address in the
/// `'listening'` handler, which is not the configured port for a `:0` listener.
#[test]
fn the_backend_is_told_the_bound_address_after_a_port_zero_bind() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let backend = Arc::new(ListeningBackend::default());
    let logger: Arc<dyn Logger> = Arc::new(NullLogger);
    let daemon = start(
        &env(&home, &[]),
        Arc::clone(&backend) as Arc<dyn SessionBackend>,
        &logger,
    )
    .unwrap();
    let told = backend.told.lock().unwrap().clone();
    assert_eq!(told.len(), 1, "told once: {told:?}");
    let ListenTarget::Tcp { host, port } = &told[0] else {
        panic!("expected a TCP target, got {:?}", told[0]);
    };
    assert_eq!(host, "127.0.0.1");
    assert!(*port != 0 && *port != 6767 && *port != 6768, "{told:?}");
    assert_eq!(format_listen_target(&told[0]), daemon.listen());
    daemon.stop();
}

/// A log sink the test can read back.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Sink {
    /// The records written so far, as JSON.
    fn records(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// The start logged one fatal record and nothing that says it listened or was
/// closed gracefully.
fn assert_only_one_fatal(records: &[Value]) {
    let messages: Vec<&str> = records
        .iter()
        .map(|record| record["msg"].as_str().unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record["level"] == 60)
            .count(),
        1,
        "{messages:?}"
    );
    assert!(
        !messages.contains(&"Server listening") && !messages.contains(&"Server closed"),
        "{messages:?}"
    );
}

/// A failure in the `'listening'` handler rejects the start in the baseline,
/// which undoes it: the credential is deleted, the heartbeat stopped, the
/// server closed. The start fails with the handler's message.
#[test]
fn a_failing_listening_hook_undoes_the_start_and_fails_it_with_the_message() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let backend = Arc::new(ListeningBackend {
        fail: Some("agent MCP base url is unusable"),
        ..ListeningBackend::default()
    });
    let sink = Sink::default();
    let logger: Arc<dyn Logger> = Arc::new(JsonLineLogger::new(sink.clone(), vec![]));
    let error = start(
        &env(&home, &[]),
        Arc::clone(&backend) as Arc<dyn SessionBackend>,
        &logger,
    )
    .err()
    .expect("the start must fail");
    assert_eq!(error.0, "agent MCP base url is unusable");
    // The worker's fatal record comes last, after the undo; no "Server
    // listening" before it, and no "Server closed", which the graceful stop
    // alone writes.
    let records = sink.records();
    let fatal = records.last().unwrap();
    assert_eq!(fatal["level"], 60);
    assert_eq!(fatal["msg"], "Daemon failed to start listening");
    assert_eq!(fatal["err"]["type"], "Error");
    assert_eq!(fatal["err"]["message"], "agent MCP base url is unusable");
    assert_eq!(fatal.as_object().unwrap().len(), 6, "{fatal}");
    assert_only_one_fatal(&records);
    assert!(
        !home.join("local-credential").exists(),
        "credential deleted"
    );
    assert!(!home.join("paseo.pid").exists(), "lock released");
    let told = backend.told.lock().unwrap().clone();
    assert_eq!(told.len(), 1, "told once: {told:?}");
    let ListenTarget::Tcp { host, port } = &told[0] else {
        panic!("expected a TCP target, got {:?}", told[0]);
    };
    let port = u16::try_from(*port).unwrap();
    assert!(port != 6767 && port != 6768);
    assert!(
        TcpStream::connect((host.as_str(), port)).is_err(),
        "the listener is closed"
    );
}

/// `daemon-worker.ts` logs every rejection of `daemon.start()`, a bind error
/// included, at fatal.
#[test]
fn a_bind_error_is_logged_at_fatal() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let holder = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = holder.local_addr().unwrap().port();
    assert!(port != 6767 && port != 6768);
    write_config(&home, &json!({"listen": format!("127.0.0.1:{port}")}));
    let sink = Sink::default();
    let logger: Arc<dyn Logger> = Arc::new(JsonLineLogger::new(sink.clone(), vec![]));
    let error = start(&env(&home, &[]), Arc::new(NoSessionBackend), &logger)
        .err()
        .expect("the port is taken");
    assert_eq!(
        error.0,
        format!("listen EADDRINUSE: address already in use 127.0.0.1:{port}")
    );
    let records = sink.records();
    let fatal = records.last().unwrap();
    assert_eq!(fatal["level"], 60);
    assert_eq!(fatal["msg"], "Daemon failed to start listening");
    assert_eq!(fatal["err"]["message"], error.0.as_str());
    assert_eq!(fatal["err"]["code"], "EADDRINUSE");
    assert_eq!(fatal["err"]["syscall"], "listen");
    assert_eq!(fatal["err"]["address"], "127.0.0.1");
    assert_eq!(fatal["err"]["port"], port);
    assert_only_one_fatal(&records);
    assert!(!home.join("local-credential").exists());
    assert!(!home.join("paseo.pid").exists());
}

#[test]
fn a_home_held_by_another_live_process_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let holder = KillOnDrop(
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap(),
    );
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let lock = json!({
        "pid": holder.0.id(),
        "startedAt": spocky_daemon::iso_time::to_iso_string(i64::try_from(started).unwrap()),
        "hostname": "x", "uid": 1, "listen": "127.0.0.1:9", "heartbeat": true
    });
    fs::write(home.join("paseo.pid"), lock.to_string()).unwrap();
    let error = start_daemon(&env(&home, &[])).err().expect("must refuse");
    assert!(
        error.starts_with(&format!(
            "Another Paseo daemon is already running (PID {}, started ",
            holder.0.id()
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
fn a_fresh_home_writes_the_default_config_and_an_explicit_listen_overrides_it() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("fresh");
    let daemon = start_daemon(&env(&home, &[("PASEO_LISTEN", "127.0.0.1:0")])).unwrap();
    let config = fs::read_to_string(home.join("config.json")).unwrap();
    assert!(
        config.contains("\"listen\": \"127.0.0.1:6767\""),
        "{config}"
    );
    assert!(!daemon.listen().ends_with(":6767"), "{}", daemon.listen());
    daemon.stop();
}

const HASH: &str = "$2b$12$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234";

#[test]
fn a_configured_password_is_refused_until_bcrypt_is_available() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(
        &home,
        &json!({"listen": "127.0.0.1:0", "auth": {"password": HASH}}),
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
fn a_blank_environment_password_is_unset_and_an_empty_config_password_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let daemon = start_daemon(&env(&home, &[("PASEO_PASSWORD", "   ")])).unwrap();
    daemon.stop();
    for bad in ["", "not-a-hash"] {
        write_config(
            &home,
            &json!({"listen": "127.0.0.1:0", "auth": {"password": bad}}),
        );
        let error = start_daemon(&env(&home, &[]))
            .err()
            .expect("must refuse, not open the daemon");
        assert!(
            error.contains("daemon.auth.password: Expected a bcrypt hash"),
            "{error}"
        );
        assert!(!home.join("paseo.pid").exists());
    }
}

#[test]
fn debug_output_of_the_environment_never_shows_values() {
    let shown = format!(
        "{:?}",
        env(Path::new("/tmp/h"), &[("PASEO_PASSWORD", "hunter2")])
    );
    assert!(shown.contains("PASEO_PASSWORD"));
    assert!(!shown.contains("hunter2"), "{shown}");
}

#[test]
fn a_failed_lock_publication_undoes_the_start() {
    use spocky_daemon::daemon::start_with;
    use spocky_daemon::pid_lock::PidLockError;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    write_config(&home, &json!({"listen": "127.0.0.1:0"}));
    let seen_listen = std::sync::Mutex::new(String::new());
    let logger: Arc<dyn Logger> = Arc::new(NullLogger);
    let result = start_with(
        &env(&home, &[]),
        Arc::new(NoSessionBackend),
        &logger,
        &|_, patch| {
            if let spocky_daemon::pid_lock::PidLockPatch::Listening { listen, .. } = patch {
                *seen_listen.lock().unwrap() = listen.clone();
            }
            Err(PidLockError::Io(std::io::Error::other("disk full")))
        },
    );
    assert_eq!(result.err().expect("must fail").0, "disk full");
    let listen = seen_listen.lock().unwrap().clone();
    assert!(!listen.is_empty());
    assert!(
        !home.join("local-credential").exists(),
        "credential removed"
    );
    assert!(!home.join("paseo.pid").exists(), "lock released");
    assert!(TcpStream::connect(&listen).is_err(), "listener closed");
    let again = start_daemon(&env(&home, &[])).unwrap();
    again.stop();
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
    assert_eq!(
        start_daemon(&env(&home, &[])).err().unwrap(),
        "options.port should be >= 0 and < 65536. Received type number (70000)."
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
