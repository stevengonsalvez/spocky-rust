# Daemon relay client, slice 2 core

Capability: CLOUD-RELAY-DAEMON-008 (daemon relay control, client sockets, reconnect, backpressure).
Crate: `spocky-daemon-relay`, sans-IO. Sockets, timers and logging sit behind `RelayIo`; the
differential harness and a later network runner implement it.

## Baseline

- Paseo `5de45e208690b0efc51c59a585ae9729325a9204`, node `v22.20.0`.
- Loaded unchanged by `scripts/phase4/relay-daemon-driver.mjs`: `relay-transport.ts`,
  `relay-runtime.ts`, `websocket/encrypted-relay-socket.ts`, `websocket/physical-socket.ts`
  (compiled by the pinned TypeScript 5.9.3 because Node's strip-only mode rejects its
  constructor parameter property), `packages/protocol/src/daemon-endpoints.ts`, and the pinned
  `ws` wrapper. The test checks the SHA-256 of every loaded file.
- Replaced: `@getpaseo/relay/e2ee` by a recording stand-in for `createDaemonChannel`. The channel
  is covered by the spocky-crypto differential.
- Time is virtual (`setTimeout`, `setInterval`, `Date.now`); sockets are fakes. Every operation
  drains microtasks before its entries print.

## Compared

Each operation runs on the pinned TypeScript and on the Rust port, and its entries must be
identical raw text: every socket created with its URL, ping, terminate, close and send, every log
record with bindings and fields, every attach with metadata, every frame the application or the
channel receives, every encrypted socket state.

| Area | Operations |
|---|---|
| Endpoint | 4,424 operations: hosts (26 IDNA vectors for bidi, joiners, newer Unicode, `xn--` labels; IPv4 forms, IPv6, userinfo, `?` and `#` in the host), ports, server ids, connection ids, versions |
| Lifecycle | open, keepalive, pong, stale termination, ready timeout, close, reconnect backoff to the 30 s cap over 34 attempts |
| Data sockets | create from `sync` and `connected`, 15 s open timeout, close, `disconnected`, duplicate ids |
| Stop | before open, with data sockets, close throwing, events after stop |
| Adapter send | the transport adapter's `send` (`relay-transport.ts:470-486`): ok, error callback, synchronous throw, pending callback settled later, and the `relay_socket_send_failed` warn record |
| End-to-end attach | channel pending, ready, failed, attach pending, rejected, frames queued until attach, close and error events, 1011 on failure; the stand-in channel rejects when the transport closes or errors before the handshake; a listener throwing during the pending flush |
| Encrypted socket | exact 64 MiB bound accepted and one byte over rejected, terminate, close, closed state, listeners present and absent (also throwing), multi-byte payloads, `close` and `terminate` throwing |
| Runtime | enable, disable, repeat, start throwing, stop rejecting |
| Control messages | 116 frames: types, whitespace ids (U+00A0, U+2028, U+FEFF, U+0085, U+180E), lone surrogates, duplicate keys, non-objects, BOM, invalid UTF-8 (truncated and overlong sequences), nesting to 100,000 levels, `Buffer`, `ArrayBuffer`, fragment arrays |

Result: endpoint 4,424 operations and 13,272 transcript lines; relay client 131 scenarios (15 named,
116 control message), 1,231 operations, 3,857 transcript lines. No difference apart from the host
list below. Tests assert these counts. Run:
`SPOCKY_PINNED_NODE=$HOME/.nvm/versions/node/v22.20.0/bin/node cargo test -p spocky-daemon-relay`
(`SPOCKY_RELAY_DAEMON_EVIDENCE=<dir>` keeps the raw transcripts).

## Findings

- `new URL(...)` followed by `searchParams.set` re-serializes the whole query. A `?` inside the
  host puts the rest of the host into the query, and it is re-encoded as form data before the new
  pairs. The port rebuilds the pair list the same way.
- `sync` ids are filtered but not trimmed; `connected` and `disconnected` ids are trimmed (JS
  `trim`, which keeps U+0085 and strips U+FEFF). Ids stay UTF-16, so a lone surrogate becomes
  U+FFFD only in the URL.
- A data socket that opens after `stop()` is still attached: the original's open handler has no
  `stopped` check. The port keeps that.

## Baseline outcome of the process-level gaps

`daemon-worker.ts:344` (`uncaughtException`) and `:348` (`unhandledRejection`) log a `fatal`
record and exit the process. The driver models both: a throw that reaches the top of an operation,
and an unhandled rejection, print `{"t":"fatal","kind":"uncaughtException"|"unhandledRejection",
"message":...}`. The Rust side returns a `Fatal` value and the harness prints the same entry.

| Case | Baseline outcome | Covered by |
|---|---|---|
| plain attach rejects (`attachSocket`) | `unhandledRejection`, fatal and exit | `fatal` scenario |
| channel listener throws on message, close or error (`listenerMode`) | `uncaughtException`, fatal and exit | `fatal` scenario |
| `emitter.emit("error")` with no listener | throws, `uncaughtException`, fatal and exit | `fatal` scenario (`none`) |
| listener throws while the pending queue flushes | flush fails, `relay_e2ee_handshake_failed` warn, close 1011, socket stays attached | `fatal` scenario |
| encrypted socket `close` or `terminate` throws | exception reaches the caller | `encrypted-socket-throws` scenario |

## Known gaps

- IDNA hosts: Node 22 uses ada; the `url` crate's idna differs on bidi, joiner and newer-Unicode
  cases. `endpoint_differential` lists the divergent hosts (`KNOWN_DIVERGENT_HOSTS`: U+0661 `.com`,
  `1.` U+05D0, U+1FAE9 `.test`, `xn--9hb.com`, `a` U+05D0 `.com`, U+0661 U+0627 `.com`, U+1E9E
  `.com`). The test fails on any other difference and on a listed host that stops diverging. The
  switch to `spocky_contracts::url` (ada 2.9.2 port) removes the list.
- The network runner (real client sockets, TLS, timers, the end-to-end channel) is the next
  commit range.
- The network runner (real client sockets, TLS, timers, the end-to-end channel) is the next
  commit range.
