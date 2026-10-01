# Phase 3 gate G1 evidence

Lane `p3_slice_harness`, 2026-10-01. Status: harness self-check passes under
loopback-only egress; G1 parity (original against Spocky) is not claimed and
is blocked on the `spocky-daemon` binary.

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

Exit 0. Recorded passing run with every current safeguard (dedicated tmux
socket, owned-only stop, loopback-only egress, codex input capture, project
and tmp capture, position-paired wall-clock instants):
`g1-20261001T180907Z`. Earlier passing runs with fewer safeguards:
`g1-20261001T164755Z`, `g1-20261001T165528Z`, `g1-20261001T145527Z`,
`g1-20261001T151044Z`, `g1-20261001T151654Z`, `g1-20261001T152401Z`.

## Recorded run `g1-20261001T180907Z`

Raw evidence lives under `evidence/raw/phase3/g1-20261001T180907Z/` (untracked).

| File | SHA-256 |
|---|---|
| `self-check/verdict.json` | `19f6a41a9d55e555f57678fdbafab2f5e8d50412a4964479e70f062489d08523` |
| `self-check/manifest.json` | `d47f30fa99f38d977d8f68d3b1f2776aa2b13a2dd4acb1d86ebac1c4b6eb393b` |
| `self-check/rules.json` | `1ca55e430aaf0458f425e89fffdcd37235fb954388b3e1214f7a4da5a38f819e` |
| `self-check/transforms.json` | `f0344493280ff2a50767289657d266afe6e35454d0b3d5402eb60b6d17a168f9` |

Verdict: `pass: true`, zero differences, zero check failures, zero survivors,
zero harness errors (so zero egress denials and zero unowned processes
mentioning the root), 6 of 6 fixtures (readiness probe plus five steps), 5 of
5 positive checks, 82 exact-value normalization rules, and the one named
transform. Both daemons exited 0 after SIGTERM; nothing needed SIGKILL; 43 and
42 PIDs were tracked on the two sides.

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
2020-01-02 date), `bin/codex` (recording wrapper around the pinned binary),
`codex-io`, `tmp`, and `stub`. Every process runs with a cleared environment: `PATH`, `HOME`,
`USERPROFILE`, `PASEO_HOME`, `CODEX_HOME`, `TMPDIR`, `TZ=UTC`. The daemon runs
in tmux session `spocky-p3-g1-<side>-<epoch ms>` on a dedicated per-run tmux
socket (see Process safety). Launch facts (session, root,
ports, log path, argv, environment) are written to `launch.json` before launch
and the PID to `pid.json` right after.

Steps, all through the pinned CLI with `--host 127.0.0.1:<port> --json`:
readiness `ls` (at most 60 s), `workspace create --isolation local --path
<project>`, `run --provider codex --mode full-access --workspace <wks>
"Reply with the single word READY."`, `logs <agent>`, `ls -a`,
`inspect <agent>`. Stop: SIGTERM to the daemon PID only if it is an owned live
process, 30 s grace for every owned process, exact `tmux kill-session -t
=<name>` on the dedicated socket, SIGKILL for any owned process still alive,
5 s, then any remaining owned PID fails the gate.

## Process safety

- Every tmux call goes through one function that adds `-L
  spocky-p3-gate-<harness pid> -f /dev/null`. The gate never talks to the
  default tmux server and never calls `kill-server`; sessions are addressed
  only as `=<exact name>`.
- Ownership: a sampler reads `ps -A -o pid=,ppid=,stat=,lstart=,comm=`
  (`LC_ALL=C`) every 100 ms from before the tmux launch until stop. The roots
  are the tmux pane PID and each CLI step PID. A process is owned when its
  parent was owned. Identity is (PID, start time), which survives the `env`,
  `sandbox-exec`, and codex wrapper `exec` hand-offs and changes on PID reuse.
  A process whose command name is `tmux` is never owned, even inside the tree.
  The harness and the stub are never owned.
- Signals go only to owned, live, non-zombie processes with the recorded
  start time. The daemon PID read from `daemon.pid` gets SIGTERM only if it
  passes the same check and `daemon.exit` does not exist yet.
- A root-path scan (`ps -A -E -ww`) only reports: any process that mentions
  the disposable root but is not owned (and is not tmux or the stub) becomes a
  harness error and is never signalled.
- `run_bounded` kills a held-pipe process group only while that group still
  has members other than its reaped leader, so a reused group ID is never
  signalled.
- Tests with live processes: an unrelated decoy whose environment mentions the
  root, an unrelated decoy whose argv names a path under the root (confirmed
  in `ps -o command=`), and a program named `tmux` inside the owned tree all
  survive the sweep while the owned process dies; the argv decoy runs in its
  own process group and its cleanup asserts no leaked grandchild; a decoy PID
  written into the daemon pid file is refused; zombies and reused PIDs are
  never targets.

Positive checks per side: every step exits 0, `run` reports
`status: completed`, `logs` has a line exactly `READY` (the prompt echo alone
does not satisfy it), the stub served exactly its one scripted reply with no
unscripted request, and the daemon exits 0. The per-step stub request count is
a compared artifact, so request counts must also match step by step.

## Egress: loopback only, enforced and detected

Codex 0.159.0's built-in provider ignores `OPENAI_BASE_URL`, and app-server
calls such as `initialize`, `config/read`, `model/list`, and `skills/list` use
the configured provider. Three layers keep every request local:

1. Routing. Both sides write the same `codex-home/config.toml`:
   `model_provider = "spocky-stub"` with `base_url` the loopback stub,
   `env_key = "OPENAI_API_KEY"`, `wire_api = "responses"`,
   `supports_websockets = false`, zero retries, `[analytics] enabled = false`,
   and `[features] plugins = false`. The built-in provider is never selected.
2. Enforcement. The daemon (`/usr/bin/sandbox-exec -p <profile> ...` inside
   the tmux launch script) and every CLI step run under the seatbelt profile
   `side::EGRESS_PROFILE`: `(allow default)`, `(deny network-outbound (remote
   ip "*:*"))`, `(allow network-outbound (remote ip "localhost:*"))`. Every
   descendant inherits it, including the codex app-server. A non-loopback
   connect fails with `EPERM`.
3. Detection. The harness checks the PIDs it tracked: every owned process the
   100 ms sampler saw (see Process safety), each CLI step PID, and each codex
   invocation PID, which the wrapper writes before `exec`. A descendant that
   lives under 100 ms and is never sampled is still blocked by the sandbox but
   may go undetected. After stop it reads the kernel log
   (`log show --last <window> --predicate 'eventMessage CONTAINS "deny" AND
   eventMessage CONTAINS "network-outbound"'`) and adds a harness error for
   any `Sandbox: <name>(<pid>) deny(1) network-outbound` line from those PIDs.
   Any harness error fails the gate.

Test `non_loopback_connect_is_blocked_and_detected_but_loopback_is_not`
connects to `192.0.2.1:443` under the profile (blocked and detected) and to a
loopback listener (allowed, no violation).

The `run` step passes no `--model`, as the plan's G1 argv specifies. The
original daemon chooses the model from codex `model/list` and sends it
explicitly: the captured `turn/start` carries `"model":"gpt-6-astra"`. The
Spocky daemon must do the same, and that input is compared (next section).

## Codex app-server input, byte for byte

`bin/codex` on `PATH` is a wrapper. Per invocation it writes argv and its PID,
then `tee`s the daemon's stdin into `codex-io/<n>/stdin` through a FIFO and
`exec`s the pinned binary as the same PID. Each invocation becomes one
compared state record, `argv` plus the exact stdin bytes (the JSON-RPC lines:
`initialize`, `config/read`, `model/list`, `thread/start`, `turn/start`, and
others). Only exact-value normalization applies (root path, generated ids);
key order and content are compared byte for byte. The arrival number is not
compared, because concurrent `--version` probes race for it. In the recorded
run the original daemon invoked codex 14 times (11 `--version`, 3
`app-server`).

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
| `wall-clock-<format>-<n>` | `iso-frac3`, `epoch-ms`, and other formats, only inside the run window, paired by occurrence position |

No other 64-hex value is normalized. UUIDs match only versions 4, 5, and 7
with the RFC 9562 variant, so constants such as the nil and max UUIDs stay
literal. A class whose values are identical on both sides emits no rule.
Rules apply only to texts both sides have; a one-sided text is a plain
difference.

Wall-clock instants are paired by occurrence position within each format,
and both sides must have the same number of occurrences. Literals connected by
pairing form one class `wall-clock-<format>-<n>`. A class may hold several
distinct literals on one side only when they lie within the format's
resolution (1 ms for `iso-frac3` and `epoch-ms`, 1 s for `epoch-s` and
`iso-frac0`), which tolerates two events that print the same millisecond on
one side but not the other. Any other equality-structure difference (for
example `createdAt == updatedAt` on one side and 5 ms apart on the other)
fails discovery. Earlier, numbering distinct values by first appearance made
identical daemons fail (`g1-20261001T161513Z`: `iso-frac3` 8 against 6;
`g1-20261001T161957Z`: 6 against 7); pairing by occurrence removes that, and
`g1-20261001T180907Z` passed with six paired `iso-frac3` groups.

## Named transform `codex-client-metadata-key-order`

The only non-normalization transform. It sorts the keys of the
`client_metadata` object inside stub-recorded codex request bodies and nothing
else. The verdict and `transforms.json` (beside an unchanged, replayable
`manifest.json`) name it with target, reason, owner, where raw bytes are kept,
and which records (`left:stub/000`, `right:stub/000`) it changed.

Producer: the pinned codex binary, not the CLI or either daemon. Four direct
`codex exec` runs with no Paseo process, against the same stub, produced four
key orders (raw records under `evidence/raw/phase3/codex-client-metadata-order/`):

| Record | SHA-256 | Key order |
|---|---|---|
| `run1-record.jsonl` | `121c9504ad3367cdf16131aef99eafc5dee3006b715ed539bf2001901c42b3ba` | turn-metadata, installation-id, window-id, thread_id, session_id, turn_id, root_turn_id |
| `run2-record.jsonl` | `15c1b4ee7cc0e4193931c07cdcc62756ea15cc448ec6ef04231fce7356919eeb` | turn-metadata, installation-id, turn_id, session_id, thread_id, window-id, root_turn_id |
| `run3-record.jsonl` | `e8f40e22e560ff342653fd2229b374d1441d83df4b9d2646bea2a6495b0fea4d` | turn_id, turn-metadata, installation-id, thread_id, root_turn_id, session_id, window-id |
| `run4-record.jsonl` | `e69d84bdb0741d686bb8ecf1e8ac9d95b0143ee6128b52be596cd19cc392f0e8` | installation-id, turn_id, turn-metadata, session_id, root_turn_id, thread_id, window-id |

In self-check run `g1-20261001T145047Z` (both sides the original daemon) the
differing order appeared in the `run` step's single stub request. Every
daemon-controlled input to codex is compared unchanged through `codex-io`.
Tests prove a key-order swap anywhere else in the same body still fails.

## Deviations

1. Accepted by the lead (slice-plan section 6) and approved by the
   coordinator: the `client_metadata` transform above.
2. Normalized classes beyond the plan's list (`wks_`, agent id, Codex thread
   id, `serverId`): project id, client id, Codex turn, window, installation,
   tools, and message ids, the daemon keypair, verified creation digests and
   fingerprints, and the CLI short id. Each has a test.
3. State files are enumerated in a canonical order (masked path, masked
   content, raw bytes) because directory listing order is not defined.
4. Wall-clock pairing tolerates merges within one resolution unit (see
   Normalization); a structure difference beyond that fails.
5. Observed once, not normalized: in `g1-20261001T161153Z` one side's codex
   `x-codex-turn-metadata` lacked the `workspaces` git entry the other side
   had (codex's own git probe, under machine load near 80). The gate failed
   as it should. The next comparing run, `g1-20261001T164755Z`, passed
   without it.

## Not compared (retained raw)

- `paseo-home/daemon.log*` and the daemon's tmux stdout and stderr
  (`daemon.out`): pino lines carry PIDs, metrics, and timings. The daemon exit
  status and forced-kill count are compared.
- `codex-home` contents other than `config.toml`: codex internal SQLite stores
  and rollout files. A listing is retained. What the daemon sends codex is
  compared byte for byte through `codex-io`, and what codex sends the model
  through the stub request records (headers and bodies).
- Readiness attempt count.

## Remaining gaps

- G1 parity is blocked: crate `spocky-daemon` has no `spocky-daemon` bin
  target on main `d6618ba`. The harness launches it with no arguments, the
  same environment, and the same `config.json`; the daemon lane owns that
  interface.
- G2 to G4 are not defined; `gate.sh` rejects them.
- The outer seatbelt sandbox blocks nested sandboxing. G1 runs codex with
  `danger-full-access`, so codex applies no sandbox of its own; G2 (`auto`
  mode) needs another egress mechanism if codex must sandbox commands.
