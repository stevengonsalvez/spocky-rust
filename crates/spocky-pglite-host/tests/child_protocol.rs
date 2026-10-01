//! Drives `spocky-pglite-host-child` through the retained child protocol.
//! `SPOCKY_PGLITE_PACKAGE` and `SPOCKY_HUB_MIGRATIONS` are required.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

fn data_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "spocky-pglite-child-{}-{nonce}",
        std::process::id()
    ))
}

fn send(stdin: &mut ChildStdin, value: &Value) {
    let bytes = value.to_string().into_bytes();
    let length = u32::try_from(bytes.len()).expect("frame length");
    stdin
        .write_all(&length.to_be_bytes())
        .expect("write header");
    stdin.write_all(&bytes).expect("write body");
    stdin.flush().expect("flush");
}

fn receive(stdout: &mut ChildStdout) -> Value {
    let mut header = [0_u8; 4];
    stdout.read_exact(&mut header).expect("read header");
    let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
    stdout.read_exact(&mut body).expect("read body");
    serde_json::from_slice(&body).expect("reply is JSON")
}

/// Kills and reaps the child this test started when the test ends, pass or
/// fail. Only the PID of this spawned child is signalled.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(data: &PathBuf, maximum: usize) -> (ChildGuard, ChildStdin, ChildStdout) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_spocky-pglite-host-child"))
        .arg("ignored-adapter-path")
        .arg(required("SPOCKY_PGLITE_PACKAGE"))
        .arg(required("SPOCKY_HUB_MIGRATIONS"))
        .arg(data)
        .arg(maximum.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    (ChildGuard(child), stdin, stdout)
}

#[test]
fn serves_the_retained_child_protocol() {
    let data = data_directory();
    let (mut child, mut stdin, mut stdout) = spawn(&data, 64 * 1024);

    let hello = receive(&mut stdout);
    assert_eq!(hello["id"], 0);
    assert_eq!(hello["ok"], true);
    assert_eq!(hello["result"]["package"], "@electric-sql/pglite");
    assert_eq!(hello["result"]["packageVersion"], "0.5.4");

    send(
        &mut stdin,
        &json!({"id": 1, "operation": "query", "sql": "select $1::int + 1 as n, '{\"b\":1,\"a\":2}'::json as j",
                "params": [{"type": "numeric", "value": "41"}]}),
    );
    let reply = receive(&mut stdout);
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["result"]["columns"], json!(["n", "j"]));
    assert_eq!(
        reply["result"]["rows"][0][0],
        json!({"type": "numeric", "value": "42"})
    );
    // JSON keys keep the order PostgreSQL returned, as JSON.parse does.
    assert_eq!(
        serde_json::to_string(&reply["result"]["rows"][0][1]["value"]).expect("json"),
        r#"{"b":1,"a":2}"#
    );

    send(
        &mut stdin,
        &json!({"id": 2, "operation": "select * from missing"}),
    );
    let reply = receive(&mut stdout);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["code"], "UNKNOWN_OPERATION");

    send(
        &mut stdin,
        &json!({"id": 3, "operation": "query", "sql": "select * from missing_table"}),
    );
    let reply = receive(&mut stdout);
    assert_eq!(reply["error"]["code"], "42P01");
    assert_eq!(reply["error"]["details"]["severity"], "ERROR");

    send(
        &mut stdin,
        &json!({"id": 4, "operation": "query", "sql": "select repeat('x', 70000)"}),
    );
    let reply = receive(&mut stdout);
    assert_eq!(reply["id"], 4);
    assert_eq!(reply["error"]["code"], "FRAME_TOO_LARGE");

    send(&mut stdin, &json!({"id": 5, "operation": "close"}));
    let reply = receive(&mut stdout);
    assert_eq!(reply, json!({"id": 5, "ok": true, "result": null}));
    assert_eq!(child.0.wait().expect("exit").code(), Some(0));
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn stall_reads_stops_reading_before_the_next_frame_body() {
    let data = data_directory();
    let (child, mut stdin, mut stdout) = spawn(&data, 8 * 1024 * 1024);
    assert_eq!(receive(&mut stdout)["ok"], true);

    send(&mut stdin, &json!({"id": 1, "operation": "stallReads"}));
    assert_eq!(
        receive(&mut stdout),
        json!({"id": 1, "ok": true, "result": null})
    );

    // A 4 MiB frame cannot be delivered once the child stops reading: the
    // write stays blocked on the full pipe, as with Node's stdin.pause().
    let (done, finished) = mpsc::channel();
    let writer = thread::spawn(move || {
        let payload = "x".repeat(4 * 1024 * 1024);
        let frame = json!({"id": 2, "operation": "query", "sql": "select $1::text",
                           "params": [{"type": "string", "value": payload}]});
        let bytes = frame.to_string().into_bytes();
        let length = u32::try_from(bytes.len()).expect("frame length");
        let written = stdin
            .write_all(&length.to_be_bytes())
            .and_then(|()| stdin.write_all(&bytes));
        let _ = done.send(written.is_ok());
    });
    assert_eq!(
        finished.recv_timeout(Duration::from_secs(3)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the child read the frame after stallReads"
    );
    drop(child);
    writer.join().expect("writer thread");
    let _ = std::fs::remove_dir_all(&data);
}
