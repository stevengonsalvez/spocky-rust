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
`SPOCKY_ALLOW_SKIP` unset. It exits nonzero unless every command passes and
both normalized differential outputs exist and are byte-identical:

```sh
cargo test --locked -p spocky-message-receipts
cargo clippy --locked -p spocky-message-receipts --all-targets -- -D warnings
cargo fmt --package spocky-message-receipts -- --check
```

Test counts: 4 unit, 4 ported (`tests/receipts.rs`), 4 differential
(`tests/receipts_differential.rs`), 0 doc tests. 12 passed, 0 failed, 0
ignored. Clippy and fmt are clean. The differential first fails unless node
reports `v22.20.0` and both dist modules match the digests below.

The runner's own failure handling is proven by:

```sh
gtimeout --kill-after=30 1800 scripts/phase3/receipts-differential.test.sh
```

Exit 0, 6 cases: a stubbed match exits 0 even with `SPOCKY_ALLOW_SKIP=1`
exported; mismatched outputs, missing outputs, a failing test with matching
outputs, and failing clippy exit nonzero; a real run against a dist with a
tampered `index.js` exits nonzero on the pinned digest. Removing the output
comparison, the test exit check, or the `SPOCKY_ALLOW_SKIP` unset from the
runner each made one case fail.

## Recorded run `receipts-20261001T190736Z`

Raw evidence lives under `evidence/raw/phase3/receipts-20261001T190736Z/`
(untracked).

| Input | Value |
|---|---|
| Commit | `1a19cb6030da510863d9e42fba1b915aa392d68b` |
| Node | `v22.20.0` |
| Pinned dist | `paseo-original-5de45e208690b0efc51c59a585ae9729325a9204/packages/server/dist/server` |
| `server/message-receipts/index.js` SHA-256 | `e99ca1a266f038efbceaf398b45ccb2e904a58ca46e4422546dc22ea498c4559` |
| `server/atomic-file.js` SHA-256 | `835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25` |

| File | SHA-256 |
|---|---|
| `receipts-node-normalized.json` | `bd7d6e5734284c4bb8ddf82b0ded8c8beaa27c638078143eab06e2fadc738c63` |
| `receipts-rust-normalized.json` | `bd7d6e5734284c4bb8ddf82b0ded8c8beaa27c638078143eab06e2fadc738c63` |
| `receipts-node-raw.json` | `c4c9619cd4ee8f517463790d2eb6a4916e6f2a4259d858d30ca0235ad04a60c8` |
| `receipts-rust-raw.json` | `57dea0aea4d4d47ada4755836cd6c889006c19c43567d818166f1bb3cbd33565` |
| `inputs.txt` | `30e893dc75bf33075d8716568e27af9cf35c7b16a96c55b41c0339a21a990e12` |
| `test.log` | `47834232159872c7a595afdf02765bf0dbefac29cc15097a1144838d34d761c0` |
| `clippy.log` | `abfeb40db11fb4a1dbe03bd1de171883b7940977ebcb78bb5039a74119c8bc9a` |
| `fmt.log` (empty) | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |

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
| Two instances, one directory | concurrent sends of a fresh id from two instances both prepare and deliver, as in the pinned build |
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
- Queue position: the baseline queues a send when `send` is called; the port
  queues it when the future is first polled.
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
