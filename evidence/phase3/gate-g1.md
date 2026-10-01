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

Exit 0. Recorded passing runs: `g1-20261001T164755Z` and
`g1-20261001T165528Z` (both with loopback-only egress, codex input capture,
and project and tmp capture). Earlier passing runs before those captures:
`g1-20261001T145527Z`, `g1-20261001T151044Z`, `g1-20261001T151654Z`,
`g1-20261001T152401Z`.

## Recorded run `g1-20261001T165528Z`

Raw evidence lives under `evidence/raw/phase3/g1-20261001T165528Z/` (untracked).

| File | SHA-256 |
|---|---|
| `self-check/verdict.json` | `54ddcda2bd240f4ad92c25d57e3871722a893ce6bc7ebbf6187764ed1dec3a8f` |
| `self-check/manifest.json` | `0e5ea36bdccf24e645fbe7ed4f96a6127d232ed83276d75a7860145e59c1922e` |
| `self-check/rules.json` | `850df80585a973425689a939110e2495b2aeb9163e162817c2e31b6943be3c16` |

Verdict: `pass: true`, zero differences, zero check failures, zero survivors,
zero harness errors (so zero egress denials), 6 of 6 fixtures (readiness probe
plus five steps), 5 of 5 positive checks, 78 exact-value normalization rules,
and the one named transform.

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
3. Detection. The harness records every PID of a side: the daemon process
   tree and a root-path scan (`ps -A -E -ww`) after readiness, after every
   step, and before stop; each CLI step PID; and each codex invocation PID,
   which the wrapper writes before `exec`. After stop it reads the kernel log
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
| `wall-clock-<format>` | `iso-frac3`, `epoch-ms`, and other formats, only inside the run window |

No other 64-hex value is normalized. UUIDs match only versions 4, 5, and 7
with the RFC 9562 variant, so constants such as the nil and max UUIDs stay
literal. A class whose values are identical on both sides emits no rule.
Rules apply only to texts both sides have; a one-sided text is a plain
difference.

Wall-clock instants share one class per format and are not numbered. The lead
asked for numbering by first appearance; two original-against-original runs
then failed at discovery only (`g1-20261001T161513Z`: `iso-frac3` 8 against 6;
`g1-20261001T161957Z`: 6 against 7), because instants that share a
millisecond differ between identical runs. A format present on one side only
still fails.

## Named transform `codex-client-metadata-key-order`

The only non-normalization transform. It sorts the keys of the
`client_metadata` object inside stub-recorded codex request bodies and nothing
else. The verdict, `rules.json` (`transforms`), and `manifest.json`
(`transforms`) name it with target, reason, owner, where raw bytes are kept,
and which records it changed.

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
4. For lead decision: wall-clock instants are not numbered (see
   Normalization), with the failing runs as evidence.
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
