# Phase 3 gate G1 evidence

Lane `p3_slice_harness`, 2026-10-01. Status: harness self-check passes;
G1 parity (original against Spocky) is not claimed and is blocked on the
`spocky-daemon` binary.

## Command

```sh
gtimeout --kill-after=30 1800 scripts/phase3/gate.sh g1
```

Exit 1. The original-against-original self-check passes, then the parity half
stops with `error: no bin target named spocky-daemon in spocky-daemon package`
and prints `g1 parity blocked`. A gate run never passes while either half is
missing.

Self-check only:

```sh
gtimeout --kill-after=30 1800 scripts/phase3/gate.sh g1 --self-check-only
```

Exit 0. It passed on four consecutive runs: `g1-20261001T145527Z`,
`g1-20261001T151044Z`, `g1-20261001T151654Z`, and `g1-20261001T152401Z`. The
first two passing runs differ only in the original install mode (see Runtime).

## Recorded run `g1-20261001T152401Z`

Raw evidence lives under `evidence/raw/phase3/g1-20261001T152401Z/` (untracked).

| File | SHA-256 |
|---|---|
| `self-check/verdict.json` | `84b6410fedd0c065bd8c2eb4481e0f7a48bb2eb044c44d738432d034e348a72a` |
| `self-check/manifest.json` | `94e033a85e30945853e76daa5ac17638cdbe69cb122c0488dfe69080401fc75d` |
| `self-check/rules.json` | `8591019baa8591e82f86181d59adb2a292156420b94a7de7c9e66382174c6d56` |

Verdict: `pass: true`, zero differences, zero check failures, zero survivors,
zero harness errors, 6 of 6 fixtures (readiness probe plus five steps), 5 of 5
positive checks, 78 exact-value normalization rules.

## Runtime

| Item | Value |
|---|---|
| Client and original daemon | `.baselines/paseo-runtime` at `5de45e208690b0efc51c59a585ae9729325a9204`, `git archive` into `/private/tmp/spocky-targets/p3_slice_harness/paseo-original-<commit>`, `package-lock.json` SHA-256 `844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6` |
| Install | `npm ci --no-audit --no-fund` with lifecycle scripts, then `npm run build:server`. A `node-pty` spawn smoke check printed `pty-ok`, exit 0. |
| Node | 22.20.0 via nvm, tarball SHA-256 `2a291f0a9555f5d6685d96ce9429da3d0ea3cd896c012702c6ee0015f818684e` (nvm verified it against SHASUMS256), binary SHA-256 `1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931` |
| Codex | `codex-cli 0.159.0`, SHA-256 `1ad71e5ed117114f9d04cdd8d5dd411515b5ab7ebc725b8ca2f484695d71c838` |

## Scenario

Each side gets a fresh disposable root `/private/tmp/spocky-p3-g1-<11 hex>`
holding `home`, `paseo-home`, `codex-home`, `project` (git, fixed author and
2020-01-02 date), `bin/codex` (symlink to the pinned binary), `tmp`, and
`stub`. Every process runs with a cleared environment: `PATH`, `HOME`,
`USERPROFILE`, `PASEO_HOME`, `CODEX_HOME`, `TMPDIR`, `TZ=UTC`. The daemon runs
in tmux session `spocky-p3-g1-<side>-<epoch ms>`. Launch facts (session, root,
ports, log path, argv, environment) are written to `launch.json` before launch
and the PID to `pid.json` right after.

Steps, all through the pinned CLI with `--host 127.0.0.1:<port> --json`:
readiness `ls` (at most 60 s), `workspace create --isolation local --path
<project>`, `run --provider codex --mode full-access --workspace <wks>
"Reply with the single word READY."`, `logs <agent>`, `ls -a`,
`inspect <agent>`. Stop: SIGTERM to the daemon PID, 30 s grace for the whole
recorded process tree, exact `tmux kill-session -t =<name>`, SIGKILL by PID for
any survivor, 5 s, then any remaining PID fails the gate.

Positive checks per side: every step exits 0, `run` reports
`status: completed`, `logs` contains `READY`, the stub served exactly its one
scripted reply with no unscripted request, and the daemon exits 0.

## Setup facts found while building the harness

- Codex 0.159.0 ignores `OPENAI_BASE_URL` and `OPENAI_API_KEY` from the
  environment for its built-in provider. A probe with only those variables
  tried `wss://api.openai.com/v1/responses` and got 401 (no key was sent).
  Both sides therefore write `codex-home/config.toml` with a `spocky-stub`
  model provider (`base_url` the stub, `env_key = "OPENAI_API_KEY"`,
  `supports_websockets = false`, zero retries), `[analytics] enabled = false`,
  and `[features] plugins = false`. The daemon config still passes
  `CODEX_HOME`, `OPENAI_BASE_URL`, and `OPENAI_API_KEY=test-key` as provider
  `env`, per the plan.
- Without `[features] plugins = false`, codex app-server cloned a remote
  plugin repository into `codex-home/.tmp/plugins`.
- Without `features.dictation.enabled = false` and
  `features.voiceMode.enabled = false` in `config.json`, the original daemon
  downloaded about 700 MB of local speech models into `paseo-home/models`.
- `config.json` sets `daemon.relay.enabled = false`; a missing field enables
  the relay.

## Normalization

Rules are exact values only, applied by `spocky-differential`. Every token
names its class. Discovery fails the gate on any count, shape, format, or
presence difference between the sides.

| Class | Values |
|---|---|
| `daemon-public-key`, `daemon-secret-key`, `local-credential` | allowlisted secrets read from `daemon-keypair.json` and `local-credential` |
| `disposable-root`, `disposable-root-tmp-alias`, `disposable-root-slug` | the side's root, its `/tmp` alias, and Paseo's per-cwd slug of it |
| `daemon-listen`, `stub-listen` | `127.0.0.1:<port>` |
| `sha256-of-workspace-create-request`, `sha256-of-agent-create-request` | creation fingerprints, verified as SHA-256 of the exact request the CLI sent, keys sorted as `creation/index.ts` sorts them |
| `sha256-<kind>-of-<id class>` | `creations/` file names, verified as SHA-256 of `[kind, id]` |
| `generated-id-<shape>-<n>` | shapes `uuid`, `codex-tools-id` (`at_`), `codex-message-id` (`msg_`), `workspace-id`, `project-id`, `client-id`, `server-id` |
| `short7-of-<id class>` | the CLI's `agent.id.slice(0, 7)` |
| `wall-clock-<format>` | `iso-frac3`, `epoch-ms`, and other formats, only inside the run window |

No other 64-hex value is normalized.

## Deviations for lead decision

1. `client_metadata` key order. Two original runs of the same pinned codex
   binary put the keys of the Responses request body's `client_metadata` in
   different orders (hash map serialization, run `g1-20261001T145047Z`). No
   daemon controls this order. The harness reorders only that one object's
   keys, only when its exact serialization occurs once in the body; every
   other byte stays exact and the raw record is retained. Tests prove that
   top-level order changes and nested-object order changes still fail. This is
   the only reordering in the harness.
2. Normalized classes beyond the plan's list (`wks_`, agent id, Codex thread
   id, `serverId`): project id, client id, Codex turn, window, installation,
   tools, and message ids, the daemon keypair, verified creation digests and
   fingerprints, and the CLI short id. Each has a test.
3. State files are enumerated in a canonical order (masked path, masked
   content, raw bytes) because directory listing order is not defined.

## Not compared (retained raw)

- `paseo-home/daemon.log*` and the daemon's tmux stdout and stderr
  (`daemon.out`): pino lines carry PIDs, metrics, and timings. The daemon exit
  status and forced-kill count are compared.
- `codex-home` contents other than `config.toml`: codex internal SQLite stores
  and rollout files. A listing is retained. What the daemon sends codex is
  compared through the stub request records (headers and bodies).
- Readiness attempt count.

## Remaining gaps

- G1 parity is blocked: crate `spocky-daemon` has no `spocky-daemon` bin
  target on main `d6618ba`. The harness launches it with no arguments, the
  same environment, and the same `config.json`; the daemon lane owns that
  interface.
- G2 to G4 are not defined; `gate.sh` rejects them.
- `Cargo.lock` needs four dependency lines for `spocky-slice-harness`
  (`serde`, `serde_json`, `sha2 0.10.9`, `spocky-differential`). The lane did
  not commit the lock; the lead owns it.
