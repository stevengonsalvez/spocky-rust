//! Terminal output streams against the pinned `TerminalSessionController`:
//! the same scripted scenarios (subscribe, terminal messages, snapshot reads
//! settling or failing, timers, backpressure readings, exit) run through the
//! controller from the pinned dist on the pinned Node, with a fake terminal
//! manager, a fake clock and the pinned `SessionDelivery`, and through
//! [`TerminalStreams`] with a recording host. The traces, in order, must
//! match as text: terminal subscribe and unsubscribe calls, snapshot reads
//! with their options, every message and binary frame the client receives,
//! and the timers the coalescers arm and clear.
//!
//! Large frames are traced as a length and checksum. The only normalization
//! is the generated `subscriptionId` of an owned subscription, which becomes
//! `<id>` on both sides.

mod support;

use std::collections::{HashSet, VecDeque};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::restore::{RestoreMode, RestoreOptions, SnapshotOptions};
use spocky_terminal::session::{ServerMessage, StateSnapshot};
use spocky_terminal::stream::{StreamHost, TerminalStreams};
use spocky_wire::{TerminalCursor, TerminalState};

const NODE_SCRIPT: &str = r#"
const [terminalDir, scenarioJson] = process.argv.slice(1);
const s = JSON.parse(scenarioJson);
const trace = [];
let now = 0;
let nextTimerId = 1;
const timers = new Map();
globalThis.setTimeout = (callback, ms = 0) => {
  const id = nextTimerId++;
  timers.set(id, { due: now + ms, callback });
  trace.push(["timer.schedule", ms]);
  return id;
};
globalThis.clearTimeout = (id) => {
  if (timers.delete(id)) trace.push(["timer.clear"]);
};
Date.now = () => now;
const { TerminalSessionController } = await import(`${terminalDir}/terminal-session-controller.js`);
const { SessionDelivery } = await import(`${terminalDir}/../server/session/owned-subscriptions/index.js`);
const { decodeTerminalStreamFrame } = await import(`${terminalDir}/../../../../protocol/dist/binary-frames/index.js`);

const summarize = (bytes) => {
  const buffer = Buffer.from(bytes);
  if (buffer.length <= 256) return buffer.toString("hex");
  let sum = 0;
  for (const byte of buffer) sum = (sum + byte) % 65521;
  return `${buffer.length}:${buffer.subarray(0, 16).toString("hex")}:${sum}`;
};
const normalize = (message) => {
  const copy = JSON.parse(JSON.stringify(message));
  if (copy.payload && copy.payload.subscriptionId !== undefined) copy.payload.subscriptionId = "<id>";
  return copy;
};
const STATE = { rows: 1, cols: 2, grid: [[{ char: "a" }, { char: "b" }]], scrollback: [], cursor: { row: 0, col: 2 } };
let wrap = false;
let buffered = 0;
let exists = true;
let terminalListener = null;
let exitListener = null;
const pending = [];
const terminal = {
  id: "term-1",
  subscribe: (listener, options) => {
    const mode = options?.initialSnapshot ?? "state";
    trace.push(["terminal.subscribe", mode]);
    terminalListener = listener;
    queueMicrotask(() => {
      if (mode === "ready") listener({ type: "snapshotReady", revision: s.initialRevision, replayPreamble: "" });
      else listener({ type: "snapshot", state: STATE, revision: s.initialRevision });
    });
    return () => trace.push(["terminal.unsubscribe"]);
  },
  onExit: (listener) => { exitListener = listener; return () => {}; },
  getReplayPreamble: () => s.preamble,
};
const terminalManager = {
  getTerminal: () => (exists ? terminal : undefined),
  getTerminalState: (id, options) => {
    trace.push(["getState", JSON.parse(JSON.stringify(options ?? null))]);
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  },
  killTerminal: () => {},
  killTerminalAndWait: async () => {},
  subscribeTerminalsChanged: () => () => {},
};
const source = {};
const ownership = new SessionDelivery(
  (_source, message) => trace.push(["message", normalize(message)]),
  (_source, frame) => {
    const decoded = decodeTerminalStreamFrame(frame);
    trace.push(["binary", decoded.opcode, decoded.slot, summarize(decoded.payload)]);
  },
);
ownership.attach(source, s.modern);
const controller = new TerminalSessionController({
  terminalManager,
  emit: (message) => { if (!ownership.reply(message)) trace.push(["message", normalize(message)]); },
  hasBinaryChannel: () => true,
  isPathWithinRoot: () => false,
  sessionLogger: { warn() {}, error() {} },
  clientSupportsWrapReflow: () => wrap,
  getClientBufferedAmount: () => buffered,
});
const settle = () => new Promise((resolve) => setImmediate(resolve));
const dispatch = (message) =>
  ownership.request(source, message, async () => { await controller.dispatch(message, ownership); });
for (const op of s.ops) {
  const [name, arg] = op;
  if (name === "subscribe") {
    await dispatch({ type: "subscribe_terminal_request", terminalId: "term-1", requestId: arg.requestId, ...(arg.restore ? { restore: arg.restore } : {}) });
  } else if (name === "unsubscribe") {
    // The release waits for an in-flight snapshot task, so do not await it.
    void dispatch({ type: "unsubscribe_terminal_request", terminalId: "term-1", requestId: arg });
  } else if (name === "terminal") {
    terminalListener(arg);
  } else if (name === "resolve") {
    pending.shift().resolve(arg === null ? null : { state: STATE, revision: arg });
  } else if (name === "reject") {
    pending.shift().reject(new Error(arg));
  } else if (name === "advance") {
    const target = now + arg;
    for (;;) {
      const due = [...timers].filter(([, timer]) => timer.due <= target).sort((a, b) => a[1].due - b[1].due || a[0] - b[0])[0];
      if (!due) break;
      timers.delete(due[0]);
      now = due[1].due;
      due[1].callback();
      await settle();
    }
    now = target;
  } else if (name === "buffered") {
    buffered = arg;
  } else if (name === "wrap") {
    wrap = arg;
  } else if (name === "exists") {
    exists = arg;
  } else if (name === "exit") {
    exitListener();
  } else if (name === "preamble") {
    s.preamble = arg;
  }
  await settle();
}
process.stdout.write(JSON.stringify(trace), () => process.exit(0));
"#;

/// One scenario: the terminal's initial revision, its replay preamble, and
/// the operations.
struct Scenario {
    name: &'static str,
    modern: bool,
    initial_revision: u64,
    preamble: &'static str,
    ops: &'static str,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "legacy-snapshot-then-output",
        modern: false,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["resolve", 1],
          ["terminal", {"type":"output","data":"a","revision":2}],
          ["terminal", {"type":"output","data":"b","revision":3}],
          ["terminal", {"type":"output","data":"cé","revision":4}],
          ["advance", 5],
          ["advance", 20],
          ["terminal", {"type":"output","data":"late","revision":5}],
          ["terminal", {"type":"output","data":"","revision":6}],
          ["terminal", {"type":"titleChange","title":"x"}],
          ["advance", 50]
        ]"#,
    },
    Scenario {
        name: "restore-live-with-preamble",
        modern: true,
        initial_revision: 1,
        preamble: "\u{1b}[?1h\u{1b}[?2004h",
        ops: r#"[
          ["subscribe", {"requestId":"r1","restore":{"mode":"live"}}],
          ["terminal", {"type":"output","data":"x","revision":2}],
          ["terminal", {"type":"output","data":"y","revision":3}],
          ["advance", 10],
          ["preamble", ""],
          ["terminal", {"type":"snapshotReady","revision":9}],
          ["terminal", {"type":"output","data":"z","revision":10}],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "restore-visible-output-in-flight",
        modern: true,
        initial_revision: 1,
        preamble: "\u{1b}[?1h\u{1b}[?2004h",
        ops: r#"[
          ["subscribe", {"requestId":"r1","restore":{"mode":"visible-snapshot","scrollbackLines":200}}],
          ["terminal", {"type":"output","data":"restore-after\n","revision":2}],
          ["terminal", {"type":"output","data":"old\n","revision":1}],
          ["resolve", 1],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "restore-bounds-and-wrap",
        modern: false,
        initial_revision: 4,
        preamble: "",
        ops: r#"[
          ["wrap", true],
          ["subscribe", {"requestId":"r1","restore":{"mode":"visible-snapshot","scrollbackLines":9999}}],
          ["resolve", 4],
          ["unsubscribe", "u1"],
          ["subscribe", {"requestId":"r2","restore":{"mode":"full-snapshot"}}],
          ["resolve", 4],
          ["subscribe", {"requestId":"r3","restore":{"mode":"visible-snapshot"}}],
          ["wrap", false],
          ["resolve", 4],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "overflow-backs-up-then-catches-up",
        modern: true,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1","restore":{"mode":"live"}}],
          ["buffered", 5000000],
          ["terminal", {"type":"output","data":{"$":"repeat","text":"A","count":140000},"revision":2}],
          ["advance", 10],
          ["terminal", {"type":"output","data":{"$":"repeat","text":"B","count":140000},"revision":3}],
          ["advance", 10],
          ["terminal", {"type":"output","data":"during","revision":4}],
          ["resolve", 4],
          ["advance", 10],
          ["terminal", {"type":"output","data":"after","revision":5}],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "overflow-client-keeps-draining",
        modern: false,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1","restore":{"mode":"live"}}],
          ["buffered", 0],
          ["terminal", {"type":"output","data":{"$":"repeat","text":"A","count":140000},"revision":2}],
          ["advance", 10],
          ["terminal", {"type":"output","data":{"$":"repeat","text":"B","count":140000},"revision":3}],
          ["advance", 10],
          ["terminal", {"type":"output","data":{"$":"repeat","text":"C","count":140000},"revision":4}],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "overflow-without-a-backpressure-signal",
        modern: true,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1","restore":{"mode":"full-snapshot"}}],
          ["resolve", 1],
          ["buffered", null],
          ["terminal", {"type":"output","data":{"$":"repeat","text":"A","count":300000},"revision":2}],
          ["advance", 10],
          ["resolve", 2],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "snapshot-error-legacy-retries",
        modern: false,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["reject", "read failed"],
          ["terminal", {"type":"snapshot","state":{},"revision":2}],
          ["resolve", 2],
          ["terminal", {"type":"output","data":"ok","revision":3}],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "snapshot-error-modern-exits",
        modern: true,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["terminal", {"type":"output","data":"buffered","revision":2}],
          ["reject", "read failed"],
          ["terminal", {"type":"output","data":"after","revision":3}],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "exit-while-snapshot-in-flight",
        modern: true,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["terminal", {"type":"output","data":"tail1","revision":2}],
          ["exit"],
          ["terminal", {"type":"output","data":"tail2","revision":3}],
          ["resolve", 1],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "exit-after-snapshot",
        modern: false,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["resolve", 1],
          ["terminal", {"type":"output","data":"a","revision":2}],
          ["terminal", {"type":"output","data":"b","revision":3}],
          ["exit"],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "terminal-gone-detaches",
        modern: true,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["resolve", null],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "unsubscribe-mid-snapshot",
        modern: false,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["terminal", {"type":"output","data":"x","revision":2}],
          ["unsubscribe", "u1"],
          ["resolve", 1],
          ["advance", 10]
        ]"#,
    },
    Scenario {
        name: "two-streams-and-slot-reuse",
        modern: false,
        initial_revision: 1,
        preamble: "",
        ops: r#"[
          ["subscribe", {"requestId":"r1"}],
          ["resolve", 1],
          ["unsubscribe", "u1"],
          ["subscribe", {"requestId":"r2"}],
          ["resolve", 1],
          ["terminal", {"type":"output","data":"z","revision":2}],
          ["advance", 10]
        ]"#,
    },
];

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn num(value: u64) -> JsValue {
    #[allow(clippy::cast_precision_loss)]
    JsValue::Number(value as f64)
}

fn entry(items: Vec<JsValue>) -> JsValue {
    JsValue::Array(items)
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

/// A frame as the trace shows it: small payloads whole, large ones as a
/// length, prefix and checksum.
fn summarize(payload: &[u8]) -> String {
    use std::fmt::Write as _;
    if payload.len() <= 256 {
        return payload.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    }
    let sum = payload
        .iter()
        .fold(0u64, |sum, byte| (sum + u64::from(*byte)) % 65521);
    let prefix = payload[..16].iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!("{}:{prefix}:{sum}", payload.len())
}

fn state() -> TerminalState {
    let cell = |c: &str| spocky_wire::TerminalCell::new(c);
    TerminalState {
        rows: 1.0,
        cols: 2.0,
        grid: vec![vec![cell("a"), cell("b")]],
        scrollback: Vec::new(),
        cursor: TerminalCursor {
            row: 0.0,
            col: 2.0,
            hidden: None,
            style: None,
            blink: None,
        },
        title: None,
        grid_wrapped: None,
        scrollback_wrapped: None,
    }
}

struct Timer {
    id: u64,
    slot: u8,
    token: u64,
    due: f64,
}

enum Micro {
    Resume(u8),
    Initial(u8, ServerMessage),
}

struct RecordingHost {
    trace: Vec<JsValue>,
    modern: bool,
    now: f64,
    buffered: Option<u64>,
    wrap: bool,
    exists: bool,
    preamble: String,
    timers: Vec<Timer>,
    next_timer: u64,
    pending_reads: VecDeque<u8>,
    microtasks: VecDeque<Micro>,
    closed_owners: HashSet<u8>,
}

impl RecordingHost {
    fn message(&mut self, slot: u8, message: JsValue) {
        if !self.closed_owners.contains(&slot) {
            self.trace.push(entry(vec![text("message"), message]));
        }
    }
}

impl StreamHost for RecordingHost {
    fn emit_binary(&mut self, slot: u8, frame: Vec<u8>) {
        if self.closed_owners.contains(&slot) {
            return;
        }
        self.trace.push(entry(vec![
            text("binary"),
            num(u64::from(frame[0])),
            num(u64::from(frame[1])),
            text(&summarize(&frame[2..])),
        ]));
    }

    fn emit_stream_exit(&mut self, slot: u8, terminal_id: &str, error: Option<&str>) {
        // The owned subscription tags the payload with its id (normalized).
        let mut payload = vec![("terminalId", text(terminal_id))];
        if let Some(error) = error {
            payload.push(("error", text(error)));
        }
        if self.modern {
            payload.push(("subscriptionId", text("<id>")));
        }
        let message = object(vec![
            ("type", text("terminal_stream_exit")),
            ("payload", object(payload)),
        ]);
        self.message(slot, message);
    }

    fn release(&mut self, slot: u8) {
        self.closed_owners.insert(slot);
    }

    fn request_snapshot(&mut self, slot: u8, _terminal_id: &str, options: &SnapshotOptions) {
        let mut object_options = JsObject::new();
        if let Some(lines) = options.scrollback_lines {
            object_options.insert("scrollbackLines", num(u64::from(lines)));
        }
        object_options.insert(
            "includeWrapFlags",
            JsValue::Bool(options.include_wrap_flags),
        );
        self.trace.push(entry(vec![
            text("getState"),
            JsValue::Object(object_options),
        ]));
        self.pending_reads.push_back(slot);
    }

    fn replay_preamble(&mut self, _terminal_id: &str) -> String {
        self.preamble.clone()
    }

    fn client_buffered_amount(&mut self, _slot: u8) -> Option<u64> {
        self.buffered
    }

    fn supports_wrap_reflow(&mut self, _slot: u8) -> bool {
        self.wrap
    }

    fn terminal_exists(&mut self, _terminal_id: &str) -> bool {
        self.exists
    }

    fn schedule_timer(&mut self, slot: u8, token: u64, delay_ms: f64) {
        self.next_timer += 1;
        self.timers.push(Timer {
            id: self.next_timer,
            slot,
            token,
            due: self.now + delay_ms,
        });
        self.trace.push(entry(vec![
            text("timer.schedule"),
            JsValue::Number(delay_ms),
        ]));
    }

    fn clear_timer(&mut self, slot: u8) {
        let before = self.timers.len();
        self.timers.retain(|timer| timer.slot != slot);
        if self.timers.len() != before {
            self.trace.push(entry(vec![text("timer.clear")]));
        }
    }

    fn defer(&mut self, slot: u8) {
        self.microtasks.push_back(Micro::Resume(slot));
    }

    fn terminal_unsubscribe(&mut self, _slot: u8) {
        self.trace.push(entry(vec![text("terminal.unsubscribe")]));
    }

    fn now(&mut self) -> f64 {
        self.now
    }
}

struct Harness {
    streams: TerminalStreams,
    host: RecordingHost,
    open_slots: Vec<u8>,
    initial_revision: u64,
}

impl Harness {
    fn drain(&mut self) {
        while let Some(task) = self.host.microtasks.pop_front() {
            match task {
                Micro::Resume(slot) => self.streams.resume(&mut self.host, slot),
                Micro::Initial(slot, message) => {
                    self.streams.terminal_message(&mut self.host, slot, message);
                }
            }
        }
    }

    fn subscribe(&mut self, request_id: &str, restore: Option<RestoreOptions>) {
        // COMPAT(ownedSubscriptions): a legacy source keeps one subscription
        // per slot name, so `begin` releases the previous one first.
        if !self.host.modern {
            for slot in std::mem::take(&mut self.open_slots) {
                self.host.closed_owners.insert(slot);
                self.streams.release_registration(&mut self.host, slot);
            }
        }
        let bound = self
            .streams
            .bind("term-1", restore, !self.host.modern)
            .expect("slot");
        self.open_slots.push(bound.slot);
        let mode = match bound.snapshot_mode {
            spocky_terminal::restore::SnapshotMode::Ready => "ready",
            spocky_terminal::restore::SnapshotMode::State => "state",
        };
        self.host
            .trace
            .push(entry(vec![text("terminal.subscribe"), text(mode)]));
        let initial = if mode == "ready" {
            ServerMessage::SnapshotReady {
                revision: self.initial_revision,
                replay_preamble: String::new(),
            }
        } else {
            ServerMessage::Snapshot {
                state: Box::new(state()),
                revision: self.initial_revision,
            }
        };
        self.host
            .microtasks
            .push_back(Micro::Initial(bound.slot, initial));
        let mut payload = vec![
            ("terminalId", text("term-1")),
            ("slot", num(u64::from(bound.slot))),
        ];
        if self.host.modern {
            payload.push(("subscriptionId", text("<id>")));
        }
        payload.push(("error", JsValue::Null));
        payload.push(("requestId", text(request_id)));
        let response = object(vec![
            ("type", text("subscribe_terminal_response")),
            ("payload", object(payload)),
        ]);
        self.host.trace.push(entry(vec![text("message"), response]));
        self.streams.try_send_snapshot(&mut self.host, bound.slot);
        self.drain();
    }

    /// `releaseLegacySlot("terminal-output:term-1")`: the oldest open stream
    /// of the terminal is released.
    fn unsubscribe(&mut self) {
        if let Some(slot) = self.open_slots.first().copied() {
            self.open_slots.remove(0);
            self.host.closed_owners.insert(slot);
            self.streams.release_registration(&mut self.host, slot);
        }
        self.drain();
    }

    fn terminal(&mut self, message: ServerMessage) {
        // The fake terminal keeps only its latest subscriber's listener.
        if let Some(slot) = self.open_slots.last().copied() {
            self.streams.terminal_message(&mut self.host, slot, message);
        }
        self.drain();
    }

    fn resolve(&mut self, revision: Option<u64>, reject: Option<&str>) {
        let Some(slot) = self.host.pending_reads.pop_front() else {
            return;
        };
        let result = match (reject, revision) {
            (Some(message), _) => Err(message.to_owned()),
            (None, Some(revision)) => Ok(Some(StateSnapshot {
                state: state(),
                revision,
            })),
            (None, None) => Ok(None),
        };
        self.streams.snapshot_result(&mut self.host, slot, result);
        self.drain();
    }

    fn advance(&mut self, ms: f64) {
        let target = self.host.now + ms;
        loop {
            let next = self
                .host
                .timers
                .iter()
                .enumerate()
                .filter(|(_, timer)| timer.due <= target)
                .min_by(|a, b| a.1.due.total_cmp(&b.1.due).then(a.1.id.cmp(&b.1.id)))
                .map(|(index, _)| index);
            let Some(index) = next else { break };
            let timer = self.host.timers.remove(index);
            self.host.now = timer.due;
            self.streams
                .fire_timer(&mut self.host, timer.slot, timer.token);
            self.drain();
        }
        self.host.now = target;
    }

    fn exit(&mut self) {
        self.streams.detach_stream(&mut self.host, "term-1", true);
        self.drain();
    }
}

/// A scenario value: `{"$":"repeat","text":...,"count":...}` is a long string.
fn revive(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(revive).collect()),
        JsValue::Object(object) => {
            if object.get("$").and_then(JsValue::as_str) == Some("repeat") {
                let unit = object.get("text").and_then(JsValue::as_str).expect("text");
                let count: usize = format!(
                    "{}",
                    object
                        .get("count")
                        .and_then(JsValue::as_f64)
                        .expect("count")
                )
                .parse()
                .expect("count");
                return JsValue::String(unit.repeat(count));
            }
            let mut out = JsObject::new();
            for (key, value) in object.iter() {
                out.insert(key, revive(value));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn restore_from(value: &JsValue) -> RestoreOptions {
    let mode = match value.get("mode").and_then(JsValue::as_str).expect("mode") {
        "live" => RestoreMode::Live,
        "visible-snapshot" => RestoreMode::VisibleSnapshot,
        _ => RestoreMode::FullSnapshot,
    };
    RestoreOptions {
        mode,
        scrollback_lines: value
            .get("scrollbackLines")
            .and_then(JsValue::as_f64)
            .map(|lines| format!("{lines}").parse().expect("lines")),
        size: None,
    }
}

fn server_message(value: &JsValue) -> ServerMessage {
    let revision = value
        .get("revision")
        .and_then(JsValue::as_f64)
        .map_or(0, |revision| {
            format!("{revision}").parse().expect("revision")
        });
    match value.get("type").and_then(JsValue::as_str).expect("type") {
        "output" => ServerMessage::Output {
            data: value
                .get("data")
                .and_then(JsValue::as_str)
                .expect("data")
                .to_owned(),
            revision,
        },
        "snapshot" => ServerMessage::Snapshot {
            state: Box::new(state()),
            revision,
        },
        "snapshotReady" => ServerMessage::SnapshotReady {
            revision,
            replay_preamble: String::new(),
        },
        _ => ServerMessage::TitleChange {
            title: value
                .get("title")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
        },
    }
}

fn run_rust(scenario: &Scenario) -> String {
    let mut harness = Harness {
        streams: TerminalStreams::new(),
        host: RecordingHost {
            trace: Vec::new(),
            modern: scenario.modern,
            now: 0.0,
            buffered: Some(0),
            wrap: false,
            exists: true,
            preamble: scenario.preamble.to_owned(),
            timers: Vec::new(),
            next_timer: 0,
            pending_reads: VecDeque::new(),
            microtasks: VecDeque::new(),
            closed_owners: HashSet::new(),
        },
        open_slots: Vec::new(),
        initial_revision: scenario.initial_revision,
    };
    let ops = revive(&parse(scenario.ops).expect("ops"));
    for op in ops.as_array().expect("array") {
        let op = op.as_array().expect("op");
        let name = op[0].as_str().expect("name");
        let arg = op.get(1);
        match name {
            "subscribe" => {
                let arg = arg.expect("arg");
                let request_id = arg.get("requestId").and_then(JsValue::as_str).expect("id");
                harness.subscribe(request_id, arg.get("restore").map(restore_from));
            }
            "unsubscribe" => harness.unsubscribe(),
            "terminal" => harness.terminal(server_message(arg.expect("arg"))),
            "resolve" => {
                let revision = arg
                    .and_then(JsValue::as_f64)
                    .map(|r| format!("{r}").parse().expect("rev"));
                harness.resolve(revision, None);
            }
            "reject" => harness.resolve(None, arg.and_then(JsValue::as_str)),
            "advance" => harness.advance(arg.and_then(JsValue::as_f64).expect("ms")),
            "buffered" => {
                harness.host.buffered = arg
                    .and_then(JsValue::as_f64)
                    .map(|amount| format!("{amount}").parse().expect("amount"));
            }
            "wrap" => harness.host.wrap = arg.and_then(JsValue::as_bool).expect("wrap"),
            "exists" => harness.host.exists = arg.and_then(JsValue::as_bool).expect("exists"),
            "exit" => harness.exit(),
            "preamble" => {
                arg.and_then(JsValue::as_str)
                    .expect("preamble")
                    .clone_into(&mut harness.host.preamble);
            }
            other => panic!("unknown op {other}"),
        }
    }
    stringify(&JsValue::Array(harness.host.trace))
}

fn node_scenario_json(scenario: &Scenario) -> String {
    let mut object = JsObject::new();
    object.insert("modern", JsValue::Bool(scenario.modern));
    object.insert("initialRevision", num(scenario.initial_revision));
    object.insert("preamble", text(scenario.preamble));
    object.insert("ops", parse(scenario.ops).expect("ops"));
    stringify(&JsValue::Object(object))
}

fn first_difference(expected: &str, actual: &str) -> String {
    let expected: Vec<char> = expected.chars().collect();
    let actual: Vec<char> = actual.chars().collect();
    let at = expected
        .iter()
        .zip(&actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    let window = |text: &[char]| -> String {
        text[at.saturating_sub(200)..(at + 300).min(text.len())]
            .iter()
            .collect()
    };
    format!(
        "differs at char {at} (node {} chars, rust {} chars)\n  node: {}\n  rust: {}",
        expected.len(),
        actual.len(),
        window(&expected),
        window(&actual)
    )
}

#[test]
fn streams_match_the_pinned_session_controller() {
    let Some(pinned) = support::pinned("terminal stream differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut failures = Vec::new();
    let mut seen = String::new();
    for scenario in SCENARIOS {
        let ops = node_scenario_json(scenario);
        // The Node script revives the `$` repeat markers itself.
        let script = NODE_SCRIPT.replace(
            "const s = JSON.parse(scenarioJson);",
            "const revive = (v) => Array.isArray(v) ? v.map(revive) : v && typeof v === 'object' ? (v.$ === 'repeat' ? v.text.repeat(v.count) : Object.fromEntries(Object.entries(v).map(([k, x]) => [k, revive(x)]))) : v;\nconst s = revive(JSON.parse(scenarioJson));",
        );
        let expected = support::run_node(&pinned, &script, &[&ops]);
        let actual = run_rust(scenario);
        seen.push_str(&expected);
        if actual != expected {
            failures.push(format!(
                "{}: {}",
                scenario.name,
                first_difference(&expected, &actual)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // The scenarios must exercise what the trace is meant to compare.
    for needle in [
        "\"getState\"",
        "\"timer.schedule\"",
        "\"timer.clear\"",
        "\"terminal.unsubscribe\"",
        "terminal_stream_exit",
        "\"error\":\"read failed\"",
        "\"binary\",4,",
        "\"binary\",5,",
        "\"binary\",1,",
    ] {
        assert!(seen.contains(needle), "no scenario produced {needle}");
    }
}
