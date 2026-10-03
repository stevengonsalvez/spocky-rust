# Phase 3 e2ee channel: TypeScript versus Rust

Lane `p3_e2ee_channel` ports the pinned relay `encrypted-channel.ts` onto
the existing `spocky-crypto` primitives and proves it against the original
source under node 22.20.0. Capabilities: `DSEC-003`, `CLOUD-RELAY-E2EE-007`.

## Inputs

| Input | Identity |
|---|---|
| Paseo reference | `5de45e208690b0efc51c59a585ae9729325a9204`, clean tracked tree |
| `packages/relay/src/encrypted-channel.ts` | `2b0d31520917d24993644fc30f065be4d46d3517475f8821388a2263ced3e6a6` |
| `packages/relay/src/crypto.ts` | `309d5cb94ceb236ced93a188d0a0bc3b42a0e347815b4dbe6679374d810db845` |
| `packages/relay/src/base64.ts` | `74d69461af9727aa3fb70b37ee50ae54e467e1eebf26fb0356453557094137e7` |
| `tweetnacl/nacl-fast.js` (1.0.3) | `6bcd37a3b20dce913f82d4b23e4e2b661058b4b953df8a3f8c45d56ac4f72447` |
| `base64-js/index.js` (1.5.1) | `829eadd8a1a441d25be0cb93b00e16a0d0c20fd294db95d8f2ed87e6954b7182` |
| node | `v22.20.0`, binary SHA-256 `1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931`, asserted by the test and by `scripts/phase3/pins.sh` |

Dependencies come from the pinned-lockfile install made by
`scripts/phase3/build-original.sh`; the test asserts every digest above
(and both `package.json` digests) before it runs a scenario.

## Method

`scripts/phase3/e2ee-driver.mjs` loads the three relay sources unchanged
through node type stripping and executes JSON operations against one
channel endpoint. `crates/spocky-crypto/tests/support/mod.rs` executes the
same operations on the Rust port. After every operation both sides emit
entries rendered as `JSON.stringify` output: every transport frame with its
exact text or bytes, every transport close code and reason, `onopen`,
`onmessage`, `onerror`, `onclose`, handshake and creation outcomes, send
results, `isOpen`, and wire lengths. The entries must be equal as strings.

Pair scenarios connect a client and a daemon and deliver every frame each
side writes to the other: TypeScript client with Rust daemon, Rust client
with TypeScript daemon, and both same-language pairs. All four transcripts
must be byte-identical.

Nothing is normalized. The client key pair and all nonces come from a
seeded xorshift32 byte stream injected through `nacl.setPRNG` on the
TypeScript side and the channel random source on the Rust side.

## Coverage

60 single-endpoint scenarios and 4 pair transcripts (the run fails unless
exactly 60 have both transcripts):

- client and daemon handshakes, `binaryCiphertext` negotiation both ways,
  legacy peers, base64 text frames and raw binary frames;
- tampered ciphertext and nonce, foreign key, truncation at 23, 24, 39, and
  n-1 bytes, empty and `=` frames, base64-js garbage decoding, plaintext
  JSON, opcode and payload mismatches, invalid UTF-8, and BOM stripping;
- replayed and reordered frames (both delivered: no live-session replay
  tracking), and delivery after a 1011 close (the channel stays open);
- the 200-send handshake backlog: 205 queued sends flush exactly the newest
  200 ciphertexts byte for byte;
- backlog flush failure, pending flushes interleaved with live sends, and a
  transport close during a pending flush;
- re-entrant sends from `onopen` ahead of the backlog;
- 16 invalid daemon hellos with exact `Invalid hello message (...)` texts,
  including lone surrogates, lossy UTF-8 decoding, and long previews;
- ready-send failure, pending ready with buffering and filtered replay,
  close or error during the handshake, and an open after a rejection;
- re-hello reuse, key mismatch close 1008, and every re-hello fall-through;
- the `plaintext frame` rethrow: a re-hello whose send (rejected or pending)
  or close fails with text containing `plaintext frame` closes the
  transport with 1011 and that text instead of falling through, and V8
  `JSON.parse` errors that quote the frame itself (`{"plaintext frame":}`)
  do the same; a corpus of several hundred mutated frames compares the V8
  `Unexpected token` message against the Rust reproduction;
- only `Error` rejections are rethrown: a re-hello send rejected or
  settled with a string value falls through to ciphertext decoding where the
  same text as an `Error` closes 1011, and string rejections of hello, app,
  pending, and ready sends report `String(value)`;
- frames delivered in one task: a batch runs every `onmessage` call before
  any awaited send continues, so a daemon buffers frames behind its ready
  send, drops buffered hello and ready frames, and replays the rest in
  order; a client handles frames while its backlog flushes, including a
  failing flush. The Rust side reports sends that would settle at once as
  `Pending` during the batch and settles them after it;
- hello retry timing, including retries that continue after `close()`;
- `JSON.parse`, `TextDecoder`, `base64ToArrayBuffer`, and wire-size helpers
  compared directly against the pinned runtime.

A mutation check (not committed) confirmed the harness fails for a changed
backlog limit, unfiltered replay, a kept-open channel after key mismatch, a
returned re-hello failure, a non-legacy plaintext decode, an always-firing
retry tick, sends that settle at once during a batch, and a rethrow that
ignores whether the rejection is an `Error`.

## Run

```sh
scripts/phase3/e2ee-differential.sh
```

Run `e2ee-20261003T121413Z` at commit `d1750e63`:

| Command | Result |
|---|---|
| `SPOCKY_PINNED_NODE=... cargo test --locked -p spocky-crypto` | lib 15, `baseline_vectors` 6, `channel_differential` 27 passed |
| `cargo clippy --locked -p spocky-crypto --all-targets -- -D warnings` | clean |
| `cargo fmt --package spocky-crypto -- --check` | clean |

| Raw artifact (untracked, `evidence/raw/phase3/e2ee-20261003T121413Z/`) | SHA-256 |
|---|---|
| `inputs.txt` | `bdfcab72ea069b6ee8b0b0f7e2032fb0dafdaea142a13b781d0aa446760c726b` |
| `test.log` | `10ad6080e0cdaa6f793b9daa0a66f8aeee11ba9bb183d0ab922a1e3d07ffaffe` |
| `clippy.log` | `49f81784397e1d67ad05223cfa5cb94440e04542c7a696ec38349873772f6d2b` |
| `fmt.log` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `transcripts/*.txt`, concatenated by name | `03d82feef48513499cafc63f360ffc38006bb36dd612076a033d359849e3ad13` |

The 53-scenario set (`179c8daf...2233`) reproduced across two independent
runs, including the reviewer's; this 60-scenario digest comes from a single
run.

## Known defects reproduced

- No replay or reordering protection within a live session.
- The client handshake backlog keeps 200 sends and silently drops older ones.
- A 1011 decryption or protocol close leaves the channel open.
- A failed daemon re-hello falls through to ciphertext decoding of the hello,
  unless the failure is an `Error` whose text contains `plaintext frame`,
  which closes 1011 with that text.
- A daemon hello whose key is rejected buffers every later frame forever.
- `close()` on a handshaking client leaves the hello retry running until the
  transport reports its close.

## Gaps

- Scheduling: the original yields a microtask after a send that settles at
  once. The Rust port matches only when a runtime reports each such send as
  `Pending` and settles it after the frames of the same task, as the pinned
  daemon transport and the batch scenarios do. A runtime that reports `Sent`
  and delivers frames synchronously in the same task sees different
  interleavings.
- A transport `close` that throws a value that is not an `Error` cannot be
  expressed through `Transport::close`, so the original rule (rethrow only
  `Error` instances) is covered for send rejections and not for close
  throws, which `ws` raises as `Error`.
- Event callbacks cannot throw in Rust, so the original paths where a
  throwing `onopen` skips the backlog flush have no equivalent.
- Lone surrogates in a rethrown close reason are unreachable through the
  JSON path: a `JSON.parse` message contains `plaintext frame` only when the
  whole source (at most 20 UTF-16 units) is quoted, so no 10-unit context
  window can split a surrogate pair. A search of about 88,000 mutated frames
  found no message holding the marker and a lone surrogate. The only other
  source is a transport error text, which Rust carries as `String` and
  cannot hold a lone surrogate.
- No real WebSocket, relay, or daemon runtime is exercised here; transport
  behavior belongs to `CLOUD-RELAY-DAEMON-008`.
- Coverage is macOS x64 with node 22.20.0 only.
- Claude-authored under the 2026-10-01 routing revision; not Sol-authored
  for success criterion 3.
