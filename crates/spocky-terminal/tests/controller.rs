//! The terminal message handlers against the pinned
//! `TerminalSessionController`: scripted scenarios run through the controller
//! from the pinned dist on the pinned Node, with a fake terminal manager, a
//! fake clock, fake workspace lookups and the pinned `SessionDelivery`, and
//! through [`TerminalSessionController`] with a recording host that plays the
//! same roles. The traces, in order, must match as text: manager calls,
//! workspace lookups, every message and binary frame the clients receive,
//! stream reads and timers, and how each handler's promise settled.
//!
//! Microtasks drain fully after every operation on both sides, so the
//! scenarios compare outcomes and order within an operation, not microtask
//! counts. The only normalization is the generated `subscriptionId` of an
//! owned subscription, which becomes `<id>` on both sides.

mod support;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::capture::{CaptureOptions, CaptureResult};
use spocky_terminal::controller::{
    Begun, ClientFrame, ControllerHost, CreateRequest, KillTimeouts, Owner, OwnerId, Resume,
    SourceId, TaskId, TerminalInfo, TerminalSessionController, TerminalsChanged, WorkspaceRef,
};
use spocky_terminal::restore::{SnapshotMode, SnapshotOptions};
use spocky_terminal::session::{ClientMessage, ServerMessage, StateSnapshot};
use spocky_terminal::size_ownership::{SizeOwnership, SizeRequest, SizeTarget};
use spocky_terminal::stream::StreamHost;
use spocky_wire::{TerminalCursor, TerminalOpcode, TerminalState};

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
const within = (root, path) => path === root || path.startsWith(`${root}/`);

let wrap = false;
let buffered = 0;
let binary = true;
let refs = s.refs;
let roots = s.roots;
let preamble = "";
let changedListener = null;
const listeners = new Map();
const exitListeners = new Map();
const pendingReads = [];
const pendingCreates = [];
const pendingKills = [];
const terminals = new Map();
const makeTerminal = (info) => {
  const terminal = {
    id: info.id,
    name: info.name,
    cwd: info.cwd,
    workspaceId: info.workspaceId,
    title: info.title ?? undefined,
    size: { rows: 24, cols: 80 },
    getTitle() { return this.title; },
    getActivity() { return null; },
    getSize() { return this.size; },
    send(message) {
      trace.push(["terminal.send", this.id, message]);
      if (message.type === "resize") this.size = { rows: message.rows, cols: message.cols };
    },
    subscribe(listener, options) {
      const mode = options?.initialSnapshot ?? "state";
      trace.push(["terminal.subscribe", this.id, mode]);
      listeners.set(this.id, listener);
      queueMicrotask(() => {
        if (mode === "ready") listener({ type: "snapshotReady", revision: s.initialRevision, replayPreamble: "" });
        else listener({ type: "snapshot", state: STATE, revision: s.initialRevision });
      });
      return () => trace.push(["terminal.unsubscribe", this.id]);
    },
    onExit(listener) { exitListeners.set(this.id, listener); return () => {}; },
    getReplayPreamble() { return preamble; },
  };
  terminals.set(info.id, terminal);
  return terminal;
};
for (const info of s.terminals) makeTerminal(info);
const terminalManager = {
  getTerminal: (id) => terminals.get(id),
  getTerminals: async (cwd, options) => {
    trace.push(["getTerminals", cwd, options?.workspaceId ?? null]);
    return [...terminals.values()].filter((t) => within(cwd, t.cwd) && (options?.workspaceId === undefined || t.workspaceId === options.workspaceId));
  },
  listDirectories: () => [...new Set([...terminals.values()].map((t) => t.cwd))],
  getTerminalState: (id, options) => {
    trace.push(["getState", JSON.parse(JSON.stringify(options ?? null))]);
    return new Promise((resolve, reject) => pendingReads.push({ resolve, reject }));
  },
  createTerminal: (options) => {
    trace.push(["createTerminal", JSON.parse(JSON.stringify(options))]);
    return new Promise((resolve, reject) => pendingCreates.push({ resolve, reject }));
  },
  setTerminalTitle: (id, title) => {
    trace.push(["setTerminalTitle", id, title]);
    const terminal = terminals.get(id);
    if (!terminal) return false;
    terminal.title = title;
    return true;
  },
  killTerminal: (id) => {
    trace.push(["killTerminal", id]);
    terminals.delete(id);
  },
  killTerminalAndWait: (id, options) => {
    trace.push(["killTerminalAndWait", id, options ?? null]);
    return new Promise((resolve, reject) => pendingKills.push({ id, resolve, reject }));
  },
  captureTerminal: async (id, options) => {
    trace.push(["captureTerminal", id, JSON.parse(JSON.stringify(options))]);
    if (s.captureFails) throw new Error("capture failed");
    return { lines: ["one", "two"], totalLines: 2 };
  },
  subscribeTerminalsChanged: (listener) => {
    trace.push(["changed.subscribe"]);
    changedListener = listener;
    return () => { trace.push(["changed.unsubscribe"]); changedListener = null; };
  },
};
const sockets = { a: {}, b: {} };
const names = new Map([[sockets.a, "a"], [sockets.b, "b"]]);
const ownership = new SessionDelivery(
  (_socket, message) => trace.push(["message", normalize(message)]),
  (_socket, frame) => {
    const decoded = decodeTerminalStreamFrame(frame);
    trace.push(["binary", decoded.opcode, decoded.slot, summarize(decoded.payload)]);
  },
);
ownership.attach(sockets.a, s.modern);
if (s.bModern !== null) ownership.attach(sockets.b, s.bModern);
const controller = new TerminalSessionController({
  terminalManager,
  emit: (message) => { if (!ownership.reply(message)) trace.push(["message", normalize(message)]); },
  hasBinaryChannel: () => binary,
  isPathWithinRoot: within,
  sessionLogger: { warn() {}, error() {} },
  listTerminalWorkspaceRefs: async () => { trace.push(["listRefs"]); return refs; },
  listTerminalWorkspaceRoots: async () => { trace.push(["listRoots"]); return roots; },
  clientSupportsWrapReflow: () => wrap,
  getClientBufferedAmount: () => buffered,
});
const settle = () => new Promise((resolve) => setImmediate(resolve));
let nextToken = 0;
const done = [];
const track = (promise) => {
  const token = nextToken++;
  promise.then(() => done.push([token, null]), (error) => done.push([token, error.message]));
};
const apply = (op) => {
  const [name, a, b] = op;
  if (name === "add") makeTerminal(a);
  else if (name === "remove") terminals.delete(a);
  else if (name === "title") terminals.get(a).title = b;
  else if (name === "changed") {
    if (changedListener) changedListener({ cwd: a, terminals: [] });
  }
};
const frameOf = (spec) => ({
  opcode: spec.opcode,
  slot: spec.slot,
  payload: new Uint8Array(Buffer.from(spec.payload.text !== undefined ? spec.payload.text : JSON.stringify(spec.payload.json), "utf8")),
});
const revive = (v) => Array.isArray(v) ? v.map(revive) : v && typeof v === "object" ? (v.$ === "repeat" ? v.text.repeat(v.count) : Object.fromEntries(Object.entries(v).map(([k, x]) => [k, revive(x)]))) : v;
const expand = (ops) => ops.flatMap((op) => (op[0] === "repeat" ? Array.from({ length: op[1] }, () => op[2]) : [op]));
for (const op of expand(revive(s.ops))) {
  const [name, a, b, c] = op;
  if (name === "dispatch") {
    const source = sockets[b ?? "a"];
    const request = ownership.request(source, a, async () => { await controller.dispatch(a, ownership); });
    if (c === false) request.catch(() => {});
    else track(request);
  } else if (name === "dispatchBare") {
    // Outside any request there is no current source.
    try { controller.dispatch(a, ownership); } catch (error) { trace.push(["throws", error.message]); }
  } else if (name === "burst") {
    for (const step of a) apply(step);
  } else if (name === "add" || name === "remove" || name === "title" || name === "changed") {
    apply(op);
  } else if (name === "refs") {
    refs = a;
  } else if (name === "roots") {
    roots = a;
  } else if (name === "metrics") {
    trace.push(["metrics", controller.getMetrics()]);
  } else if (name === "hasDirectory") {
    const result = await controller.hasDirectorySubscription(a, b === undefined ? undefined : sockets[b]);
    trace.push(["hasDirectory", result]);
  } else if (name === "createResolve") {
    pendingCreates.shift().resolve(makeTerminal(a));
  } else if (name === "createReject") {
    pendingCreates.shift().reject(new Error(a));
  } else if (name === "killResolve") {
    const pending = pendingKills.shift();
    terminals.delete(pending.id);
    pending.resolve();
  } else if (name === "killReject") {
    pendingKills.shift().reject(new Error(a));
  } else if (name === "closeKill") {
    trace.push(["closeKill", controller.killTerminalForClose(a)]);
  } else if (name === "archive") {
    track(controller.killTerminalsForWorkspace(a));
  } else if (name === "dispose") {
    controller.dispose();
  } else if (name === "frame") {
    controller.handleBinaryFrame(frameOf(a), sockets[b ?? "a"]);
  } else if (name === "terminal") {
    listeners.get(a)(b);
  } else if (name === "exit") {
    exitListeners.get(a)();
  } else if (name === "resolve") {
    pendingReads.shift().resolve(a === null ? null : { state: STATE, revision: a });
  } else if (name === "reject") {
    pendingReads.shift().reject(new Error(a));
  } else if (name === "advance") {
    const target = now + a;
    for (;;) {
      const due = [...timers].filter(([, timer]) => timer.due <= target).sort((x, y) => x[1].due - y[1].due || x[0] - y[0])[0];
      if (!due) break;
      timers.delete(due[0]);
      now = due[1].due;
      due[1].callback();
      await settle();
    }
    now = target;
  } else if (name === "buffered") {
    buffered = a;
  } else if (name === "wrap") {
    wrap = a;
  } else if (name === "binary") {
    binary = a;
  } else if (name === "preamble") {
    preamble = a;
  } else {
    throw new Error(`unknown op ${name}`);
  }
  await settle();
  done.sort((x, y) => x[0] - y[0]);
  for (const [token, error] of done.splice(0)) trace.push(["settled", token, error]);
}
process.stdout.write(JSON.stringify(trace), () => process.exit(0));
"#;

/// One scenario: who the clients are, and the operations.
struct Scenario {
    name: &'static str,
    /// Source `a` negotiated owned subscriptions.
    modern: bool,
    /// Source `b`, when it is attached up front.
    b_modern: Option<bool>,
    capture_fails: bool,
    ops: &'static str,
}

const TERMINALS: &str = r#"[
  {"id":"t1","name":"Terminal 1","cwd":"/w/a","workspaceId":"ws1","title":"one"},
  {"id":"t2","name":"Terminal 2","cwd":"/w/a/sub","workspaceId":"ws1"},
  {"id":"t3","name":"Terminal 3","cwd":"/w/b","workspaceId":"ws2","title":"three"},
  {"id":"term-1","name":"Stream","cwd":"/w/a","workspaceId":"ws1"}
]"#;
const REFS: &str = r#"[{"workspaceId":"ws1","cwd":"/w/a"},{"workspaceId":"ws2","cwd":"/w/b"}]"#;
const ROOTS: &str = r#"["/w/a","/w/b"]"#;
const INITIAL_REVISION: u64 = 1;
/// The token of a dispatch made outside any request.
const BARE_TOKEN: u64 = u64::MAX - 1;

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "directories-modern",
        modern: true,
        b_modern: Some(false),
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminals_request","requestId":"r1","cwd":"/w/a","workspaceId":"ws1"}],
          ["changed","/w/a/sub"],
          ["burst",[["remove","t2"],["changed","/w/a/sub"],["title","t1","renamed"],["changed","/w/a"],["add",{"id":"t4","name":"Terminal 4","cwd":"/w/a/sub","workspaceId":"ws1"}],["changed","/w/a/sub"]]],
          ["dispatch",{"type":"subscribe_terminals_request","requestId":"r2","cwd":"/w/b","workspaceId":"ws2"}],
          ["changed","/w/b"],
          ["changed","/elsewhere"],
          ["dispatch",{"type":"unsubscribe_terminals_request","requestId":"r3","cwd":"/w/a","workspaceId":"ws1"}],
          ["metrics"],
          ["hasDirectory",{"workspaceId":"ws2","cwd":"/w/b"}],
          ["hasDirectory",{"workspaceId":"ws1","cwd":"/w/b"}],
          ["hasDirectory",{"workspaceId":"ws2","cwd":"/w/b"},"b"],
          ["hasDirectory",{"workspaceId":"ws2","cwd":"/w/b"},"a"],
          ["dispatch",{"type":"subscribe_terminals_request","cwd":"/w/a","workspaceId":"ws1"}],
          ["dispose"],
          ["changed","/w/b"],
          ["metrics"]
        ]"#,
    },
    Scenario {
        name: "directories-legacy",
        modern: false,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminals_request","cwd":"/w/a","workspaceId":"ws1","requestId":"r1"}],
          ["changed","/w/a"],
          ["dispatch",{"type":"subscribe_terminals_request","cwd":"/w/a","workspaceId":"ws1","requestId":"r2"}],
          ["dispatch",{"type":"subscribe_terminals_request","cwd":"/w/a","requestId":"r3"}],
          ["burst",[["add",{"id":"t5","name":"Terminal 5","cwd":"/w/a/x","workspaceId":"ws1"}],["changed","/w/a/x"],["remove","t5"],["changed","/w/a/x"]]],
          ["roots",[]],
          ["changed","/w/a"],
          ["roots",["/w/a/sub","/w/a"]],
          ["changed","/w/a/sub"],
          ["dispatch",{"type":"unsubscribe_terminals_request","cwd":"/w/a","workspaceId":"ws1","requestId":"u1"}],
          ["metrics"],
          ["dispatch",{"type":"unsubscribe_terminals_request","cwd":"/w/a","requestId":"u2"}],
          ["metrics"],
          ["changed","/w/a"]
        ]"#,
    },
    Scenario {
        name: "list",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"list_terminals_request","requestId":"l1"}],
          ["dispatch",{"type":"list_terminals_request","requestId":"l2","cwd":"/w/a"}],
          ["dispatch",{"type":"list_terminals_request","requestId":"l3","workspaceId":"ws2"}],
          ["dispatch",{"type":"list_terminals_request","requestId":"l4","cwd":"/w/a","workspaceId":"ws1"}],
          ["roots",[]],
          ["dispatch",{"type":"list_terminals_request","requestId":"l5","cwd":"/w/a"}],
          ["roots",["/w/a/sub","/w/a"]],
          ["dispatch",{"type":"list_terminals_request","requestId":"l6","cwd":"/w/a"}],
          ["title","t1","a title"],
          ["dispatch",{"type":"list_terminals_request","requestId":"l7","cwd":""}]
        ]"#,
    },
    Scenario {
        name: "create",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"create_terminal_request","requestId":"c1","cwd":"/w/a","workspaceId":"ws1","name":"Shell","command":"zsh","args":["-l"],"size":{"rows":30,"cols":100}}],
          ["createResolve",{"id":"n1","name":"Shell","cwd":"/w/a","workspaceId":"ws1","title":"zsh"}],
          ["dispatch",{"type":"create_terminal_request","requestId":"c2","cwd":"/w/a","workspaceId":"ws9"}],
          ["dispatch",{"type":"create_terminal_request","requestId":"c3","cwd":"/w/a/sub"}],
          ["createReject","spawn failed"],
          ["dispatch",{"type":"create_terminal_request","requestId":"c4","cwd":"/nowhere"}],
          ["dispatch",{"type":"create_terminal_request","requestId":"c5","cwd":"/w/b","workspaceId":""}],
          ["dispatch",{"type":"create_terminal_request","requestId":"c6","cwd":"/w/a","agentId":"agent-1","workspaceId":"ws1"}],
          ["dispatch",{"type":"create_terminal_request","requestId":"c9","cwd":"/w/b"}],
          ["createResolve",{"id":"n2","name":"Terminal 2","cwd":"/w/b","workspaceId":"ws2"}],
          ["refs",[]],
          ["dispatch",{"type":"create_terminal_request","requestId":"c7","cwd":"/w/a"}],
          ["dispatch",{"type":"create_terminal_request","requestId":"c8","cwd":"/w/b","workspaceId":"ws2"}],
          ["refs",[{"workspaceId":"ws3","cwd":"/w/x"},{"workspaceId":"ws4","cwd":"/w/x/y"}]],
          ["dispatch",{"type":"create_terminal_request","requestId":"c10","cwd":"/w/x/y/z"}],
          ["createResolve",{"id":"n3","name":"Terminal 1","cwd":"/w/x/y/z","workspaceId":"ws4"}]
        ]"#,
    },
    Scenario {
        name: "rename",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"terminal.rename.request","requestId":"n1","terminalId":"t1","title":"  hello  "}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n2","terminalId":"t1","title":"   "}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n3","terminalId":"t1","title":{"$":"repeat","text":"x","count":201}}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n4","terminalId":"t1","title":{"$":"repeat","text":"x","count":200}}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n5","terminalId":"missing","title":"hi"}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n6","terminalId":"t1","title":" ﻿ trimmed  "}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n7","terminalId":"t1","title":{"$":"repeat","text":"😀","count":100}}],
          ["dispatch",{"type":"terminal.rename.request","requestId":"n8","terminalId":"t1","title":{"$":"repeat","text":"😀","count":101}}]
        ]"#,
    },
    Scenario {
        name: "capture",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"capture_terminal_request","requestId":"p1","terminalId":"t1"}],
          ["dispatch",{"type":"capture_terminal_request","requestId":"p2","terminalId":"t1","start":-5,"end":10,"stripAnsi":false}],
          ["dispatch",{"type":"capture_terminal_request","requestId":"p3","terminalId":"missing"}]
        ]"#,
    },
    Scenario {
        name: "capture-fails",
        modern: false,
        b_modern: None,
        capture_fails: true,
        ops: r#"[
          ["dispatch",{"type":"capture_terminal_request","requestId":"p1","terminalId":"t1"}]
        ]"#,
    },
    Scenario {
        name: "kill",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"kill_terminal_request","requestId":"k1","terminalId":"t2"}],
          ["killResolve"],
          ["dispatch",{"type":"kill_terminal_request","requestId":"k2","terminalId":"t3"}],
          ["killReject","busy"],
          ["closeKill","t1"],
          ["closeKill","missing"]
        ]"#,
    },
    Scenario {
        name: "archive",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["archive","ws1"],
          ["killResolve"],
          ["killReject","stuck"],
          ["killResolve"],
          ["killResolve"],
          ["archive","ws-empty"]
        ]"#,
    },
    Scenario {
        name: "input-and-resize",
        modern: true,
        b_modern: Some(false),
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"input","data":"ls\r"}}],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":30,"cols":100,"intent":"claim"}}],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":30,"cols":100,"intent":"claim"}}],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":31,"cols":101,"intent":"update"}},"b"],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":32,"cols":102,"intent":"claim"}},"b"],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":33,"cols":103,"intent":"update"}},"b"],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":34,"cols":104,"intent":"update"}}],
          ["dispatch",{"type":"terminal_input","terminalId":"t1","message":{"type":"resize","rows":24,"cols":80}}],
          ["dispatch",{"type":"terminal_input","terminalId":"missing","message":{"type":"input","data":"x"}}],
          ["dispatchBare",{"type":"terminal_input","terminalId":"t1","message":{"type":"input","data":"x"}}]
        ]"#,
    },
    Scenario {
        name: "stream-modern-frames",
        modern: true,
        b_modern: Some(true),
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"s1","restore":{"mode":"live"}}],
          ["terminal","term-1",{"type":"output","data":"x","revision":2}],
          ["advance",10],
          ["metrics"],
          ["frame",{"opcode":2,"slot":0,"payload":{"text":"ls"}}],
          ["frame",{"opcode":2,"slot":0,"payload":{"text":""}}],
          ["frame",{"opcode":3,"slot":0,"payload":{"json":{"rows":40,"cols":120,"intent":"claim"}}}],
          ["frame",{"opcode":3,"slot":0,"payload":{"json":{"rows":41,"cols":121,"intent":"update"}}},"b"],
          ["frame",{"opcode":3,"slot":0,"payload":{"json":{"rows":0,"cols":120}}}],
          ["frame",{"opcode":3,"slot":0,"payload":{"text":"not json"}}],
          ["frame",{"opcode":1,"slot":0,"payload":{"text":"ignored"}}],
          ["frame",{"opcode":2,"slot":0,"payload":{"text":"zz"}},"b"],
          ["frame",{"opcode":2,"slot":9,"payload":{"text":"zz"}}],
          ["remove","term-1"],
          ["frame",{"opcode":2,"slot":0,"payload":{"text":"zz"}}],
          ["advance",10],
          ["metrics"]
        ]"#,
    },
    Scenario {
        name: "stream-legacy-restore-size",
        modern: false,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"s1","restore":{"mode":"visible-snapshot","size":{"rows":10,"cols":20}}}],
          ["resolve",1],
          ["terminal","term-1",{"type":"output","data":"a","revision":2}],
          ["advance",10],
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"s2","restore":{"mode":"live","size":{"rows":11,"cols":21}}}],
          ["terminal","term-1",{"type":"output","data":"b","revision":3}],
          ["advance",10],
          ["metrics"],
          ["dispatch",{"type":"unsubscribe_terminal_request","terminalId":"term-1","requestId":"u1"},"a",false],
          ["metrics"],
          ["terminal","term-1",{"type":"output","data":"c","revision":4}],
          ["advance",10]
        ]"#,
    },
    Scenario {
        name: "stream-errors",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"nope","requestId":"e1"}],
          ["binary",false],
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"e2"}],
          ["binary",true],
          ["metrics"],
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1"}],
          ["dispatch",{"type":"unsubscribe_terminal_request","terminalId":"term-1","requestId":"e3"}]
        ]"#,
    },
    Scenario {
        name: "stream-slots-exhausted",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["repeat",257,["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"many"},"a",false]],
          ["metrics"]
        ]"#,
    },
    Scenario {
        name: "stream-kill-and-dispose",
        modern: true,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"s1","restore":{"mode":"live"}}],
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"t1","requestId":"s2","restore":{"mode":"live"}}],
          ["terminal","term-1",{"type":"output","data":"x","revision":2}],
          ["dispatch",{"type":"kill_terminal_request","requestId":"k1","terminalId":"term-1"}],
          ["killResolve"],
          ["advance",10],
          ["metrics"],
          ["dispose"],
          ["metrics"],
          ["terminal","t1",{"type":"output","data":"y","revision":3}],
          ["advance",10]
        ]"#,
    },
    Scenario {
        name: "stream-exit-and-close",
        modern: false,
        b_modern: None,
        capture_fails: false,
        ops: r#"[
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"term-1","requestId":"s1"}],
          ["terminal","term-1",{"type":"output","data":"tail","revision":2}],
          ["exit","term-1"],
          ["resolve",1],
          ["advance",10],
          ["dispatch",{"type":"subscribe_terminal_request","terminalId":"t1","requestId":"s2","restore":{"mode":"live"}}],
          ["closeKill","t1"],
          ["advance",10],
          ["metrics"]
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
    let hex = |bytes: &[u8]| {
        bytes.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    };
    if payload.len() <= 256 {
        return hex(payload);
    }
    let sum = payload
        .iter()
        .fold(0u64, |sum, byte| (sum + u64::from(*byte)) % 65521);
    format!("{}:{}:{sum}", payload.len(), hex(&payload[..16]))
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
    Tick(TaskId),
    Refs(TaskId, Vec<WorkspaceRef>),
    Roots(TaskId, Vec<String>),
    Resume(u8),
    Initial(u8, ServerMessage),
}

struct SourceRecord {
    name: &'static str,
    modern: bool,
    /// The identity `applyTerminalSize` compares owners by.
    owner: Arc<()>,
}

struct OwnerRecord {
    id: OwnerId,
    source: SourceId,
    legacy_slot: String,
    active: bool,
}

struct FakeTerminal {
    info: TerminalInfo,
    size: (u16, u16),
    sizing: SizeOwnership<()>,
}

/// The terminal a resize is applied to; its sends go to the trace.
struct SizeSink<'a> {
    id: &'a str,
    size: &'a mut (u16, u16),
    trace: &'a mut Vec<JsValue>,
}

impl SizeTarget for SizeSink<'_> {
    fn size(&self) -> (u16, u16) {
        *self.size
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.trace.push(entry(vec![
            text("terminal.send"),
            text(self.id),
            object(vec![
                ("type", text("resize")),
                ("rows", num(u64::from(rows))),
                ("cols", num(u64::from(cols))),
            ]),
        ]));
        *self.size = (rows, cols);
    }
}

#[allow(clippy::struct_excessive_bools)]
struct Host {
    trace: Vec<JsValue>,
    now: f64,
    buffered: Option<u64>,
    wrap: bool,
    binary: bool,
    preamble: String,
    refs: Vec<WorkspaceRef>,
    roots: Vec<String>,
    capture_fails: bool,
    terminals: Vec<FakeTerminal>,
    sources: Vec<SourceRecord>,
    owners: Vec<OwnerRecord>,
    next_owner: OwnerId,
    slot_owner: HashMap<u8, OwnerId>,
    current_request: JsValue,
    current_source: SourceId,
    changed_subscribed: bool,
    /// The slot of the latest subscription to each terminal.
    listeners: HashMap<String, u8>,
    /// The terminal each slot streams.
    slot_terminal: HashMap<u8, String>,
    timers: Vec<Timer>,
    next_timer: u64,
    pending_reads: VecDeque<u8>,
    pending_creates: VecDeque<TaskId>,
    pending_kills: VecDeque<(TaskId, String)>,
    microtasks: VecDeque<Micro>,
    settled: Vec<(u64, Result<(), String>)>,
}

/// `JSON.parse(JSON.stringify(message))` with the owned subscription id
/// normalized.
fn normalized(message: &JsValue) -> JsValue {
    let message = parse(&stringify(message)).expect("message json");
    let Some(source) = message.as_object() else {
        return message;
    };
    let mut copy = JsObject::new();
    for (name, value) in source.iter() {
        match (name, value.as_object()) {
            ("payload", Some(payload)) => {
                let mut fixed = JsObject::new();
                for (field, inner) in payload.iter() {
                    if field == "subscriptionId" {
                        fixed.insert(field, text("<id>"));
                    } else {
                        fixed.insert(field, inner.clone());
                    }
                }
                copy.insert(name, JsValue::Object(fixed));
            }
            _ => copy.insert(name, value.clone()),
        }
    }
    JsValue::Object(copy)
}

/// `message.payload` without `key`.
fn without_payload_key(message: &JsValue, key: &str) -> JsValue {
    let mut copy = JsObject::new();
    for (name, value) in message.as_object().expect("object").iter() {
        if name == "payload" {
            let mut payload = JsObject::new();
            for (field, inner) in value.as_object().expect("payload").iter() {
                if field != key {
                    payload.insert(field, inner.clone());
                }
            }
            copy.insert(name, JsValue::Object(payload));
        } else {
            copy.insert(name, value.clone());
        }
    }
    JsValue::Object(copy)
}

/// `withSubscriptionId`: the owner's id appended to the payload.
fn with_subscription_id(message: &JsValue) -> JsValue {
    let mut copy = JsObject::new();
    for (name, value) in message.as_object().expect("object").iter() {
        if name == "payload" {
            let mut payload = JsObject::new();
            for (field, inner) in value.as_object().expect("payload").iter() {
                payload.insert(field, inner.clone());
            }
            payload.insert("subscriptionId", text("<id>"));
            copy.insert(name, JsValue::Object(payload));
        } else {
            copy.insert(name, value.clone());
        }
    }
    JsValue::Object(copy)
}

fn type_of(message: &JsValue) -> &str {
    message.get("type").and_then(JsValue::as_str).unwrap_or("")
}

fn within(root: &str, path: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

impl Host {
    fn terminal_index(&self, id: &str) -> Option<usize> {
        self.terminals
            .iter()
            .position(|terminal| terminal.info.id == id)
    }

    fn source(&self, id: SourceId) -> &SourceRecord {
        &self.sources[usize::try_from(id).expect("source index")]
    }

    fn owner_index(&self, owner: OwnerId) -> Option<usize> {
        self.owners.iter().position(|record| record.id == owner)
    }

    fn owner_modern(&self, owner: OwnerId) -> bool {
        self.owner_index(owner)
            .is_some_and(|index| self.source(self.owners[index].source).modern)
    }

    fn close_owner(&mut self, owner: OwnerId) {
        if let Some(index) = self.owner_index(owner) {
            self.owners[index].active = false;
        }
    }

    fn send_json(message: &ClientMessage) -> JsValue {
        match message {
            ClientMessage::Input(data) => {
                object(vec![("type", text("input")), ("data", text(data))])
            }
            ClientMessage::Resize { rows, cols } => object(vec![
                ("type", text("resize")),
                ("rows", num(u64::from(*rows))),
                ("cols", num(u64::from(*cols))),
            ]),
            ClientMessage::Mouse => object(vec![("type", text("mouse"))]),
        }
    }
}

impl StreamHost for Host {
    fn emit_binary(&mut self, slot: u8, frame: Vec<u8>) {
        let Some(owner) = self.slot_owner.get(&slot).copied() else {
            return;
        };
        let active = self
            .owner_index(owner)
            .is_some_and(|index| self.owners[index].active);
        if !active {
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
        let Some(owner) = self.slot_owner.get(&slot).copied() else {
            return;
        };
        let mut payload = vec![("terminalId", text(terminal_id))];
        if let Some(error) = error {
            payload.push(("error", text(error)));
        }
        let message = object(vec![
            ("type", text("terminal_stream_exit")),
            ("payload", object(payload)),
        ]);
        ControllerHost::owner_emit(self, owner, message);
    }

    fn release(&mut self, slot: u8) {
        if let Some(owner) = self.slot_owner.get(&slot).copied() {
            self.close_owner(owner);
        }
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

    fn terminal_exists(&mut self, terminal_id: &str) -> bool {
        self.terminal_index(terminal_id).is_some()
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

    fn terminal_unsubscribe(&mut self, slot: u8) {
        let terminal = self.slot_terminal.get(&slot).cloned();
        self.trace.push(entry(vec![
            text("terminal.unsubscribe"),
            terminal.map_or(JsValue::Undefined, |id| text(&id)),
        ]));
    }

    fn now(&mut self) -> f64 {
        self.now
    }
}

impl ControllerHost for Host {
    fn has_manager(&mut self) -> bool {
        true
    }

    fn emit(&mut self, message: JsValue) {
        // The reply path of the delivery: a legacy source never sees the
        // subscription id of the response.
        let legacy = !self.source(self.current_source).modern;
        let message = if legacy && type_of(&message) == "subscribe_terminal_response" {
            without_payload_key(&message, "subscriptionId")
        } else {
            message
        };
        self.trace
            .push(entry(vec![text("message"), normalized(&message)]));
    }

    fn has_binary_channel(&mut self) -> bool {
        self.binary
    }

    fn is_path_within_root(&mut self, root: &str, path: &str) -> bool {
        within(root, path)
    }

    fn begin_owner(&mut self, _family: &str, legacy_slot: &str) -> Result<Begun, String> {
        let source = self.current_source;
        let modern = self.source(source).modern;
        let has_request_id = self
            .current_request
            .get("requestId")
            .and_then(JsValue::as_str)
            .is_some_and(|id| !id.is_empty());
        if modern && !has_request_id {
            return Err("Owned subscriptions require a requestId".to_owned());
        }
        let mut released_prior = Vec::new();
        if !modern {
            for record in &mut self.owners {
                if record.source == source && record.legacy_slot == legacy_slot && record.active {
                    record.active = false;
                    released_prior.push(record.id);
                }
            }
        }
        self.next_owner += 1;
        let id = self.next_owner;
        self.owners.push(OwnerRecord {
            id,
            source,
            legacy_slot: legacy_slot.to_owned(),
            active: true,
        });
        Ok(Begun {
            owner: Owner {
                id,
                response_id: format!("owner-{id}"),
                source,
            },
            released_prior,
        })
    }

    fn owner_emit(&mut self, owner: OwnerId, message: JsValue) {
        let Some(index) = self.owner_index(owner) else {
            return;
        };
        if !self.owners[index].active {
            return;
        }
        let tagged = if self.owner_modern(owner) {
            with_subscription_id(&message)
        } else if type_of(&message) == "terminals_changed" {
            without_payload_key(&message, "workspaceId")
        } else {
            message
        };
        self.trace
            .push(entry(vec![text("message"), normalized(&tagged)]));
    }

    fn owner_aborted(&mut self, owner: OwnerId) -> bool {
        self.owner_index(owner)
            .is_none_or(|index| !self.owners[index].active)
    }

    fn release_owner(&mut self, owner: OwnerId) {
        self.close_owner(owner);
    }

    fn release_legacy_slot(&mut self, slot: &str) -> Result<Option<OwnerId>, String> {
        let source = self.current_source;
        if self.source(source).modern {
            return Err(
                "Release subscriptions using the server-assigned subscription ID".to_owned(),
            );
        }
        let found = self
            .owners
            .iter_mut()
            .find(|record| record.source == source && record.legacy_slot == slot && record.active);
        Ok(found.map(|record| {
            record.active = false;
            record.id
        }))
    }

    fn is_modern(&mut self, source: SourceId) -> bool {
        self.source(source).modern
    }

    fn stream_bound(&mut self, slot: u8, owner: OwnerId) {
        self.slot_owner.insert(slot, owner);
    }

    fn terminal_subscribe(&mut self, slot: u8, terminal_id: &str, mode: SnapshotMode) {
        let label = match mode {
            SnapshotMode::Ready => "ready",
            SnapshotMode::State => "state",
        };
        self.trace.push(entry(vec![
            text("terminal.subscribe"),
            text(terminal_id),
            text(label),
        ]));
        self.listeners.insert(terminal_id.to_owned(), slot);
        self.slot_terminal.insert(slot, terminal_id.to_owned());
        let initial = if mode == SnapshotMode::Ready {
            ServerMessage::SnapshotReady {
                revision: INITIAL_REVISION,
                replay_preamble: String::new(),
            }
        } else {
            ServerMessage::Snapshot {
                state: Box::new(state()),
                revision: INITIAL_REVISION,
            }
        };
        self.microtasks.push_back(Micro::Initial(slot, initial));
    }

    fn settled(&mut self, token: u64, result: Result<(), String>) {
        self.settled.push((token, result));
    }

    fn defer_task(&mut self, task: TaskId) {
        self.microtasks.push_back(Micro::Tick(task));
    }

    fn request_workspace_refs(&mut self, task: TaskId) {
        self.trace.push(entry(vec![text("listRefs")]));
        self.microtasks
            .push_back(Micro::Refs(task, self.refs.clone()));
    }

    fn request_workspace_roots(&mut self, task: TaskId) {
        self.trace.push(entry(vec![text("listRoots")]));
        self.microtasks
            .push_back(Micro::Roots(task, self.roots.clone()));
    }

    fn terminals_changed_subscribe(&mut self) {
        self.trace.push(entry(vec![text("changed.subscribe")]));
        self.changed_subscribed = true;
    }

    fn terminals_changed_unsubscribe(&mut self) {
        self.trace.push(entry(vec![text("changed.unsubscribe")]));
        self.changed_subscribed = false;
    }

    fn get_terminals(
        &mut self,
        cwd: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<TerminalInfo>, String> {
        self.trace.push(entry(vec![
            text("getTerminals"),
            text(cwd),
            workspace_id.map_or(JsValue::Null, text),
        ]));
        Ok(self
            .terminals
            .iter()
            .filter(|terminal| within(cwd, &terminal.info.cwd))
            .filter(|terminal| workspace_id.is_none_or(|id| terminal.info.workspace_id == id))
            .map(|terminal| terminal.info.clone())
            .collect())
    }

    fn list_directories(&mut self) -> Vec<String> {
        let mut directories: Vec<String> = Vec::new();
        for terminal in &self.terminals {
            if !directories.contains(&terminal.info.cwd) {
                directories.push(terminal.info.cwd.clone());
            }
        }
        directories
    }

    fn terminal_info(&mut self, id: &str) -> Option<TerminalInfo> {
        self.terminal_index(id)
            .map(|index| self.terminals[index].info.clone())
    }

    fn terminal_send(&mut self, id: &str, message: &ClientMessage) {
        self.trace.push(entry(vec![
            text("terminal.send"),
            text(id),
            Self::send_json(message),
        ]));
        if let (Some(index), ClientMessage::Resize { rows, cols }) =
            (self.terminal_index(id), message)
        {
            self.terminals[index].size = (*rows, *cols);
        }
    }

    fn apply_terminal_size(&mut self, id: &str, source: SourceId, request: SizeRequest) {
        let Some(index) = self.terminal_index(id) else {
            return;
        };
        let owner = Arc::clone(&self.source(source).owner);
        let terminal = &mut self.terminals[index];
        let mut sink = SizeSink {
            id,
            size: &mut terminal.size,
            trace: &mut self.trace,
        };
        terminal.sizing.apply(&mut sink, &owner, request);
    }

    fn create_terminal(&mut self, task: TaskId, request: &CreateRequest) {
        let optional = |value: &Option<String>| value.as_deref().map_or(JsValue::Undefined, text);
        let options = object(vec![
            ("cwd", text(&request.cwd)),
            ("workspaceId", text(&request.workspace_id)),
            ("name", optional(&request.name)),
            ("command", optional(&request.command)),
            (
                "args",
                request.args.as_ref().map_or(JsValue::Undefined, |args| {
                    JsValue::Array(args.iter().map(|arg| text(arg)).collect())
                }),
            ),
            (
                "rows",
                request
                    .rows
                    .map_or(JsValue::Undefined, |rows| num(u64::from(rows))),
            ),
            (
                "cols",
                request
                    .cols
                    .map_or(JsValue::Undefined, |cols| num(u64::from(cols))),
            ),
        ]);
        self.trace
            .push(entry(vec![text("createTerminal"), normalized(&options)]));
        self.pending_creates.push_back(task);
    }

    fn set_terminal_title(&mut self, id: &str, title: &str) -> bool {
        self.trace
            .push(entry(vec![text("setTerminalTitle"), text(id), text(title)]));
        let Some(index) = self.terminal_index(id) else {
            return false;
        };
        self.terminals[index].info.title = Some(title.to_owned());
        true
    }

    fn kill_terminal(&mut self, id: &str) {
        self.trace.push(entry(vec![text("killTerminal"), text(id)]));
        self.terminals.retain(|terminal| terminal.info.id != id);
    }

    fn kill_terminal_and_wait(&mut self, task: TaskId, id: &str, timeouts: Option<KillTimeouts>) {
        let options = timeouts.map_or(JsValue::Null, |timeouts| {
            object(vec![
                ("gracefulTimeoutMs", JsValue::Number(timeouts.graceful)),
                ("forceTimeoutMs", JsValue::Number(timeouts.force)),
            ])
        });
        self.trace
            .push(entry(vec![text("killTerminalAndWait"), text(id), options]));
        self.pending_kills.push_back((task, id.to_owned()));
    }

    fn capture_terminal(
        &mut self,
        id: &str,
        options: &CaptureOptions,
    ) -> Result<CaptureResult, String> {
        let mut recorded = JsObject::new();
        if let Some(start) = options.start {
            recorded.insert("start", JsValue::Number(start));
        }
        if let Some(end) = options.end {
            recorded.insert("end", JsValue::Number(end));
        }
        if let Some(strip) = options.strip_ansi {
            recorded.insert("stripAnsi", JsValue::Bool(strip));
        }
        self.trace.push(entry(vec![
            text("captureTerminal"),
            text(id),
            JsValue::Object(recorded),
        ]));
        if self.capture_fails {
            return Err("capture failed".to_owned());
        }
        Ok(CaptureResult {
            lines: vec!["one".to_owned(), "two".to_owned()],
            total_lines: 2,
        })
    }
}

struct Harness {
    controller: TerminalSessionController,
    host: Host,
    next_token: u64,
}

impl Harness {
    fn drain(&mut self) {
        while let Some(task) = self.host.microtasks.pop_front() {
            match task {
                Micro::Tick(id) => self.controller.resume(&mut self.host, id, Resume::Tick),
                Micro::Refs(id, refs) => {
                    self.controller
                        .resume(&mut self.host, id, Resume::Refs(Ok(refs)));
                }
                Micro::Roots(id, roots) => {
                    self.controller
                        .resume(&mut self.host, id, Resume::Roots(Ok(roots)));
                }
                Micro::Resume(slot) => self.controller.resume_stream(&mut self.host, slot),
                Micro::Initial(slot, message) => {
                    self.controller
                        .terminal_message(&mut self.host, slot, message);
                }
            }
        }
    }

    /// The end of every operation: microtasks, then the promises that settled.
    fn settle(&mut self) {
        self.drain();
        let mut settled = std::mem::take(&mut self.host.settled);
        settled.sort_by_key(|(token, _)| *token);
        for (token, result) in settled {
            if token == BARE_TOKEN {
                if let Err(message) = result {
                    self.host
                        .trace
                        .push(entry(vec![text("throws"), text(&message)]));
                }
                continue;
            }
            if token == u64::MAX {
                continue;
            }
            self.host.trace.push(entry(vec![
                text("settled"),
                num(token),
                match result {
                    Ok(()) => JsValue::Null,
                    Err(message) => text(&message),
                },
            ]));
        }
    }

    fn source_id(&self, name: Option<&str>) -> SourceId {
        let name = name.unwrap_or("a");
        self.host
            .sources
            .iter()
            .position(|source| source.name == name)
            .map_or_else(
                || panic!("source {name}"),
                |index| SourceId::try_from(index).expect("source id"),
            )
    }

    fn token(&mut self, tracked: bool) -> u64 {
        if !tracked {
            return u64::MAX;
        }
        let token = self.next_token;
        self.next_token += 1;
        token
    }

    fn apply(&mut self, op: &[JsValue]) {
        let name = op[0].as_str().expect("name");
        let first = op.get(1);
        match name {
            "add" => {
                let info = terminal_info(first.expect("terminal"));
                self.host.terminals.push(FakeTerminal {
                    info,
                    size: (24, 80),
                    sizing: SizeOwnership::default(),
                });
            }
            "remove" => {
                let id = first.and_then(JsValue::as_str).expect("id");
                self.host
                    .terminals
                    .retain(|terminal| terminal.info.id != id);
            }
            "title" => {
                let id = first.and_then(JsValue::as_str).expect("id");
                let title = op[2].as_str().expect("title");
                if let Some(index) = self.host.terminal_index(id) {
                    self.host.terminals[index].info.title = Some(title.to_owned());
                }
            }
            "changed" => {
                if self.host.changed_subscribed {
                    let cwd = first.and_then(JsValue::as_str).expect("cwd");
                    self.controller.on_terminals_changed(
                        &mut self.host,
                        &TerminalsChanged {
                            cwd: cwd.to_owned(),
                        },
                    );
                }
            }
            other => panic!("unknown step {other}"),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run(&mut self, op: &[JsValue]) {
        let name = op[0].as_str().expect("name");
        let first = op.get(1);
        let second = op.get(2);
        match name {
            "dispatch" => {
                let message = first.expect("message");
                let source = self.source_id(second.and_then(JsValue::as_str));
                let tracked = op.get(3).and_then(JsValue::as_bool) != Some(false);
                let token = self.token(tracked);
                self.host.current_request = message.clone();
                self.host.current_source = source;
                self.controller
                    .dispatch(&mut self.host, token, message, Some(source));
            }
            "dispatchBare" => {
                let message = first.expect("message");
                self.host.current_request = message.clone();
                self.controller
                    .dispatch(&mut self.host, BARE_TOKEN, message, None);
            }
            "burst" => {
                for step in first.and_then(JsValue::as_array).expect("steps") {
                    self.apply(step.as_array().expect("step"));
                }
            }
            "add" | "remove" | "title" | "changed" => self.apply(op),
            "refs" => self.host.refs = workspace_refs(first.expect("refs")),
            "roots" => {
                self.host.roots = first
                    .and_then(JsValue::as_array)
                    .expect("roots")
                    .iter()
                    .map(|root| root.as_str().expect("root").to_owned())
                    .collect();
            }
            "metrics" => {
                let metrics = self.controller.metrics();
                self.host.trace.push(entry(vec![
                    text("metrics"),
                    object(vec![
                        (
                            "directorySubscriptionCount",
                            num(metrics.directory_subscription_count as u64),
                        ),
                        (
                            "streamSubscriptionCount",
                            num(metrics.stream_subscription_count as u64),
                        ),
                    ]),
                ]));
            }
            "hasDirectory" => {
                self.has_directory(first.expect("input"), second.and_then(JsValue::as_str));
            }
            "createResolve" => {
                let info = terminal_info(first.expect("terminal"));
                self.host.terminals.push(FakeTerminal {
                    info: info.clone(),
                    size: (24, 80),
                    sizing: SizeOwnership::default(),
                });
                let task = self.host.pending_creates.pop_front().expect("create");
                self.controller
                    .resume(&mut self.host, task, Resume::Created(Ok(info)));
            }
            "createReject" => {
                let task = self.host.pending_creates.pop_front().expect("create");
                let message = first.and_then(JsValue::as_str).expect("message");
                self.controller.resume(
                    &mut self.host,
                    task,
                    Resume::Created(Err(message.to_owned())),
                );
            }
            "killResolve" => {
                let (task, id) = self.host.pending_kills.pop_front().expect("kill");
                self.host
                    .terminals
                    .retain(|terminal| terminal.info.id != id);
                self.controller
                    .resume(&mut self.host, task, Resume::Killed(Ok(())));
            }
            "killReject" => {
                let (task, _) = self.host.pending_kills.pop_front().expect("kill");
                let message = first.and_then(JsValue::as_str).expect("message");
                self.controller.resume(
                    &mut self.host,
                    task,
                    Resume::Killed(Err(message.to_owned())),
                );
            }
            "closeKill" => {
                let id = first.and_then(JsValue::as_str).expect("id");
                let result = self.controller.kill_terminal_for_close(&mut self.host, id);
                self.host.trace.push(entry(vec![
                    text("closeKill"),
                    object(vec![
                        ("terminalId", text(&result.terminal_id)),
                        ("success", JsValue::Bool(result.success)),
                    ]),
                ]));
            }
            "archive" => {
                let token = self.token(true);
                let workspace = first.and_then(JsValue::as_str).expect("workspace");
                self.controller
                    .kill_terminals_for_workspace(&mut self.host, token, workspace);
            }
            "dispose" => self.controller.dispose(&mut self.host),
            "frame" => {
                let spec = first.expect("frame");
                let source = self.source_id(second.and_then(JsValue::as_str));
                let frame = client_frame(spec);
                self.controller
                    .handle_binary_frame(&mut self.host, &frame, source);
            }
            "terminal" => {
                let id = first.and_then(JsValue::as_str).expect("id");
                let message = server_message(second.expect("message"));
                if let Some(slot) = self.host.listeners.get(id).copied() {
                    self.controller
                        .terminal_message(&mut self.host, slot, message);
                }
            }
            "exit" => {
                let id = first.and_then(JsValue::as_str).expect("id");
                self.controller.terminal_exited(&mut self.host, id);
            }
            "resolve" => {
                let Some(slot) = self.host.pending_reads.pop_front() else {
                    return;
                };
                let revision = first
                    .and_then(JsValue::as_f64)
                    .map(|r| format!("{r}").parse().expect("rev"));
                let result = Ok(revision.map(|revision| StateSnapshot {
                    state: state(),
                    revision,
                }));
                self.controller
                    .snapshot_result(&mut self.host, slot, result);
            }
            "reject" => {
                let Some(slot) = self.host.pending_reads.pop_front() else {
                    return;
                };
                let message = first.and_then(JsValue::as_str).expect("message");
                self.controller
                    .snapshot_result(&mut self.host, slot, Err(message.to_owned()));
            }
            "advance" => self.advance(first.and_then(JsValue::as_f64).expect("ms")),
            "buffered" => {
                self.host.buffered = first
                    .and_then(JsValue::as_f64)
                    .map(|amount| format!("{amount}").parse().expect("amount"));
            }
            "wrap" => self.host.wrap = first.and_then(JsValue::as_bool).expect("wrap"),
            "binary" => self.host.binary = first.and_then(JsValue::as_bool).expect("binary"),
            "preamble" => {
                first
                    .and_then(JsValue::as_str)
                    .expect("preamble")
                    .clone_into(&mut self.host.preamble);
            }
            other => panic!("unknown op {other}"),
        }
        self.settle();
    }

    fn has_directory(&mut self, input: &JsValue, source: Option<&str>) {
        let source = source.map(|name| self.source_id(Some(name)));
        let workspace_id = input
            .get("workspaceId")
            .and_then(JsValue::as_str)
            .expect("workspace");
        let cwd = input.get("cwd").and_then(JsValue::as_str).expect("cwd");
        let result = if self.controller.has_directory_candidates(source) {
            self.host.trace.push(entry(vec![text("listRoots")]));
            let roots = self.host.roots.clone();
            self.controller.has_directory_subscription(
                &mut self.host,
                workspace_id,
                cwd,
                source,
                &roots,
            )
        } else {
            false
        };
        self.host
            .trace
            .push(entry(vec![text("hasDirectory"), JsValue::Bool(result)]));
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
            self.controller
                .fire_timer(&mut self.host, timer.slot, timer.token);
            self.drain();
        }
        self.host.now = target;
    }
}

fn terminal_info(value: &JsValue) -> TerminalInfo {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(JsValue::as_str)
            .expect("field")
            .to_owned()
    };
    TerminalInfo {
        id: field("id"),
        name: field("name"),
        cwd: field("cwd"),
        workspace_id: field("workspaceId"),
        title: value
            .get("title")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
    }
}

fn workspace_refs(value: &JsValue) -> Vec<WorkspaceRef> {
    value
        .as_array()
        .expect("refs")
        .iter()
        .map(|entry| WorkspaceRef {
            workspace_id: entry
                .get("workspaceId")
                .and_then(JsValue::as_str)
                .expect("workspaceId")
                .to_owned(),
            cwd: entry
                .get("cwd")
                .and_then(JsValue::as_str)
                .expect("cwd")
                .to_owned(),
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn client_frame(spec: &JsValue) -> ClientFrame {
    let opcode = match spec
        .get("opcode")
        .and_then(JsValue::as_f64)
        .expect("opcode") as u8
    {
        1 => TerminalOpcode::Output,
        2 => TerminalOpcode::Input,
        3 => TerminalOpcode::Resize,
        4 => TerminalOpcode::Snapshot,
        _ => TerminalOpcode::Restore,
    };
    let slot = format!(
        "{}",
        spec.get("slot").and_then(JsValue::as_f64).expect("slot")
    )
    .parse()
    .expect("slot");
    let payload = spec.get("payload").expect("payload");
    let bytes = payload.get("text").and_then(JsValue::as_str).map_or_else(
        || stringify(payload.get("json").expect("json")).into_bytes(),
        |text| text.as_bytes().to_vec(),
    );
    ClientFrame {
        opcode,
        slot,
        payload: bytes,
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

fn run_rust(scenario: &Scenario) -> String {
    let mut sources = vec![SourceRecord {
        name: "a",
        modern: scenario.modern,
        owner: Arc::new(()),
    }];
    // Source `b` attaches on first use when the scenario does not say.
    sources.push(SourceRecord {
        name: "b",
        modern: scenario.b_modern.unwrap_or(false),
        owner: Arc::new(()),
    });
    let terminals = parse(TERMINALS).expect("terminals");
    let mut harness = Harness {
        controller: TerminalSessionController::new(),
        host: Host {
            trace: Vec::new(),
            now: 0.0,
            buffered: Some(0),
            wrap: false,
            binary: true,
            preamble: String::new(),
            refs: workspace_refs(&parse(REFS).expect("refs")),
            roots: ROOTS_LIST.iter().map(|root| (*root).to_owned()).collect(),
            capture_fails: scenario.capture_fails,
            terminals: terminals
                .as_array()
                .expect("terminals")
                .iter()
                .map(|value| FakeTerminal {
                    info: terminal_info(value),
                    size: (24, 80),
                    sizing: SizeOwnership::default(),
                })
                .collect(),
            sources,
            owners: Vec::new(),
            next_owner: 0,
            slot_owner: HashMap::new(),
            current_request: JsValue::Null,
            current_source: 0,
            changed_subscribed: false,
            listeners: HashMap::new(),
            slot_terminal: HashMap::new(),
            timers: Vec::new(),
            next_timer: 0,
            pending_reads: VecDeque::new(),
            pending_creates: VecDeque::new(),
            pending_kills: VecDeque::new(),
            microtasks: VecDeque::new(),
            settled: Vec::new(),
        },
        next_token: 0,
    };
    let ops = revive(&parse(scenario.ops).expect("ops"));
    let mut expanded: Vec<Vec<JsValue>> = Vec::new();
    for op in ops.as_array().expect("array") {
        let op = op.as_array().expect("op");
        if op[0].as_str() == Some("repeat") {
            let count: usize = format!("{}", op[1].as_f64().expect("count"))
                .parse()
                .expect("count");
            for _ in 0..count {
                expanded.push(op[2].as_array().expect("op").to_vec());
            }
        } else {
            expanded.push(op.to_vec());
        }
    }
    for op in &expanded {
        harness.run(op);
    }
    stringify(&JsValue::Array(std::mem::take(&mut harness.host.trace)))
}

const ROOTS_LIST: &[&str] = &["/w/a", "/w/b"];

fn node_scenario_json(scenario: &Scenario) -> String {
    let mut object = JsObject::new();
    object.insert("modern", JsValue::Bool(scenario.modern));
    object.insert(
        "bModern",
        scenario.b_modern.map_or(JsValue::Null, JsValue::Bool),
    );
    object.insert("initialRevision", num(INITIAL_REVISION));
    object.insert("captureFails", JsValue::Bool(scenario.capture_fails));
    object.insert("terminals", parse(TERMINALS).expect("terminals"));
    object.insert("refs", parse(REFS).expect("refs"));
    object.insert("roots", parse(ROOTS).expect("roots"));
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
        text[at.saturating_sub(300)..(at + 400).min(text.len())]
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
fn handlers_match_the_pinned_session_controller() {
    let Some(pinned) = support::pinned("terminal controller differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut failures = Vec::new();
    let mut seen = String::new();
    for scenario in SCENARIOS {
        let expected = support::run_node(&pinned, NODE_SCRIPT, &[&node_scenario_json(scenario)]);
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
        "\"changed.subscribe\"",
        "\"changed.unsubscribe\"",
        "\"terminals_changed\"",
        "\"list_terminals_response\"",
        "\"create_terminal_response\"",
        "\"terminal.rename.response\"",
        "\"capture_terminal_response\"",
        "\"kill_terminal_response\"",
        "\"subscribe_terminal_response\"",
        "\"terminal_stream_exit\"",
        "\"killTerminalAndWait\"",
        "\"createTerminal\"",
        "\"No terminal stream slots available\"",
        "\"Owned subscriptions require a requestId\"",
        "\"Terminal input requires a source\"",
        "\"binary\",1,",
        "\"binary\",4,",
        "\"settled\"",
    ] {
        assert!(seen.contains(needle), "no scenario produced {needle}");
    }
}
