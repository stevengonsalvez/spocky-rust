// Drives the pinned Paseo daemon relay client (packages/server relay-transport.ts,
// relay-runtime.ts, websocket/encrypted-relay-socket.ts and packages/protocol
// daemon-endpoints.ts at 5de45e2) for the spocky-daemon-relay differential test.
//
// Usage: node relay-daemon-driver.mjs <paseo-root> <node-modules-dir>
//
// The TypeScript is loaded unchanged (Node 22.20 strips its types; a resolve hook
// maps `./x.js` imports to `./x.ts`, `ws` to the pinned install, and the two
// workspace packages to their sources). `@getpaseo/relay/e2ee` is replaced by a
// recording stand-in for createDaemonChannel because the channel itself is
// differential-tested by spocky-crypto.
//
// One line `ready` with the SHA-256 of every loaded file is printed first. Then
// each stdin line is one JSON operation, answered with the JSON entries it
// produced, one per line, followed by a line `.`. Entries print through
// JSON.stringify so they compare as raw text.
//
// Time is virtual: setTimeout, setInterval and Date.now are replaced by a
// scheduler that only moves on `advance`. Every operation drains microtasks
// (and one setImmediate) before its entries are printed.

import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { readFileSync } from "node:fs";
import { createRequire, registerHooks } from "node:module";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const [paseoRoot, nodeModules] = process.argv.slice(2);
if (!paseoRoot || !nodeModules) {
  throw new Error("usage: relay-daemon-driver.mjs <paseo-root> <node-modules-dir>");
}

const server = `${paseoRoot}/packages/server/src/server`;
const files = {
  "relay-transport.ts": `${server}/relay-transport.ts`,
  "relay-runtime.ts": `${server}/relay-runtime.ts`,
  "encrypted-relay-socket.ts": `${server}/websocket/encrypted-relay-socket.ts`,
  "physical-socket.ts": `${server}/websocket/physical-socket.ts`,
  "daemon-endpoints.ts": `${paseoRoot}/packages/protocol/src/daemon-endpoints.ts`,
  "ws/wrapper.mjs": `${nodeModules}/ws/wrapper.mjs`,
  "ws/package.json": `${nodeModules}/ws/package.json`,
  "typescript/package.json": `${nodeModules}/typescript/package.json`,
};
// physical-socket.ts uses a constructor parameter property, which Node's strip-only mode
// rejects, so that one file goes through the pinned TypeScript compiler instead.
let transpiledDigest = null;
const typescript = createRequire(`${nodeModules}/`)("typescript");
const e2eeStub = new URL("./relay-daemon-e2ee-stub.mjs", import.meta.url).href;

registerHooks({
  load(url, context, nextLoad) {
    if (url.endsWith("/websocket/physical-socket.ts")) {
      const source = typescript.transpileModule(readFileSync(new URL(url), "utf8"), {
        compilerOptions: { module: typescript.ModuleKind.ESNext, target: typescript.ScriptTarget.ES2022 },
      }).outputText;
      transpiledDigest = createHash("sha256").update(source).digest("hex");
      return { format: "module", source, shortCircuit: true };
    }
    return nextLoad(url, context);
  },
  resolve(specifier, context, nextResolve) {
    if (specifier === "ws") {
      return { url: pathToFileURL(files["ws/wrapper.mjs"]).href, shortCircuit: true };
    }
    if (specifier === "@getpaseo/relay/e2ee") {
      return { url: e2eeStub, shortCircuit: true };
    }
    if (specifier === "@getpaseo/protocol/daemon-endpoints") {
      return { url: pathToFileURL(files["daemon-endpoints.ts"]).href, shortCircuit: true };
    }
    if (
      context.parentURL?.endsWith(".ts") &&
      specifier.startsWith("./") &&
      specifier.endsWith(".js")
    ) {
      return nextResolve(`${specifier.slice(0, -3)}.ts`, context);
    }
    return nextResolve(specifier, context);
  },
});

const digests = Object.fromEntries(
  Object.entries(files).map(([name, path]) => [
    name,
    createHash("sha256").update(readFileSync(path)).digest("hex"),
  ]),
);

// ---- virtual time -------------------------------------------------------
const START = 1_000_000_000_000;
let now = START;
let timerSeq = 0;
let timers = new Map(); // handle -> { due, seq, callback, interval }

Date.now = () => now;
globalThis.setTimeout = (callback, delay) => {
  const handle = { id: ++timerSeq, unref() {} };
  timers.set(handle, { due: now + Math.max(0, delay ?? 0), seq: handle.id, callback, interval: null });
  return handle;
};
globalThis.setInterval = (callback, delay) => {
  const handle = { id: ++timerSeq, unref() {} };
  timers.set(handle, { due: now + delay, seq: handle.id, callback, interval: delay });
  return handle;
};
globalThis.clearTimeout = (handle) => {
  timers.delete(handle);
};
globalThis.clearInterval = (handle) => {
  timers.delete(handle);
};

const drain = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
  await new Promise((resolve) => setImmediate(resolve));
};

async function advance(ms) {
  const target = now + ms;
  for (;;) {
    let next = null;
    for (const [handle, timer] of timers) {
      if (!next || timer.due < next[1].due || (timer.due === next[1].due && timer.seq < next[1].seq)) {
        next = [handle, timer];
      }
    }
    if (!next || next[1].due > target) break;
    const [handle, timer] = next;
    now = timer.due;
    if (timer.interval === null) {
      timers.delete(handle);
    } else {
      timer.due += timer.interval;
      timer.seq = ++timerSeq;
    }
    timer.callback();
    await drain();
  }
  now = target;
}

// ---- modules ------------------------------------------------------------
const { startRelayTransport } = await import(pathToFileURL(files["relay-transport.ts"]).href);
const { createRelayRuntime } = await import(pathToFileURL(files["relay-runtime.ts"]).href);
const { createEncryptedRelaySocket } = await import(
  pathToFileURL(files["encrypted-relay-socket.ts"]).href
);
const { MAX_PHYSICAL_SOCKET_BUFFERED_BYTES } = await import(
  pathToFileURL(files["physical-socket.ts"]).href
);
const endpoints = await import(pathToFileURL(files["daemon-endpoints.ts"]).href);
const stub = await import(e2eeStub);

// ---- entries ------------------------------------------------------------
let entries = [];
const log = (entry) => entries.push(entry);
const failure = (error) => (error instanceof Error ? error.message : String(error));
const hex = (buffer) => Buffer.from(new Uint8Array(buffer)).toString("hex");
const dataEntry = (data) => {
  if (typeof data === "string") return { text: data };
  if (data instanceof ArrayBuffer) return { binary: hex(data) };
  return { binary: Buffer.from(data.buffer, data.byteOffset, data.byteLength).toString("hex") };
};
const toJson = (value) =>
  JSON.stringify(value, (key, inner) => (inner instanceof Error ? { error: inner.message } : inner));

function makeLogger(bindings) {
  const emit = (level) => (first, second) => {
    const [fields, message] = typeof first === "string" ? [undefined, first] : [first, second];
    log({ t: "log", level, msg: message, ctx: bindings, fields });
  };
  return {
    debug: emit("debug"),
    info: emit("info"),
    warn: emit("warn"),
    error: emit("error"),
    child: (more) => makeLogger({ ...bindings, ...more }),
  };
}

// ---- fake relay websockets ---------------------------------------------
let sockets = [];
let socketDefaults = {};

class FakeSocket {
  constructor(id, url) {
    this.id = id;
    this.url = url;
    this.readyState = 0;
    this.bufferedAmount = 0;
    this.handlers = new Map();
    this.modes = { ...socketDefaults };
    this.pendingCallbacks = [];
  }

  on(event, listener) {
    const list = this.handlers.get(event) ?? [];
    list.push({ listener, once: false });
    this.handlers.set(event, list);
  }

  once(event, listener) {
    const list = this.handlers.get(event) ?? [];
    list.push({ listener, once: true });
    this.handlers.set(event, list);
  }

  fire(event, ...args) {
    const list = this.handlers.get(event) ?? [];
    for (const entry of [...list]) {
      if (entry.once) list.splice(list.indexOf(entry), 1);
      entry.listener(...args);
    }
  }

  send(data, callback) {
    log({ t: "ws", a: "send", id: this.id, ...dataEntry(data) });
    const mode = this.modes.send ?? "ok";
    if (mode === "throw") throw new Error("send threw");
    if (mode === "error") callback?.(new Error("send failed"));
    else if (mode === "pending") this.pendingCallbacks.push(callback);
    else callback?.();
  }

  close(code, reason) {
    log({ t: "ws", a: "close", id: this.id, code, reason });
    if (this.modes.close === "throw") throw new Error("close threw");
  }

  terminate() {
    log({ t: "ws", a: "terminate", id: this.id });
    if (this.modes.terminate === "throw") throw new Error("terminate threw");
  }

  ping() {
    log({ t: "ws", a: "ping", id: this.id });
    if (this.modes.ping === "throw") throw new Error("ping threw");
  }
}

function createWebSocket(url) {
  const socket = new FakeSocket(sockets.length + 1, url);
  sockets.push(socket);
  log({ t: "ws", a: "create", id: socket.id, url });
  return socket;
}

// ---- transport scenario state ------------------------------------------
let transport = null;
let attachMode = "ok";
let attachWaiters = [];
let attachCount = 0;
let attached = new Map(); // connection id -> attached socket
let listenerMode = "normal";

const attachSocket = (ws, metadata) => {
  attachCount += 1;
  const id = metadata?.relayConnectionId;
  log({ t: "attach", id, kind: ws instanceof FakeSocket ? "plain" : "encrypted", metadata });
  attached.set(id, ws);
  const throwing = (kind) => listenerMode === "throw-all" || listenerMode === `throw-${kind}`;
  if (listenerMode !== "none") {
    ws.on("message", (data) => {
      log({ t: "app", a: "message", id, ...dataEntry(data) });
      if (throwing("message")) throw new Error("listener threw");
    });
    ws.on("close", (code, reason) => {
      log({ t: "app", a: "close", id, code, reason: reason === undefined ? undefined : String(reason) });
      if (throwing("close")) throw new Error("listener threw");
    });
    ws.on("error", (error) => {
      log({ t: "app", a: "error", id, message: failure(error) });
      if (throwing("error")) throw new Error("listener threw");
    });
  }
  if (attachMode === "ok") return Promise.resolve();
  if (attachMode === "reject") return Promise.reject(new Error("attach rejected"));
  return new Promise((resolve, reject) => attachWaiters.push({ resolve, reject }));
};

stub.configure({ log, failure, dataEntry });

function startTransport(options) {
  const keyPair = options.keyPair ? { publicKey: new Uint8Array(32), secretKey: new Uint8Array(32) } : undefined;
  transport = startRelayTransport({
    logger: makeLogger({}),
    attachSocket,
    relayEndpoint: options.endpoint,
    relayUseTls: options.useTls,
    serverId: options.serverId,
    daemonKeyPair: keyPair,
    createWebSocket,
  });
}

// ---- runtime scenario state --------------------------------------------
let runtime = null;
let runtimeStarts = [];
let runtimeStartMode = "ok";

// ---- encrypted socket scenario state -----------------------------------
let enc = null;

function describeValue(value) {
  if (value === undefined) return "undefined";
  return toJson(value);
}

async function operate(op) {
  switch (op.op) {
    case "reset": {
      transport = null;
      runtime = null;
      enc = null;
      timers = new Map();
      now = START;
      sockets = [];
      socketDefaults = {};
      attachMode = "ok";
      attachWaiters = [];
      attachCount = 0;
      attached = new Map();
      listenerMode = "normal";
      runtimeStarts = [];
      runtimeStartMode = "ok";
      stub.reset();
      return;
    }
    case "parseHostPort": {
      try {
        log({ t: "hostport", ...endpoints.parseHostPort(op.input) });
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      return;
    }
    case "buildUrl": {
      try {
        const { op: _op, ...params } = op;
        log({ t: "url", url: endpoints.buildRelayWebSocketUrl(params) });
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      return;
    }
    case "normalizeVersion": {
      try {
        log({ t: "version", value: endpoints.normalizeRelayProtocolVersion(op.value) });
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      return;
    }
    case "socketDefaults": {
      socketDefaults = { ...op.modes };
      return;
    }
    case "socketMode": {
      Object.assign(sockets[op.id - 1].modes, op.modes);
      return;
    }
    case "socketState": {
      const socket = sockets[op.id - 1];
      if (op.readyState !== undefined) socket.readyState = op.readyState;
      if (op.bufferedAmount !== undefined) socket.bufferedAmount = op.bufferedAmount;
      return;
    }
    case "sendCallback": {
      const callback = sockets[op.id - 1].pendingCallbacks.shift();
      callback?.(op.error === undefined ? undefined : new Error(op.error));
      return;
    }
    case "listenerMode": {
      listenerMode = op.mode;
      return;
    }
    case "channelSend": {
      stub.send(op);
      return;
    }
    case "attachMode": {
      attachMode = op.mode;
      return;
    }
    case "attachSettle": {
      const waiter = attachWaiters.shift();
      if (op.result === "reject") waiter.reject(new Error("attach rejected"));
      else waiter.resolve();
      return;
    }
    case "start": {
      try {
        startTransport(op);
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      return;
    }
    case "stop": {
      const current = transport;
      transport = null;
      try {
        await current?.stop();
        log({ t: "stopped" });
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      return;
    }
    case "advance": {
      await advance(op.ms);
      return;
    }
    case "socket": {
      const socket = sockets[op.id - 1];
      switch (op.event) {
        case "open":
          socket.readyState = 1;
          socket.fire("open");
          return;
        case "message": {
          const data = decodeMessage(op);
          socket.fire("message", data, op.isBinary === true);
          return;
        }
        case "close":
          socket.readyState = 3;
          socket.fire(
            "close",
            op.code,
            op.reason === undefined ? undefined : Buffer.from(op.reason, "utf8"),
          );
          return;
        case "error":
          socket.fire("error", new Error(op.message));
          return;
        case "pong":
          socket.fire("pong");
          return;
        default:
          throw new Error(`unknown socket event ${op.event}`);
      }
    }
    case "channel": {
      stub.settle(op);
      return;
    }
    case "channelEvent": {
      stub.event(op);
      return;
    }
    case "channelMode": {
      stub.setMode(op.mode, op);
      return;
    }
    case "runtime": {
      runtimeStartMode = op.startMode ?? "ok";
      runtime = createRelayRuntime({
        config: op.config,
        logger: makeLogger({ runtime: true }),
        attachSocket: async () => undefined,
        serverId: op.serverId,
        daemonKeyPair: { publicKey: new Uint8Array(32), secretKey: new Uint8Array(32) },
        startTransport: (options) => {
          runtimeStarts.push(options.relayEndpoint);
          log({ t: "runtime", a: "start", endpoint: options.relayEndpoint, useTls: options.relayUseTls, serverId: options.serverId });
          if (runtimeStartMode === "throw") throw new Error("Invalid relay endpoint");
          return {
            stop: async () => {
              log({ t: "runtime", a: "stop" });
              if (runtimeStartMode === "stop-rejects") throw new Error("stop failed");
            },
          };
        },
      });
      log({ t: "runtime", a: "config", config: runtime.getConfig() });
      return;
    }
    case "runtimeStartMode": {
      runtimeStartMode = op.mode;
      return;
    }
    case "setEnabled": {
      try {
        runtime.setEnabled(op.enabled);
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      log({ t: "runtime", a: "config", config: runtime.getConfig() });
      return;
    }
    case "runtimeStop": {
      try {
        await runtime.stop();
        log({ t: "runtime", a: "stopped" });
      } catch (error) {
        log({ t: "throw", message: failure(error) });
      }
      return;
    }
    case "enc.create": {
      const channel = {
        sendMode: op.sendMode ?? "sync",
        pending: [],
        setState: (state) => log({ t: "enc", a: "setState", state }),
        send: (data) => {
          log({ t: "enc", a: "channel.send", ...dataEntry(data) });
          switch (channel.sendMode) {
            case "reject":
              return Promise.reject(new Error("channel send failed"));
            case "pending":
              return new Promise((resolve, reject) => channel.pending.push({ resolve, reject }));
            default:
              return Promise.resolve();
          }
        },
        outboundWireByteLength: (data) =>
          (typeof data === "string" ? Buffer.byteLength(data, "utf8") : data.byteLength) +
          (op.overhead ?? 40),
        close: (code, reason) => {
          log({ t: "enc", a: "channel.close", code, reason });
          if (op.closeThrows) throw new Error("channel close threw");
        },
      };
      const emitter = new EventEmitter();
      const state = { buffered: op.buffered, sends: [] };
      const socket = createEncryptedRelaySocket({
        channel,
        emitter,
        getTransportBufferedAmount: () => state.buffered,
        terminateTransport: () => {
          log({ t: "enc", a: "terminateTransport" });
          if (op.terminateThrows) throw new Error("terminate threw");
        },
      });
      if (op.listeners !== false) {
        socket.on("error", (error) => log({ t: "enc", a: "emit.error", message: failure(error) }));
      }
      socket.on("close", (code, reason) => log({ t: "enc", a: "emit.close", code, reason }));
      socket.on("message", (data) => log({ t: "enc", a: "emit.message", ...dataEntry(data) }));
      enc = { channel, emitter, socket, state };
      log({ t: "enc", a: "created", readyState: socket.readyState, bufferedAmount: socket.bufferedAmount });
      return;
    }
    case "enc.send": {
      const data = op.text !== undefined ? op.text : decodeBinary(op);
      const index = enc.state.sends.length;
      try {
        const result = enc.socket.send(data);
        enc.state.sends.push(result);
        if (result && typeof result.then === "function") {
          result.then(
            () => log({ t: "enc", a: "send.settled", index, result: "resolved" }),
            (error) => log({ t: "enc", a: "send.settled", index, result: "rejected", message: failure(error) }),
          );
        } else {
          log({ t: "enc", a: "send.returned", index, value: describeValue(result) });
        }
      } catch (error) {
        log({ t: "enc", a: "send.threw", index, message: failure(error) });
      }
      return;
    }
    case "enc.settle": {
      const waiter = enc.channel.pending.shift();
      if (op.result === "reject") waiter.reject(new Error("channel send failed"));
      else waiter.resolve();
      return;
    }
    case "enc.state": {
      if ("buffered" in op) enc.state.buffered = op.buffered === null ? undefined : op.buffered;
      return;
    }
    case "enc.close": {
      try {
        enc.socket.close(op.code, op.reason);
      } catch (error) {
        log({ t: "enc", a: "threw", op: "close", message: failure(error) });
      }
      return;
    }
    case "enc.terminate": {
      try {
        enc.socket.terminate();
      } catch (error) {
        log({ t: "enc", a: "threw", op: "terminate", message: failure(error) });
      }
      return;
    }
    case "enc.emit": {
      if (op.event === "close") enc.emitter.emit("close", op.code, op.reason);
      else if (op.event === "error") enc.emitter.emit("error", new Error(op.message));
      else enc.emitter.emit("message", op.text);
      return;
    }
    case "enc.read": {
      log({
        t: "enc",
        a: "read",
        readyState: enc.socket.readyState,
        bufferedAmount: enc.socket.bufferedAmount,
      });
      return;
    }
    case "app.send": {
      const ws = attached.get(op.id);
      const data = op.text !== undefined ? op.text : decodeBinary(op);
      try {
        const result = ws.send(data);
        Promise.resolve(result).then(
          () => log({ t: "app", a: "send.settled", id: op.id, result: "resolved" }),
          (error) =>
            log({ t: "app", a: "send.settled", id: op.id, result: "rejected", message: failure(error) }),
        );
      } catch (error) {
        log({ t: "app", a: "send.threw", id: op.id, message: failure(error) });
      }
      return;
    }
    case "app.close": {
      attached.get(op.id).close(op.code, op.reason);
      return;
    }
    case "app.terminate": {
      attached.get(op.id).terminate();
      return;
    }
    case "app.read": {
      const ws = attached.get(op.id);
      log({ t: "app", a: "read", id: op.id, readyState: ws.readyState, bufferedAmount: ws.bufferedAmount });
      return;
    }
    case "constants": {
      log({ t: "constants", maxPhysicalSocketBufferedBytes: MAX_PHYSICAL_SOCKET_BUFFERED_BYTES });
      return;
    }
    default:
      throw new Error(`unknown operation ${op.op}`);
  }
}

function decodeBinary(op) {
  const view = Uint8Array.from(Buffer.from(op.binary, "hex"));
  return op.view === true ? view : view.buffer;
}

function decodeMessage(op) {
  switch (op.kind ?? "buffer") {
    case "buffer":
      return Buffer.from(op.hex ?? Buffer.from(op.text, "utf8").toString("hex"), "hex");
    case "string":
      return op.text;
    case "arraybuffer":
      return Uint8Array.from(Buffer.from(op.hex, "hex")).buffer;
    case "fragments":
      return op.parts.map((part) => Buffer.from(part, "hex"));
    default:
      throw new Error(`unknown message kind ${op.kind}`);
  }
}

digests["physical-socket.transpiled"] = transpiledDigest;
// daemon-worker.ts logs fatal and exits on an unhandled rejection.
process.on("unhandledRejection", (reason) =>
  log({ t: "fatal", kind: "unhandledRejection", message: failure(reason) }),
);

console.log(JSON.stringify({ ready: true, node: process.version, digests }));

let queue = Promise.resolve();
const lines = createInterface({ input: process.stdin });
lines.on("line", (line) => {
  queue = queue.then(async () => {
    entries = [];
    try {
      await operate(JSON.parse(line));
    } catch (error) {
      // daemon-worker.ts logs fatal and exits on an uncaught exception.
      log({ t: "fatal", kind: "uncaughtException", message: failure(error) });
    }
    await drain();
    for (const entry of entries) console.log(toJson(entry));
    console.log(".");
  });
});
lines.on("close", () => {
  queue.then(() => process.exit(0));
});
