//! Terminal worker framing against the real pinned worker. The parent
//! forks the worker with `serialization: "advanced"` (V8 structured clone
//! behind a 4-byte length), so this test starts the pinned
//! `terminal-worker-process.js` on the pinned Node with
//! `NODE_CHANNEL_SERIALIZATION_MODE=advanced`, sends requests built by
//! [`WorkerRequest`], and reads every frame with [`FrameDecoder`]. Each frame
//! must parse into a [`WorkerMessage`] whose re-encoding is the same bytes,
//! and the pinned worker must answer every request, including its exact
//! error text for a missing workspace. Requests are also compared byte for
//! byte with `v8.serialize` of the objects the parent builds
//! (`{ ...input, requestId }`). All reads and the worker exit are bounded;
//! only the recorded worker pid is ever signalled.

mod support;

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_terminal::worker_protocol::{
    Frame, FrameDecoder, WorkerMessage, WorkerRequest, encode_frame, parse_frame,
};

const READ_DEADLINE: Duration = Duration::from_secs(20);

#[test]
fn requests_put_type_first_and_request_id_last() {
    let request = WorkerRequest::CaptureTerminal {
        terminal_id: "t1".to_owned(),
        start: Some(-2.0),
        end: None,
        strip_ansi: Some(false),
    };
    let mut decoder = FrameDecoder::new();
    let frames = decoder.push(&encode_frame(&request.to_value("r1")));
    assert_eq!(frames.len(), 1);
    assert_eq!(
        stringify(frames[0].value.as_ref().expect("value")),
        "{\"type\":\"captureTerminal\",\"terminalId\":\"t1\",\"start\":-2,\"stripAnsi\":false,\"requestId\":\"r1\"}"
    );
}

#[test]
fn frames_split_across_reads_and_keep_undefined_keys() {
    let first = WorkerMessage::Response {
        request_id: "a".to_owned(),
        outcome: Ok(None),
    };
    let second = WorkerMessage::TerminalTitleChange {
        terminal_id: "t".to_owned(),
        title: None,
    };
    let mut bytes = encode_frame(&first.to_value());
    bytes.extend(encode_frame(&second.to_value()));
    let (head, tail) = bytes.split_at(bytes.len() / 2);
    let mut decoder = FrameDecoder::new();
    let mut frames: Vec<Frame> = decoder.push(head);
    frames.extend(decoder.push(tail));
    assert_eq!(frames.len(), 2);
    let parsed: Vec<WorkerMessage> = frames
        .iter()
        .map(|frame| parse_frame(frame.value.as_ref().expect("value")).expect("message"))
        .collect();
    assert_eq!(parsed, [first, second]);
    // `title: undefined` is on the wire, where JSON would drop the key.
    let title = frames[1].value.as_ref().expect("value");
    assert_eq!(title.get("title"), Some(&JsValue::Undefined));
}

#[test]
fn a_truthy_ok_is_a_success() {
    let mut object = JsObject::new();
    object.insert("type", JsValue::String("response".to_owned()));
    object.insert("requestId", JsValue::String("x".to_owned()));
    object.insert("ok", JsValue::Number(1.0));
    assert_eq!(
        parse_frame(&JsValue::Object(object)).expect("message"),
        WorkerMessage::Response {
            request_id: "x".to_owned(),
            outcome: Ok(None),
        }
    );
}

struct Worker {
    child: Child,
    channel: UnixStream,
    decoder: FrameDecoder,
    frames: Vec<Frame>,
    home: TempHome,
}

struct TempHome(PathBuf);

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Drop for Worker {
    /// A failed assertion must not leave the worker running: kill the
    /// recorded child, then reap it.
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Worker {
    fn start(pinned: &support::Pinned) -> Self {
        let home =
            std::env::temp_dir().join(format!("spocky-terminal-worker-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("home");
        let home = TempHome(home.canonicalize().expect("canonical home"));
        let (channel, child_end) = UnixStream::pair().expect("socketpair");
        // The child end must survive exec and must not sit on fd 0..2, which
        // the child's stdio replaces (a runner may start tests with stdin
        // closed); the parent end stays close-on-exec.
        let child_end = rustix::io::fcntl_dupfd_cloexec(&child_end, 3).expect("channel fd >= 3");
        rustix::io::fcntl_setfd(&child_end, rustix::io::FdFlags::empty()).expect("inheritable");
        let child = Command::new(&pinned.node)
            .arg(pinned.terminal_dir.join("terminal-worker-process.js"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home.0)
            .env("NODE_CHANNEL_FD", child_end.as_raw_fd().to_string())
            .env("NODE_CHANNEL_SERIALIZATION_MODE", "advanced")
            .current_dir(&home.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start pinned worker");
        drop(child_end);
        channel
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("read timeout");
        Self {
            child,
            channel,
            decoder: FrameDecoder::new(),
            frames: Vec::new(),
            home,
        }
    }

    fn send(&mut self, request: &WorkerRequest, request_id: &str) {
        self.channel
            .write_all(&encode_frame(&request.to_value(request_id)))
            .expect("send request");
    }

    /// Reads until `done` accepts a message; returns everything read.
    fn read_until(&mut self, mut done: impl FnMut(&WorkerMessage) -> bool) -> Vec<WorkerMessage> {
        let deadline = Instant::now() + READ_DEADLINE;
        let mut messages = Vec::new();
        let mut buffer = vec![0u8; 65536];
        loop {
            assert!(
                Instant::now() < deadline,
                "no matching worker frame in time: {messages:?}"
            );
            let count = match self.channel.read(&mut buffer) {
                Ok(0) => panic!("worker closed the channel: {messages:?}"),
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => panic!("read worker channel: {error}"),
            };
            for frame in self.decoder.push(&buffer[..count]) {
                let message = parse_frame(frame.value.as_ref().expect("worker frame"))
                    .expect("worker message");
                // Byte-exact framing: re-encoding gives the frame back.
                assert_eq!(encode_frame(&message.to_value()), frame.raw);
                self.frames.push(frame);
                let matched = done(&message);
                messages.push(message);
                if matched {
                    return messages;
                }
            }
        }
    }

    fn response(
        &mut self,
        request_id: &str,
    ) -> (Vec<WorkerMessage>, Result<Option<JsValue>, String>) {
        let mut messages = self.read_until(|message| {
            matches!(message, WorkerMessage::Response { request_id: id, .. } if id == request_id)
        });
        let Some(WorkerMessage::Response { outcome, .. }) = messages.pop() else {
            unreachable!("read_until stops on the response");
        };
        (messages, outcome)
    }

    /// Closes the channel (the worker kills its terminals on disconnect) and
    /// waits for the recorded worker pid, killing it after the bound.
    fn stop(mut self) {
        let _ = self.channel.shutdown(std::net::Shutdown::Both);
        let deadline = Instant::now() + READ_DEADLINE;
        while Instant::now() < deadline {
            if self.child.try_wait().expect("try_wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        panic!("pinned worker did not exit after disconnect");
    }
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn missing_workspace_is_rejected(worker: &mut Worker, cwd: &str) {
    worker.send(
        &WorkerRequest::CreateTerminal {
            options: object(vec![("cwd", text(cwd)), ("command", text("/bin/sh"))]),
        },
        "no-workspace",
    );
    let (_, outcome) = worker.response("no-workspace");
    assert_eq!(outcome, Err("workspaceId is required".to_owned()));
}

fn create_terminal(worker: &mut Worker, cwd: &str) {
    worker.send(
        &WorkerRequest::CreateTerminal {
            options: object(vec![
                ("id", text("term-1")),
                ("cwd", text(cwd)),
                ("workspaceId", text("ws-1")),
                ("command", text("/bin/sh")),
                (
                    "args",
                    JsValue::Array(vec![text("-c"), text("printf ready; read x; printf done")]),
                ),
                ("rows", JsValue::Number(5.0)),
                ("cols", JsValue::Number(30.0)),
            ]),
        },
        "create",
    );
    let (before, outcome) = worker.response("create");
    assert!(
        matches!(before.last(), Some(WorkerMessage::TerminalCreated { .. })),
        "terminalCreated precedes the create response: {before:?}"
    );
    let result = outcome.expect("create ok").expect("create result");
    assert_eq!(
        result
            .get("terminal")
            .and_then(|t| t.get("id"))
            .and_then(JsValue::as_str),
        Some("term-1")
    );
    // No wait for "ready": the shell may print it before the worker
    // subscribes, and the terminal buffers input until `read` runs.
}

fn snapshot_shapes(worker: &mut Worker) {
    worker.send(
        &WorkerRequest::GetTerminalState {
            terminal_id: "term-1".to_owned(),
            options: Some(object(vec![("includeWrapFlags", JsValue::Bool(true))])),
        },
        "state",
    );
    let (_, outcome) = worker.response("state");
    let snapshot = outcome.expect("state ok").expect("state result");
    let keys: Vec<String> = snapshot
        .as_object()
        .expect("snapshot object")
        .iter()
        .map(|(key, _)| key.to_owned())
        .collect();
    assert_eq!(keys, ["state", "revision", "replayPreamble"]);

    worker.send(
        &WorkerRequest::GetTerminalState {
            terminal_id: "missing".to_owned(),
            options: None,
        },
        "state-missing",
    );
    let (_, outcome) = worker.response("state-missing");
    assert_eq!(outcome, Ok(Some(JsValue::Null)));
}

fn input_exits_terminal(worker: &mut Worker) {
    worker.send(
        &WorkerRequest::Send {
            terminal_id: "term-1".to_owned(),
            message: object(vec![("type", text("input")), ("data", text("\r"))]),
        },
        "input",
    );
    let (_, outcome) = worker.response("input");
    assert_eq!(outcome, Ok(None));
    let exit = worker.read_until(|message| matches!(message, WorkerMessage::TerminalExit { .. }));
    let Some(WorkerMessage::TerminalExit { terminal_id, info }) = exit.last() else {
        unreachable!("read_until stops on the exit");
    };
    assert_eq!(terminal_id, "term-1");
    assert_eq!(info.get("exitCode").and_then(JsValue::as_f64), Some(0.0));

    worker.send(
        &WorkerRequest::CaptureTerminal {
            terminal_id: "term-1".to_owned(),
            start: None,
            end: None,
            strip_ansi: None,
        },
        "capture",
    );
    let (_, outcome) = worker.response("capture");
    assert_eq!(
        stringify(&outcome.expect("capture ok").expect("capture result")),
        "{\"lines\":[],\"totalLines\":0}"
    );
}

fn plain_requests_succeed(worker: &mut Worker, cwd: &str) {
    for (request, id) in [
        (
            WorkerRequest::RegisterCwdEnv {
                cwd: cwd.to_owned(),
                env: object(vec![("A", text("1"))]),
            },
            "register",
        ),
        (
            WorkerRequest::SetActivity {
                terminal_id: "missing".to_owned(),
                state: "working".to_owned(),
            },
            "activity",
        ),
        (
            WorkerRequest::ClearAttention {
                terminal_id: "missing".to_owned(),
            },
            "attention",
        ),
        (
            WorkerRequest::KillTerminal {
                terminal_id: "missing".to_owned(),
            },
            "kill",
        ),
        (
            WorkerRequest::KillTerminalAndWait {
                terminal_id: "missing".to_owned(),
                options: Some(object(vec![("gracefulTimeoutMs", JsValue::Number(10.0))])),
            },
            "kill-wait",
        ),
        (WorkerRequest::KillAll, "kill-all"),
    ] {
        worker.send(&request, id);
        let (_, outcome) = worker.response(id);
        assert_eq!(outcome, Ok(None), "{id}");
    }
}

#[test]
fn pinned_worker_round_trips_every_frame() {
    let Some(pinned) = support::pinned("worker framing differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut worker = Worker::start(&pinned);
    let cwd = worker.home.0.to_string_lossy().into_owned();
    missing_workspace_is_rejected(&mut worker, &cwd);
    create_terminal(&mut worker, &cwd);
    snapshot_shapes(&mut worker);
    input_exits_terminal(&mut worker);
    plain_requests_succeed(&mut worker, &cwd);
    let frames = worker.frames.clone();
    worker.stop();
    assert!(frames.iter().all(|frame| frame.value.is_ok()));
}

const REQUESTS_NODE_SCRIPT: &str = r#"
import v8 from "node:v8";
const mk = (input, requestId) => ({ ...input, requestId });
const list = [
  mk({ type: "killAll" }, "r0"),
  mk({ type: "killTerminal", terminalId: "t" }, "r1"),
  mk({ type: "clearAttention", terminalId: "t" }, "r2"),
  mk({ type: "setActivity", terminalId: "t", state: "working" }, "r3"),
  mk({ type: "registerCwdEnv", cwd: "/w", env: { A: "1" } }, "r4"),
  mk({ type: "getTerminalState", terminalId: "t" }, "r5"),
  mk({ type: "getTerminalState", terminalId: "t", options: { scrollbackLines: 7 } }, "r6"),
  mk({ type: "killTerminalAndWait", terminalId: "t", options: { gracefulTimeoutMs: 10 } }, "r7"),
  mk({ type: "captureTerminal", terminalId: "t" }, "r8"),
  mk({ type: "captureTerminal", terminalId: "t", start: -2, end: 5, stripAnsi: false }, "r9"),
  mk({ type: "send", terminalId: "t", message: { type: "input", data: "héllo 中" } }, "r10"),
  mk({ type: "send", terminalId: "t", message: { type: "resize", rows: 30, cols: 100 } }, "r11"),
  mk({ type: "createTerminal", options: { cwd: "/w", workspaceId: "ws", rows: 24, cols: 80, id: "t", activityToken: "tok", activityUrl: null } }, "r12"),
];
process.stdout.write(JSON.stringify(list.map((value) => v8.serialize(value).toString("hex"))));
"#;

fn number(value: f64) -> JsValue {
    JsValue::Number(value)
}

#[test]
fn requests_serialize_like_the_parents_objects() {
    let Some(pinned) = support::pinned("worker request serialization differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let requests = [
        WorkerRequest::KillAll,
        WorkerRequest::KillTerminal {
            terminal_id: "t".to_owned(),
        },
        WorkerRequest::ClearAttention {
            terminal_id: "t".to_owned(),
        },
        WorkerRequest::SetActivity {
            terminal_id: "t".to_owned(),
            state: "working".to_owned(),
        },
        WorkerRequest::RegisterCwdEnv {
            cwd: "/w".to_owned(),
            env: object(vec![("A", text("1"))]),
        },
        WorkerRequest::GetTerminalState {
            terminal_id: "t".to_owned(),
            options: None,
        },
        WorkerRequest::GetTerminalState {
            terminal_id: "t".to_owned(),
            options: Some(object(vec![("scrollbackLines", number(7.0))])),
        },
        WorkerRequest::KillTerminalAndWait {
            terminal_id: "t".to_owned(),
            options: Some(object(vec![("gracefulTimeoutMs", number(10.0))])),
        },
        WorkerRequest::CaptureTerminal {
            terminal_id: "t".to_owned(),
            start: None,
            end: None,
            strip_ansi: None,
        },
        WorkerRequest::CaptureTerminal {
            terminal_id: "t".to_owned(),
            start: Some(-2.0),
            end: Some(5.0),
            strip_ansi: Some(false),
        },
        WorkerRequest::Send {
            terminal_id: "t".to_owned(),
            message: object(vec![
                ("type", text("input")),
                ("data", text("h\u{e9}llo \u{4e2d}")),
            ]),
        },
        WorkerRequest::Send {
            terminal_id: "t".to_owned(),
            message: object(vec![
                ("type", text("resize")),
                ("rows", number(30.0)),
                ("cols", number(100.0)),
            ]),
        },
        WorkerRequest::CreateTerminal {
            options: object(vec![
                ("cwd", text("/w")),
                ("workspaceId", text("ws")),
                ("rows", number(24.0)),
                ("cols", number(80.0)),
                ("id", text("t")),
                ("activityToken", text("tok")),
                ("activityUrl", JsValue::Null),
            ]),
        },
    ];
    let expected = support::run_node(&pinned, REQUESTS_NODE_SCRIPT, &[]);
    let actual = stringify(&JsValue::Array(
        requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                let bytes = spocky_terminal::v8_serialize::serialize(
                    &request.to_value(&format!("r{index}")),
                );
                JsValue::String(bytes.iter().fold(String::new(), |mut hex, byte| {
                    use std::fmt::Write as _;
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }))
            })
            .collect(),
    ));
    assert_eq!(actual, expected);
}
