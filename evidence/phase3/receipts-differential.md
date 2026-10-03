# Phase 3 message receipts differential evidence

Lane `p3_message_receipts`, capability `DRECEIPT-001`, 2026-10-01. Status:
the Rust port matches the pinned build on every scripted step, on macOS x64
only. Claude-authored under the 2026-10-01 routing revision, not
Sol-authored for success criterion 3.

## Command

```sh
gtimeout --kill-after=30 1800 scripts/phase3/receipts-differential.sh
```

Exit 0. The script runs the three lane acceptance commands through the build
gate with `SPOCKY_PINNED_NODE` (node 22.20.0, digest checked by
`scripts/phase3/pins.sh`) and `SPOCKY_PASEO_DIST` set, and with
`SPOCKY_ALLOW_SKIP` unset. It exits nonzero unless the worktree is clean
outside `logs/` (session hook output; the full `git status --porcelain` is
kept in `git-status.txt`), every command passes, and both normalized
differential outputs exist and are byte-identical. All three commands run
through the gate:

```sh
cargo test --locked -p spocky-message-receipts
cargo clippy --locked -p spocky-message-receipts --all-targets -- -D warnings
cargo fmt --package spocky-message-receipts -- --check
```

Test counts: 4 unit, 7 ported (`tests/receipts.rs`, including call-order
queueing when futures are polled out of order, awaiting a later send first,
and dropping a send), 7 differential (`tests/receipts_differential.rs`; the 2
old-home cross-read tests were added by lane `p3_contracts`, and one checks
that a hung Rust side fails instead of hanging), 0 doc tests. 18 passed, 0
failed, 0 ignored. Clippy and fmt are clean. The Rust side of every
differential is bounded to 120 s, like node's. The differential first fails
unless node reports `v22.20.0` and both dist modules match the digests below.
One earlier run of this suite exited nonzero while the check helper was
sending its output to `/dev/null`, so the failing test was not recorded. That
run was made while the shared disk was nearly full (the coordinator reported
19G free and falling soon after), and the suite writes disposable roots under
the temp directory, so a transient `ENOSPC` is the one hypothesis that fits;
it is an inference and was not verified. It did not recur:
`scripts/phase3/receipts-flake-hunt.sh` keeps the full output of every run,
and it ran the whole suite (lib, ported, differential and doc tests) 150 times
under 24 busy loops (twice the CPU count) with no failure, on top of 12 earlier
runs with output kept and 50 runs of the three test binaries under 12 busy
loops. Any new failure leaves its log and its failing test in `failures.txt`.

The runner's own failure handling is proven by:

```sh
gtimeout --kill-after=30 1800 scripts/phase3/receipts-differential.test.sh
```

Exit 0, 8 cases, each running the committed runner from a throwaway
detached worktree at HEAD: a stubbed match exits 0 even with
`SPOCKY_ALLOW_SKIP=1` exported; mismatched outputs, missing outputs, a
failing test with matching outputs, failing clippy, failing fmt, and an
untracked file in the tree exit nonzero; a real run against a dist with a
tampered `index.js` exits nonzero on the pinned digest. Removing the output
comparison, the test exit check, or the `SPOCKY_ALLOW_SKIP` unset from the
runner each made one case fail.

The two-instance race step passed 20 consecutive gated runs of
`receipts_match_pinned_build` at `4128f37`.

## Recorded run `receipts-20261003T191742Z`

Raw evidence lives under `evidence/raw/phase3/receipts-20261003T191742Z/`
(untracked).

| Input | Value |
|---|---|
| Commit | `63e1fd678dd2d4e2f6f6fa98bf6ad1502b6a032a` |
| Node | `v22.20.0` |
| Pinned dist | `paseo-original-5de45e208690b0efc51c59a585ae9729325a9204/packages/server/dist/server` |
| `server/message-receipts/index.js` SHA-256 | `e99ca1a266f038efbceaf398b45ccb2e904a58ca46e4422546dc22ea498c4559` |
| `server/atomic-file.js` SHA-256 | `835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25` |

| File | SHA-256 |
|---|---|
| `receipts-node-normalized.json` | `bd7d6e5734284c4bb8ddf82b0ded8c8beaa27c638078143eab06e2fadc738c63` |
| `receipts-rust-normalized.json` | `bd7d6e5734284c4bb8ddf82b0ded8c8beaa27c638078143eab06e2fadc738c63` |
| `receipts-node-raw.json` | `381c9bcb67437095b654190f5104ea04cd5a401cd6629ebec6f74313e8b88742` |
| `receipts-rust-raw.json` | `b5e40db1e58e6160d424800e0fbbeb26bc9bbff6730004aa637da97416c312df` |
| `inputs.txt` | `2eb2d7669136a67156c88ec4c71e27d584bd1a2f51bbd27f753b4ed4907cbcb4` |
| `test.log` | `902c29d58c1778edeb93cf700b2c1f1f782afb3a71ed90be58f5f02882b99a02` |
| `clippy.log` | `89016f5632de499021c3528c1d970aeb58c5b48909a4762631c71ec7b50d2e56` |
| `fmt.log` (empty) | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `git-status.txt` (only `logs/` entries) | `e2a2110fea7d4e3178c32260751f4a1a1fd79c4df695769f5d2addbb096c7fe3` |

The normalized outputs are byte-identical, and an earlier gated run of the
same 54 steps produced the same digest. Raw digests differ per run
because temp names carry the pid, clock, and a random UUID.

## What is compared

Node imports the built `server/message-receipts/index.js` and runs 54
scripted steps in one disposable root; the port runs the same steps in
another. After every step both print each send's result (`{ ok: true }`, or
`{ name, message, ...error }` with node's own enumerable error fields), the
send and prepare callback counts, and every file and directory under the
root with its mode, SHA-256, and text. The two JSON texts must be equal.

| Scenario | Steps |
|---|---|
| First delivery | pending then completed receipt, 2-space JSON, key order `fingerprint`, `agentId`, `state` |
| Duplicates | two concurrent sends of a completed id, a restarted instance, reordered request keys (`localeCompare` and index-key order) |
| Per-key serialization | two concurrent sends of a fresh id on one instance: one prepare, one delivery |
| Two instances, one directory | concurrent sends of a fresh id from two instances both prepare and deliver, as in the pinned build; both wait in `prepare` until both arrive, so both reads finish before either write on both sides |
| Conflicts | same ids with another request: `agent_request_key_conflict` |
| Failed send | `connection lost`, then a restart: `agent_request_outcome_unknown` |
| Failed prepare | no receipt left; a later prepared send delivers once |
| Corruption | empty, cut, garbage, multi-line, wrong shape, array, null, byte order mark, invalid and truncated UTF-8: exact V8 `SyntaxError` and zod 4.4.3 `ZodError` text |
| Directory receipt | `EISDIR ... read` |
| Write failure | locked directory: `EACCES ... open '<temp>'`, temp file absent, no send |
| Unknown outcome defect | the completed write fails after a delivered send; the receipt stays `pending` and the retry rejects `agent_request_outcome_unknown` |
| Rename failure | `EISDIR ... rename '<temp>' -> '<receipt>'`, temp file removed |
| mkdir failures | `ENOTDIR` (parent is a file), `EEXIST` (directory is a file), `EACCES` naming the first missing parent |
| Path join | `receipts/../norm/./x//` resolves like `path.join` |

`errno_names_and_descriptions_match_node` also compares the error text for
errno -1 through -200 with node's `util.getSystemErrorName` and
`getSystemErrorMessage`.

## Normalization

Only these values are normalized, each covered by a test in
`tests/receipts_differential.rs`:

- The disposable root path becomes `<root>`.
- In `writeFileAtomic` temp names, `.<name>.json.<pid>.<ms>.<uuid>.tmp`, the
  pid, `Date.now()`, and `randomUUID()` parts become `<pid>`, `<ms>`, and
  `<uuid>`.

## Known defect

Pinned behavior, reproduced on purpose: a write failure after a successful
provider send leaves the receipt `pending`, so the delivered message reports
`agent_request_outcome_unknown` on every retry and can never be confirmed.
Covered by the differential and by
`completed_write_failure_leaves_a_delivered_message_unknown`.

## Send-retry wire probe

`scripts/phase3/receipts-retry-probe.mjs` drives the pinned `DaemonClient`
against a daemon: one agent, a send with a fixed `messageId`, the same send
retried on the same and on a second connection (accepted, no new turn), the
same `messageId` with other text (rejected as a key conflict), and two
concurrent sends of a fresh `messageId`, one per socket (one turn). The stub
script must hold exactly three turns: the initial prompt, the first send, and
the concurrent pair.

Lane `p3_slice_harness` owns the probe and the `g4-retry` fixture in
`gates.rs` and `gate.sh`. At its commit `a741982d` (probe commits `32d31810`,
`8d0ca287`, `39d346bc`, `b447d3e9`) the probe prints the outcomes line, then
three labelled blocks, `# recording client`, `# retry-other connection` and
`# race second connection` (the race's second socket), one per connection,
holding every frame in arrival order from connect. Each block starts with its
handshake's `server_info` frame (recording is keyed by client instance from
the first payload, since `DaemonClient.connect()` resolves inside the
`server_info` handler). At that commit only frames exactly equal to the bare
heartbeat pong `{"type":"pong"}` are left out, under the class
`client-heartbeat-pong` (their count follows the client's 10 s timer); any
other pong text stays. The coordinator has since ruled that exclusion into a
harness transform, so the probe's next commit prints every frame again, bare
pongs included, and the gate strips exactly-equal lines before comparing,
recording the removed count per side in its verdict. The runner below accepts
either output.

`scripts/phase3/receipts-retry-parity.sh` is the runner for the parity run.
It runs `scripts/phase3/gate.sh g4-retry` and then re-checks the gate's
evidence on its own: both verdicts clean, the sides are the original and
spocky daemons, each probe step exited 0, the stub recorded exactly three
turns (a retry that started a turn makes four), both sides' probe outcomes
equal the expected ones in order, and both sides hold two completed send
receipts with the same masked content. The probe's stdout must be the outcomes
line and then the three blocks in that order, each starting with exactly one
`server_info` frame; the two sides' frames must be byte-identical in key order
after masking generated values (as `g2-differential.sh` does), with no
exemption. Every comparison is on raw bytes; `jq` only answers yes or no and
never writes back a re-encoded value. Bare pong lines are ignored wherever they
sit; the gate compares every other frame. The runner requires a clean tree,
unsets `SPOCKY_ALLOW_SKIP`, and exits nonzero on any failure.
`scripts/phase3/receipts-retry-parity.test.sh` proves that with a fake gate
over 42 cases (a clean match with bare pongs in every block, extra pongs, and
one injected defect each, including a missing, reordered or extra block and a
`workspaceLabels` difference). Removing any one of the runner's checks makes
its own case fail, except the first-frame check, which the frame comparison
shadows; it is kept for its clearer message.

An earlier version of this runner removed `features.workspaceLabels` from the
compared `server_info` frames. That mask was not approved and it hid a real
gap, so it was removed (open gap DWLABEL-001; the Rust fix belongs to
`p4_daemon_services`).

## The runner against the harness's real evidence

The harness ran `g4-retry` on 2026-10-03 and produced
`g4-retry-20261003T200235Z`: self-check PASS (133 rules), parity FAIL with one
difference. The runner was run against that real directory (a stub gate printed
its path and exited 1; nothing else was faked). It read the real layout end to
end, so the fake gate's layout matches the real one in every path the runner
reads, and its only failures were the gate's exit status, the parity verdict,
and the `server_info` comparison in all three blocks. In each block the
difference is exactly `features.workspaceLabels:true`, present in the original
daemon's frame and absent from `spocky-daemon`'s (`crates/spocky-daemon/src/
server_info.rs`). The Rust daemon therefore does not yet match the pinned
daemon's retry wire, and no parity is claimed.

## Gaps

- Platforms: macOS x64 only. Linux was not run; its `EREMOTEIO` and
  `EUNATCH` table entries are unverified. Windows compiles the crate only:
  path handling is `path.posix`, and every Windows OS error reports as an
  unknown system error, unlike node's `path.win32` and libuv mapping.
- `localeCompare` is the ASCII-only port in `spocky_store::collate`. A
  request with non-ASCII object keys can sort, and so digest, differently.
  The pinned caller's request is `{ prompt, activeTurnBehavior }`; `prompt`
  is a string or blocks from closed zod object schemas, so its keys are
  ASCII. Recheck before another caller is ported.
- Queues are per instance in both: two instances on one directory can
  deliver one message twice (pinned behavior, covered above).
- A send starts when `send` is called, as a promise does: it joins the key's
  queue and is spawned on the current Tokio runtime, so `send` needs one at
  call time and its delivery must be `Send + 'static`. The returned future
  can be awaited in any order and dropped without cancelling the send; a panic
  in a send whose future was dropped is lost.
- A request nested deep enough to overflow V8's stack makes node throw a
  `RangeError`; the port digests it.
- Not exercised by the differential: `write` syscall failures (`ENOSPC`,
  `EIO`), a failing temp-file cleanup, and more than two concurrent sends of
  one key.
- Node's `rm` fallbacks after `EPERM` or `EISDIR` on cleanup, its endless
  retry when a missing directory has no parent, and its rejection of a path
  with a NUL byte are not reproduced (the last reports `EINVAL`).
- The request is a JSON value; JavaScript-only values (`undefined` at the top
  level, `BigInt`, `toJSON`) cannot be passed.
