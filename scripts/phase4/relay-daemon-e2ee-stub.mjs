// Stand-in for `@getpaseo/relay/e2ee` used by relay-daemon-driver.mjs. It records every
// createDaemonChannel call and lets the scenario decide when each one settles. The real
// channel is covered by the spocky-crypto differential.

let helpers = null;
let channels = [];
let mode = { kind: "pending" };

export function configure(value) {
  helpers = value;
}

export function reset() {
  channels = [];
  mode = { kind: "pending" };
}

export function setMode(kind, options) {
  mode = { kind, message: options.message };
}

function describeMessage(message) {
  return { ...helpers.dataEntry(message.data), isBinary: message.isBinary };
}

export function createDaemonChannel(transport, keyPair, events) {
  const entry = { n: channels.length + 1, transport, events, waiter: null, resolved: false };
  channels.push(entry);
  helpers.log({
    t: "channel",
    a: "create",
    n: entry.n,
    keyPairShape: [keyPair.publicKey.length, keyPair.secretKey.length],
  });
  const channel = {
    setState: (state) => helpers.log({ t: "channel", a: "setState", n: entry.n, state }),
    send: (data) => {
      helpers.log({ t: "channel", a: "send", n: entry.n, ...helpers.dataEntry(data) });
      return Promise.resolve();
    },
    outboundWireByteLength: (data) =>
      (typeof data === "string" ? Buffer.byteLength(data, "utf8") : data.byteLength) + 40,
    close: (code, reason) => helpers.log({ t: "channel", a: "close", n: entry.n, code, reason }),
  };
  entry.channel = channel;
  transport.onmessage = (message) =>
    helpers.log({ t: "channel", a: "rx", n: entry.n, ...describeMessage(message) });
  // The real createDaemonChannel rejects when the transport closes or fails before the
  // handshake completes (encrypted-channel.ts:296-312), and then reports them as events.
  const becomeOpen = () => {
    entry.resolved = true;
    transport.onclose = (code, reason) =>
      helpers.log({ t: "channel", a: "closed", n: entry.n, code, reason });
    transport.onerror = (error) =>
      helpers.log({ t: "channel", a: "error", n: entry.n, message: helpers.failure(error) });
  };
  return new Promise((resolve, reject) => {
    transport.onclose = (code, reason) =>
      reject(new Error(`Connection closed during handshake: ${code} ${reason}`));
    transport.onerror = (error) => reject(error);
    entry.waiter = {
      resolve: () => {
        becomeOpen();
        resolve(channel);
      },
      reject,
    };
    if (mode.kind === "ok") entry.waiter.resolve();
    else if (mode.kind === "fail") reject(new Error(mode.message ?? "handshake failed"));
  });
}

/** The channel sends a frame through the transport adapter (relay-transport.ts:470-486). */
export function send(op) {
  const entry = channels[op.n - 1];
  const data = op.text !== undefined ? op.text : Uint8Array.from(Buffer.from(op.binary, "hex")).buffer;
  entry.transport.send(data).then(
    () => helpers.log({ t: "channel", a: "transport.send.settled", n: entry.n, result: "resolved" }),
    (error) =>
      helpers.log({
        t: "channel",
        a: "transport.send.settled",
        n: entry.n,
        result: "rejected",
        message: helpers.failure(error),
      }),
  );
}

export function settle(op) {
  const entry = channels[op.n - 1];
  if (op.result === "fail") entry.waiter.reject(new Error(op.message ?? "handshake failed"));
  else entry.waiter.resolve();
}

export function event(op) {
  const entry = channels[op.n - 1];
  if (op.event === "message") {
    entry.events.onmessage?.(op.text !== undefined ? op.text : Uint8Array.from(Buffer.from(op.binary, "hex")).buffer);
  } else if (op.event === "close") {
    entry.events.onclose?.(op.code, op.reason);
  } else {
    entry.events.onerror?.(new Error(op.message));
  }
}
