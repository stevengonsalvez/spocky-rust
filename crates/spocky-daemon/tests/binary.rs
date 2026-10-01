//! The transport probe binary (`spocky-transport-probe`) as a process: start from a disposable home, serve
//! hello, and stop on SIGTERM or SIGINT. Ports are chosen by the kernel
//! (`127.0.0.1:0`). No test starts a process on 6767 or 6768.

mod common;

use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::Message;
use tungstenite::client::IntoClientRequest;

struct Daemon {
    child: Child,
    home: std::path::PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn spawn(root: &Path, listen: &str) -> Daemon {
    common::assert_disposable_listen(listen);
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("config.json"),
        json!({"daemon": {"listen": listen, "relay": {"enabled": false}}}).to_string(),
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_spocky-transport-probe"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("PASEO_HOME", &home)
        .env("TZ", "UTC")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Daemon { child, home }
}

/// The `listen` the daemon published in `paseo.pid`, once it is up.
fn wait_for_listen(daemon: &Daemon) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = fs::read_to_string(daemon.home.join("paseo.pid"))
            && let Ok(lock) = serde_json::from_str::<Value>(&text)
            && let Some(listen) = lock["listen"].as_str()
        {
            return listen.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never published its address"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn signal(daemon: &Daemon, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(daemon.child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success());
}

fn wait_exit(daemon: &mut Daemon, within: Duration) -> (i32, Duration) {
    let started = Instant::now();
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            return (status.code().unwrap_or(-1), started.elapsed());
        }
        assert!(
            started.elapsed() < within,
            "the daemon did not exit in {within:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hello(listen: &str) -> tungstenite::WebSocket<TcpStream> {
    let stream = TcpStream::connect(listen).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = format!("ws://{listen}/ws").into_client_request().unwrap();
    let (mut ws, _) = tungstenite::client(request, stream).unwrap();
    ws.send(Message::text(
        json!({"type": "hello", "clientId": "bin", "clientType": "cli", "protocolVersion": 1})
            .to_string(),
    ))
    .unwrap();
    let Message::Text(text) = ws.read().unwrap() else {
        panic!("expected text");
    };
    assert!(text.as_str().contains("\"status\":\"server_info\""));
    ws
}

fn stderr_of(daemon: &mut Daemon) -> String {
    use std::io::Read;
    let mut text = String::new();
    if let Some(stderr) = daemon.child.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut text);
    }
    text
}

#[test]
fn sigterm_stops_an_idle_daemon_with_exit_0_and_cleans_the_home() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = spawn(root.path(), "127.0.0.1:0");
    let listen = wait_for_listen(&daemon);
    let port: u16 = listen.rsplit(':').next().unwrap().parse().unwrap();
    assert!(port != 6767 && port != 6768);
    drop(hello(&listen));
    for file in [
        "server-id",
        "daemon-keypair.json",
        "local-credential",
        "paseo.pid",
    ] {
        assert!(daemon.home.join(file).exists(), "{file}");
    }

    signal(&daemon, "TERM");
    let (code, took) = wait_exit(&mut daemon, Duration::from_secs(8));
    assert_eq!(code, 0);
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(!daemon.home.join("local-credential").exists());
    assert!(!daemon.home.join("paseo.pid").exists());
    assert!(daemon.home.join("server-id").exists());
    assert!(stderr_of(&mut daemon).contains("Server closed"));
    assert!(TcpStream::connect(&listen).is_err(), "the port is released");
}

#[test]
fn sigint_stops_the_daemon_the_same_way() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = spawn(root.path(), "127.0.0.1:0");
    wait_for_listen(&daemon);
    signal(&daemon, "INT");
    assert_eq!(wait_exit(&mut daemon, Duration::from_secs(8)).0, 0);
}

#[test]
fn a_client_that_answers_the_close_lets_the_daemon_exit_0() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = spawn(root.path(), "127.0.0.1:0");
    let listen = wait_for_listen(&daemon);
    let mut ws = hello(&listen);
    signal(&daemon, "TERM");
    // Reading drives the client's close reply.
    while ws.read().is_ok() {}
    let (code, took) = wait_exit(&mut daemon, Duration::from_secs(8));
    assert_eq!(code, 0);
    assert!(took < Duration::from_secs(5), "{took:?}");
}

#[test]
fn a_client_that_ignores_the_close_hits_the_ten_second_force_exit_with_code_1() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = spawn(root.path(), "127.0.0.1:0");
    let listen = wait_for_listen(&daemon);
    // Holds the socket open and never reads, so it never answers the close frame.
    let _silent = hello(&listen);
    signal(&daemon, "TERM");
    let (code, took) = wait_exit(&mut daemon, Duration::from_secs(25));
    assert_eq!(
        code, 1,
        "the baseline exits 1 when stop outlives its 10 s timer"
    );
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(15),
        "forced exit after {took:?}"
    );
    assert!(stderr_of(&mut daemon).contains("Forcing shutdown"));
}
