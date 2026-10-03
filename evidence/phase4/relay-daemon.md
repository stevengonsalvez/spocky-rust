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
| Endpoint | about 1,700: hosts (IDNA, IPv4 forms, IPv6, userinfo, `?` and `#` in the host), ports, server ids, connection ids, versions |
| Lifecycle | open, keepalive, pong, stale termination, ready timeout, close, reconnect backoff to the 30 s cap over 34 attempts |
| Data sockets | create from `sync` and `connected`, 15 s open timeout, close, `disconnected`, duplicate ids |
| Stop | before open, with data sockets, close throwing, events after stop |
| End-to-end attach | channel pending, ready, failed, attach pending, rejected, frames queued until attach, close and error events, 1011 on failure |
| Encrypted socket | exact 64 MiB bound accepted and one byte over rejected, terminate, close, closed state, listeners present and absent, multi-byte payloads |
| Runtime | enable, disable, repeat, start throwing, stop rejecting |
| Control messages | 100 frames: types, whitespace ids (U+00A0, U+2028, U+FEFF, U+0085, U+180E), lone surrogates, duplicate keys, non-objects, BOM, invalid UTF-8, `Buffer`, `ArrayBuffer`, fragment arrays |

Result: 115 scenarios, 997 operations, 3,084 transcript lines, no difference. Run:
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

## Known gaps

- Not modeled: a plain attach whose `attachSocket` promise rejects is an unhandled rejection that
  ends the process in node. The port ignores the result.
- Not modeled: `emitter.emit("error")` with no listener throws in the original. Scenarios keep a
  listener registered.
- The network runner (real client sockets, TLS, timers, the end-to-end channel) is the next
  commit range.
