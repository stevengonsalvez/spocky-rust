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

Test counts: 4 unit, 5 ported (`tests/receipts.rs`, including call-order
queueing when futures are polled out of order), 6 differential
(`tests/receipts_differential.rs`; the 2 old-home cross-read tests were added
by lane `p3_contracts`), 0 doc tests. 15 passed, 0 failed, 0 ignored. Clippy and fmt are clean. The differential first fails unless node
reports `v22.20.0` and both dist modules match the digests below.

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

## Recorded run `receipts-20261003T011633Z`

Raw evidence lives under `evidence/raw/phase3/receipts-20261003T011633Z/`
(untracked).

| Input | Value |
|---|---|
| Commit | `0ed049f53cb8a326853f3c9235af18bd282f4118` |
| Node | `v22.20.0` |
| Pinned dist | `paseo-original-5de45e208690b0efc51c59a585ae9729325a9204/packages/server/dist/server` |
| `server/message-receipts/index.js` SHA-256 | `e99ca1a266f038efbceaf398b45ccb2e904a58ca46e4422546dc22ea498c4559` |
| `server/atomic-file.js` SHA-256 | `835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25` |

| File | SHA-256 |
|---|---|
| `receipts-node-normalized.json` | `bd7d6e5734284c4bb8ddf82b0ded8c8beaa27c638078143eab06e2fadc738c63` |
| `receipts-rust-normalized.json` | `bd7d6e5734284c4bb8ddf82b0ded8c8beaa27c638078143eab06e2fadc738c63` |
| `receipts-node-raw.json` | `c7f3167d5c1e816740674c05ff3164873dc430014c60371b5dd04f85126b36d4` |
| `receipts-rust-raw.json` | `b949c1c7a2bce048d39013e765d3910bdafb277eaee63ffeacd266f6b1ae1677` |
| `inputs.txt` | `7a629ef550adbbbdac8435f7e0c32ee7f093a5e956c82e2fa562d72bd703f0b0` |
| `test.log` | `13fe56990ce5af377744f934a2d5616ad8f32de04274c4f2e1058e1471a6d9c7` |
| `clippy.log` | `6a81f119d29a3ac9f57fb3eafe4abb242e49574cb892c525ca92bee3dc842a9c` |
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
concurrent sends of a fresh `messageId` (one turn). It prints each step's
outcome and the recording client's raw wire text. The stub script must hold
exactly three turns: the initial prompt, the first send, and the concurrent
pair. `gates.rs` and `gate.sh` belong to `p3_slice_harness`, which owns the
`g4-retry` fixture. Run on the pinned original daemon through a scratch copy
of that fixture, the probe's outcomes were all as expected and the stub held
exactly three turns; that gate's compare then stopped on a 5 ms
`updatedAt` and `attentionTimestamp` gap that the harness now masks under
`wall_clock`. No run has compared the original and `spocky-daemon` yet, so
wire parity of the daemon's retry handling is not claimed here.

`scripts/phase3/receipts-retry-parity.sh` is the runner for that parity run.
It runs `scripts/phase3/gate.sh g4-retry` and then re-checks the gate's
evidence on its own: both verdicts clean, the sides are the original and
spocky daemons, each probe step exited 0, the stub recorded exactly three
turns (a retry that started a turn makes four), both sides' probe outcomes
equal the expected ones in order, and both sides hold two completed send
receipts with equal fingerprints. It requires a clean tree, unsets
`SPOCKY_ALLOW_SKIP`, and exits nonzero on any failure.
`scripts/phase3/receipts-retry-parity.test.sh` proves that with a fake gate
over 24 cases (a clean match, and one injected defect each); removing any one
of the runner's checks makes its own case fail. The runner itself has not run
against the real gate, which does not support `g4-retry` on main yet.

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
