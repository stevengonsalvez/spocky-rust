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
`scripts/phase3/pins.sh`) and `SPOCKY_PASEO_DIST` set, and without
`SPOCKY_ALLOW_SKIP`:

```sh
cargo test --locked -p spocky-message-receipts
cargo clippy --locked -p spocky-message-receipts --all-targets -- -D warnings
cargo fmt --package spocky-message-receipts -- --check
```

Test counts: 4 unit, 4 ported (`tests/receipts.rs`), 4 differential
(`tests/receipts_differential.rs`), 0 doc tests. 12 passed, 0 failed, 0
ignored. Clippy and fmt are clean.

## Recorded run `receipts-20261001T183820Z`

Raw evidence lives under `evidence/raw/phase3/receipts-20261001T183820Z/`
(untracked).

| Input | Value |
|---|---|
| Commit | `954e764db44af65903603cddeae2602dc4e6385d` |
| Node | `v22.20.0` |
| Pinned dist | `paseo-original-5de45e208690b0efc51c59a585ae9729325a9204/packages/server/dist/server` |
| `server/message-receipts/index.js` SHA-256 | `e99ca1a266f038efbceaf398b45ccb2e904a58ca46e4422546dc22ea498c4559` |
| `server/atomic-file.js` SHA-256 | `835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25` |

| File | SHA-256 |
|---|---|
| `receipts-node-normalized.json` | `31957162939969a8a6ff6733cd5e384ea47b2695e5bc4620d6bbac92e8832899` |
| `receipts-rust-normalized.json` | `31957162939969a8a6ff6733cd5e384ea47b2695e5bc4620d6bbac92e8832899` |
| `receipts-node-raw.json` | `b1af006e88eaa1b9d038095a5686370cf903fe6ac604ccb46b1141d533e2630e` |
| `receipts-rust-raw.json` | `c29078ffc926317e36b4cd9e6b389e2d1d1353ba2a7a3cb6e837cdd0836ceef9` |
| `inputs.txt` | `720227e3a77fd7fd8611f791e4da333e752f5020685215d755e6f08b54045889` |
| `test.log` | `f71c0180a961e9a1edc68fb8bd8c304d9f7225c40d07bba99ac01d24c52b8368` |
| `clippy.log` | `7b89f9d8b45896e66051e0a6a0854f36f5acdc9e450391ac977c70c47d4c66e3` |
| `fmt.log` (empty) | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |

The normalized outputs are byte-identical, and the same digest was produced
by the earlier runs `receipts-20261001T183301Z` and `receipts-20261001T183341Z`. Raw digests differ per run
because temp names carry the pid, clock, and a random UUID.

## What is compared

Node imports the built `server/message-receipts/index.js` and runs 52
scripted steps in one disposable root; the port runs the same steps in
another. After every step both print each send's result (`{ ok: true }`, or
`{ name, message, ...error }` with node's own enumerable error fields), the
send and prepare callback counts, and every file and directory under the
root with its mode, SHA-256, and text. The two JSON texts must be equal.

| Scenario | Steps |
|---|---|
| First delivery | pending then completed receipt, 2-space JSON, key order `fingerprint`, `agentId`, `state` |
| Duplicates | two concurrent sends, a restarted instance, reordered request keys (`localeCompare` and index-key order) |
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
