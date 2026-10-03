// Drives one pinned Paseo relay encrypted-channel endpoint for the
// spocky-crypto differential test.
//
// Usage: node e2ee-driver.mjs <relay-src-dir> <node-modules-dir>
//
// Loads packages/relay/src/{encrypted-channel,crypto,base64}.ts unchanged
// (Node 22.20 strips their types; a resolve hook maps their `./x.js`
// imports to `./x.ts` and the bare `tweetnacl` and `base64-js` imports to
// the given node_modules). Prints one `ready` line with the SHA-256 of every
// loaded file, then reads one JSON operation per stdin line and answers each
// with the JSON entries it produced, one per line, followed by a line `.`.
// An operation runs in its own macrotask and is drained (microtasks, then
// one setImmediate) before its entries are printed.
//
// Randomness: tweetnacl's PRNG is replaced through nacl.setPRNG with a
// seeded xorshift32 byte stream, the same seam the Rust side injects. The
// relay crypto module consumes one random byte the first time it checks
// its PRNG, so a real-random generateKeyPair() runs before the seeded
// stream is installed.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const [relaySrc, nodeModules] = process.argv.slice(2);
if (!relaySrc || !nodeModules) {
  throw new Error("usage: e2ee-driver.mjs <relay-src-dir> <node-modules-dir>");
}

const files = {
  "encrypted-channel.ts": `${relaySrc}/encrypted-channel.ts`,
  "crypto.ts": `${relaySrc}/crypto.ts`,
  "base64.ts": `${relaySrc}/base64.ts`,
  "tweetnacl/nacl-fast.js": `${nodeModules}/tweetnacl/nacl-fast.js`,
  "tweetnacl/package.json": `${nodeModules}/tweetnacl/package.json`,
  "base64-js/index.js": `${nodeModules}/base64-js/index.js`,
  "base64-js/package.json": `${nodeModules}/base64-js/package.json`,
};
const packages = {
  tweetnacl: files["tweetnacl/nacl-fast.js"],
  "base64-js": files["base64-js/index.js"],
};

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (packages[specifier]) {
      return {
        url: pathToFileURL(packages[specifier]).href,
        format: "commonjs",
        shortCircuit: true,
      };
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
const channelModule = await import(pathToFileURL(files["encrypted-channel.ts"]).href);
const cryptoModule = await import(pathToFileURL(files["crypto.ts"]).href);
const base64Module = await import(pathToFileURL(files["base64.ts"]).href);
const { default: nacl } = await import("tweetnacl");

cryptoModule.generateKeyPair();
let rngState = 0;
nacl.setPRNG((target, length) => {
  for (let index = 0; index < length; index += 1) {
    rngState ^= rngState << 13;
    rngState >>>= 0;
    rngState ^= rngState >>> 17;
    rngState ^= rngState << 5;
    rngState >>>= 0;
    target[index] = rngState & 0xff;
  }
});

// Interval timers are captured so the `tick` operation fires them.
const intervals = new Map();
let nextInterval = 1;
globalThis.setInterval = (callback) => {
  const handle = { id: nextInterval, unref() {} };
  nextInterval += 1;
  intervals.set(handle, callback);
  return handle;
};
globalThis.clearInterval = (handle) => {
  intervals.delete(handle);
};

const hex = (buffer) => Buffer.from(new Uint8Array(buffer)).toString("hex");
const bytes = (text) => {
  const view = Uint8Array.from(Buffer.from(text, "hex"));
  return view.buffer;
};
const dataEntry = (data) =>
  typeof data === "string" ? { text: data } : { binary: hex(data) };

let entries = [];
const log = (entry) => entries.push(entry);
const failure = (error) => (error instanceof Error ? error.message : String(error));

let persistentMode = { kind: "sync" };
let modeQueue = [];
let closeMode = { kind: "ok" };
const pendingSends = new Map();
let nextSendId = 1;
let onOpenSend = null;

const transport = {
  send(data) {
    const id = nextSendId;
    nextSendId += 1;
    log({ t: "wire", id, ...dataEntry(data) });
    const mode = modeQueue.length > 0 ? modeQueue.shift() : persistentMode;
    switch (mode.kind) {
      case "sync":
        return undefined;
      case "throw":
        throw new Error(mode.message);
      case "reject":
        return Promise.reject(new Error(mode.message));
      case "reject-value":
        // A rejection value that is not an Error instance.
        return Promise.reject(mode.message);
      case "pending":
        return new Promise((resolve, reject) => pendingSends.set(id, { resolve, reject }));
      default:
        throw new Error(`unknown send mode ${mode.kind}`);
    }
  },
  close(code, reason) {
    log({ t: "transport-close", code, reason });
    if (closeMode.kind === "throw") throw new Error(closeMode.message);
  },
  onmessage: null,
  onclose: null,
  onerror: null,
};

let channel = null;
const events = {
  onopen: () => {
    log({ t: "open" });
    if (onOpenSend !== null && channel !== null) {
      void channel.send(onOpenSend).catch(() => undefined);
    }
  },
  onmessage: (data) => log({ t: "message", ...dataEntry(data) }),
  onclose: (code, reason) => log({ t: "close", code, reason }),
  onerror: (error) => log({ t: "error", message: failure(error) }),
};

let nextHandle = 1;
const appData = (op) => (op.binary !== undefined ? bytes(op.binary) : op.text);

function describe(value) {
  if (value === undefined) return "undefined";
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  if (typeof value === "string") return `string:${value}`;
  if (typeof value === "boolean") return `boolean:${value}`;
  return typeof value;
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function run(op) {
  switch (op.op) {
    case "client":
      rngState = op.seed;
      channelModule.createClientChannel(transport, op.daemonKey, events).then(
        (created) => {
          channel = created;
          log({ t: "created", ok: true });
        },
        (error) => log({ t: "created", error: failure(error) }),
      );
      break;
    case "daemon": {
      rngState = op.seed;
      const keyPair = nacl.box.keyPair.fromSecretKey(Uint8Array.from(Buffer.from(op.secret, "hex")));
      channelModule.createDaemonChannel(transport, keyPair, events).then(
        (created) => {
          channel = created;
          log({ t: "handshake", ok: true });
        },
        (error) => log({ t: "handshake", error: failure(error) }),
      );
      break;
    }
    case "raw": {
      rngState = op.seed;
      const options = { binaryCiphertext: op.binary };
      if (op.daemonSecret !== null) {
        options.daemonKeyPair = nacl.box.keyPair.fromSecretKey(
          Uint8Array.from(Buffer.from(op.daemonSecret, "hex")),
        );
      }
      channel = new channelModule.EncryptedChannel(
        transport,
        Uint8Array.from(Buffer.from(op.shared, "hex")),
        events,
        options,
      );
      if (op.open) channel.setState("open");
      break;
    }
    case "deliver": {
      const data = op.binary !== undefined ? bytes(op.binary) : op.text;
      const isBinary = op.isBinary ?? op.binary !== undefined;
      transport.onmessage?.({ data, isBinary });
      break;
    }
    case "batch":
      // Frames a transport delivers in one task, before any awaited send
      // settles.
      for (const frame of op.frames) {
        const data = frame.binary !== undefined ? bytes(frame.binary) : frame.text;
        transport.onmessage?.({ data, isBinary: frame.isBinary ?? frame.binary !== undefined });
      }
      break;
    case "send": {
      const handle = nextHandle;
      nextHandle += 1;
      channel.send(appData(op)).then(
        () => log({ t: "sent", handle, ok: true }),
        (error) => log({ t: "sent", handle, error: failure(error) }),
      );
      break;
    }
    case "mode":
      persistentMode = { kind: op.kind, message: op.message };
      break;
    case "queue":
      modeQueue = op.modes.map((mode) => ({ kind: mode.kind, message: mode.message }));
      break;
    case "close-mode":
      closeMode = { kind: op.kind, message: op.message };
      break;
    case "settle": {
      const pending = pendingSends.get(op.id);
      pendingSends.delete(op.id);
      if (op.errorValue !== undefined) pending.reject(op.errorValue);
      else if (op.error === null) pending.resolve();
      else pending.reject(new Error(op.error));
      break;
    }
    case "tick":
      for (const callback of [...intervals.values()]) callback();
      break;
    case "transport-close":
      transport.onclose?.(op.code, op.reason);
      break;
    case "transport-error":
      transport.onerror?.(new Error(op.message));
      break;
    case "close":
      try {
        channel.close(op.code, op.reason);
        log({ t: "closed", ok: true });
      } catch (error) {
        log({ t: "closed", error: failure(error) });
      }
      break;
    case "set-state":
      channel.setState(op.state);
      break;
    case "is-open":
      log({ t: "is-open", value: channel.isOpen() });
      break;
    case "wire-length":
      log({ t: "wire-length", value: channel.outboundWireByteLength(appData(op)) });
      break;
    case "on-open-send":
      onOpenSend = op.text;
      break;
    case "probe-json": {
      let parsed;
      try {
        parsed = JSON.parse(op.text);
      } catch {
        log({ t: "json", ok: false });
        break;
      }
      const summary = isRecord(parsed)
        ? [
            "record",
            describe(parsed.type),
            describe(parsed.key),
            isRecord(parsed.capabilities)
              ? `record:${describe(parsed.capabilities.binaryCiphertext)}`
              : describe(parsed.capabilities),
          ].join("|")
        : describe(parsed);
      log({ t: "json", ok: true, summary });
      break;
    }
    case "probe-json-error": {
      let message = null;
      try {
        JSON.parse(op.text);
      } catch (error) {
        if (error.message.startsWith("Unexpected token '")) message = error.message;
      }
      log({ t: "json-error", message });
      break;
    }
    case "probe-decode": {
      const input = bytes(op.binary);
      let fatal = null;
      try {
        fatal = new TextDecoder("utf-8", { fatal: true }).decode(input);
      } catch {
        fatal = null;
      }
      log({ t: "decode", lossy: new TextDecoder().decode(input), fatal });
      break;
    }
    case "probe-base64":
      try {
        log({ t: "base64", bytes: hex(base64Module.base64ToArrayBuffer(op.text)) });
      } catch (error) {
        log({ t: "base64", error: failure(error) });
      }
      break;
    case "probe-wire-sizes":
      log({
        t: "wire-sizes",
        encrypted: channelModule.base64EncryptedWireByteLength(op.value),
        plaintext: channelModule.maxBase64EncryptedPlaintextByteLength(op.value),
      });
      break;
    default:
      throw new Error(`unknown operation ${op.op}`);
  }
}

process.stdout.write(`${JSON.stringify({ ready: true, node: process.version, digests })}\n`);

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  if (line.trim() === "") continue;
  entries = [];
  await new Promise((resolve) => setImmediate(resolve));
  run(JSON.parse(line));
  await new Promise((resolve) => setImmediate(resolve));
  const output = entries.map((entry) => JSON.stringify(entry)).join("\n");
  process.stdout.write(output === "" ? ".\n" : `${output}\n.\n`);
}
