//! Terminal worker framing against the real pinned worker: this test starts
//! the pinned `terminal-worker-process.js` on the pinned Node with a JSON IPC
//! channel (`NODE_CHANNEL_FD`, `serialization: "json"`), sends requests built
//! by [`WorkerRequest`], and reads every frame with [`FrameDecoder`]. Each
//! frame must parse into a [`WorkerMessage`] whose re-encoding is the same
//! bytes, and the pinned worker must answer every request, including its
//! exact error text for a missing workspace. All reads and the worker exit
//! are bounded; only the recorded worker pid is ever signalled.

mod support;

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::worker_protocol::{
    FrameDecoder, WorkerMessage, WorkerRequest, encode_frame, parse_frame,
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
    assert_eq!(
        encode_frame(&request.to_value("r1")),
        "{\"type\":\"captureTerminal\",\"terminalId\":\"t1\",\"start\":-2,\"stripAnsi\":false,\"requestId\":\"r1\"}\n"
    );
    assert_eq!(
        encode_frame(&WorkerRequest::KillAll.to_value("r2")),
        "{\"type\":\"killAll\",\"requestId\":\"r2\"}\n"
    );
}

#[test]
fn frames_split_on_newlines_across_reads() {
    let mut decoder = FrameDecoder::new();
    assert!(
        decoder
            .push(b"{\"type\":\"response\",\"requestId\":\"a\",")
            .is_empty()
    );
    let frames = decoder.push(b"\"ok\":true}\n{\"type\":\"response\",\"requestId\":\"b\",\"ok\":false,\"error\":\"x\"}\n{");
    assert_eq!(frames.len(), 2);
    assert_eq!(
        parse_frame(&frames[0]).expect("frame"),
        WorkerMessage::Response {
            request_id: "a".to_owned(),
            outcome: Ok(None),
        }
    );
    assert_eq!(
        parse_frame(&frames[1]).expect("frame"),
        WorkerMessage::Response {
            request_id: "b".to_owned(),
            outcome: Err("x".to_owned()),
        }
    );
}

struct Worker {
    child: Child,
    channel: UnixStream,
    decoder: FrameDecoder,
    frames: Vec<String>,
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
            .env("NODE_CHANNEL_SERIALIZATION_MODE", "json")
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
            .write_all(encode_frame(&request.to_value(request_id)).as_bytes())
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
                let message = parse_frame(&frame).expect("worker frame");
                // Byte-exact framing: re-encoding gives the frame back.
                assert_eq!(encode_frame(&message.to_value()), format!("{frame}\n"));
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
    for frame in frames {
        parse(&frame).expect("json frame");
    }
}
