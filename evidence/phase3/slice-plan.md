# Phase 3 vertical slice plan

Owner: Claude lead (`claude-opus-5-5`), 2026-10-01. Objective: exact
like-for-like Paseo parity for one path, nothing more.

```text
pinned Paseo CLI (unchanged) --ws /ws--> daemon --stdio JSON-RPC--> real codex 0.159.0
                                             |                          |
                                       disposable home           scripted local
                                                                 /v1/responses
```

The same unchanged client and the same real `codex` binary run against the
original daemon and the Spocky daemon. Only the daemon differs.

## 1. Entry manifest (frozen before any P3 integration)

Decision: P3 starts in parallel with remaining P2 gaps on coordinator
authority while Stevie is away (`porting/tasks.json`, P3 revision
2026-10-01). It overrides the plan gate that required P2 done and reviewed.
P2 adversarial reviews remain rejected. Astra is unused in the Claude routing
window, so the P3 two-review boundary cannot close until it is.

Main at freeze: `f3cda7e` (ledger), crate skeletons at `2cfedb1`.

| Core dependency | Integrated commits | Evidence and SHA-256 | Lead rerun on main `eaf95e8` |
|---|---|---|---|
| `p2_harness` (`spocky-differential`) | `dafc711`, `521e06c`, `36ad3d0`, `53c14f1`, `a72e856`, `f8c7995`, `3553934` | `tests/normalization.rs` | `normalization` 13/13 |
| `p2_codec_crypto` (`spocky-wire`, `spocky-crypto`) | `2f66289`, `ac533e2`, `dee5823`, `be80cb2`, `a6cca0c` | `evidence/phase2/baseline-captures.md` `7fe0bc9e59e29c78efa678b6878f27a6ef0921b9563b1a01d1f584e38129c7a5`; `pinned-crypto.json` `3ddc666cfb41731222ced4d60fd4e13a2aa16bc8127abef15814a6134763faf9`; raw `pinned-wire.json` `f7a609aced8350ae634e81176b77dd31297ca3a032bd9c52b090427b26bdfa10` | `binary_frames` 5/5, `baseline_vectors` 6/6 |
| `p2_state_lifecycle` (`spocky-store`) | `9fc7298`, `12d2812`, `0516dec`, `4baeed3` | `evidence/phase2/store-differential.md` `eb9faa4aa19eedd367071a2b1b931a71160b6d65b86faa056a70f63081019103`; `baseline-agent.json` `98cf62b88525fe99cfc21dda5740bbee5e568a4b6c6c0633673d6dbc658029bc` | `baseline_roundtrip` 3/3 |
| `p2_lifecycle_full` (`spocky-domain`) | `1d92b81`, `5661e07`, `a7961f7`, `8615ccd`, `8743950` | `evidence/phase2/lifecycle-differential.md` `3d9a859d08c55c9fa9deab8ad18036422f06d5fdd380fc0b4d298b99e958370d` | `lifecycle_contract` 8/8, `lifecycle_runtime` 1/1 |
| Brand rename covering all five crates | `4cce774` | `evidence/phase2/spocky-branding.md` | n/a |

Known entry limits (facts):

- The P2 lifecycle proof used Paseo's in-process fake Codex. It excluded the
  daemon, WebSocket, and provider runtime (`lifecycle-differential.md`, Limits).
- `spocky-wire` has binary frames only. No JSON envelopes exist yet.
- `spocky-store` covers agent records only. No workspace registry, config, pid,
  or credential files.
- Review G found a harness that records instead of enforcing its gate. P3 gates
  must fail the run on any mismatch.

Runtime pins:

| Item | Value |
|---|---|
| Paseo client and original daemon | `.baselines/paseo-runtime` at `5de45e208690b0efc51c59a585ae9729325a9204`, built in a disposable `git archive` copy |
| Node | `22.20.0` from Paseo `.tool-versions`. Not installed yet (22.19.0 and 26.7.0 present). Harness lane installs it locally via nvm and records the digest, or blocks. |
| Codex | `codex-cli 0.159.0`, `/usr/local/Caskroom/codex/0.159.0/bin/codex`, SHA-256 `1ad71e5ed117114f9d04cdd8d5dd411515b5ab7ebc725b8ca2f484695d71c838` |
| Rust | toolchain `1.94.0` |

## 2. Crate layout

```text
spocky-contracts ──┬──▶ spocky-provider-codex
 (wire JSON, msgs, │
  provider trait)  ├──▶ spocky-session ◀── spocky-store, spocky-domain
                   │          │
                   └──▶ spocky-daemon (bin spocky-daemon: ws, hello, home files)
spocky-slice-harness (test-only: original vs Spocky driver, responses stub)
```

| Crate | Status | Owns (Paseo source at `5de45e2`) |
|---|---|---|
| `spocky-contracts` | new, skeleton `bc7db87` | `packages/protocol/src/messages.ts` subset below, `AgentSnapshotPayload`, persistence handle, session config, provider boundary trait and timeline item types |
| `spocky-daemon` | new, skeleton `29469ca` | `websocket-server.ts` (`/ws`, Host and Origin admission, hello within 15 s, close 4001, ping/pong, 503 before ready), `session-admission-auth.ts`, `local-credential.ts`, `pid-lock.ts`, `server-id.ts`, `config.ts` listen precedence and env |
| `spocky-session` | new, skeleton `6ac8863` | `session.ts` dispatch and `rpc_error`, owned subscriptions, agent updates, `workspace-registry*.ts`, `creation/index.ts` directory source, `agent-manager.ts`, `agent-loading.ts`, `agent-projections.ts`, `lifecycle-command.ts`, `permission-response.ts`, in-memory timeline store and projection |
| `spocky-provider-codex` | new, skeleton `c91cd12` | `codex-app-server-agent.ts` slice subset, `codex/app-server-transport.ts`, `codex/tool-call-mapper.ts`, `jsonl-frame-decoder.ts` |
| `spocky-store` | existing | add `projects/projects.json`, `projects/workspaces.json`, `config.json` persisted config, atomic file; keep agent records |
| `spocky-domain` | existing | reuse `AgentLifecycleMachine`; extend only where `agent-manager.ts` needs states it lacks |
| `spocky-wire`, `spocky-crypto` | existing | unchanged in this slice; no binary frames or E2EE on this path |
| `spocky-slice-harness` | new, skeleton `ccdf9d8` | harness only, never shipped |

Lead owns root `Cargo.toml` and the crate graph. Writers may add dependencies
to their own crate manifest. Reuse versions already in `Cargo.lock` where one
exists (`serde` 1.0.229, `serde_json` 1.0.145, `tokio` 1.53.1, `tungstenite`
0.27, `axum` 0.8.9, `sha2` 0.10.9, `uuid` 1.26.1). A writer's `Cargo.lock`
change is its own one-file commit; the lead regenerates the lock at integration
if lanes conflict.

## 3. Message subset (all in `packages/protocol/src/messages.ts`)

| Gate | Client sends | Daemon emits |
|---|---|---|
| G1 | `hello`, `ping` | `session` + `status` `server_info`, `hello.rejected`, `pong` |
| G1 | `workspace.create.request` (`source.kind: "directory"`), `fetch_workspaces_request` | `workspace.create.response`, `workspace_update`, `fetch_workspaces_response` |
| G1 | `create_agent_request` or `agent.create.request` and `creation.subscribe.request`, whichever path the pinned CLI takes against the pinned daemon's advertised `features` | `agent_created` / `agent_create_failed` or `agent.create.response` / `agent.create.update` / snapshot |
| G1 | `send_agent_message_request`, `wait_for_finish_request`, `fetch_agents_request`, `fetch_agent_request`, `fetch_agent_timeline_request`, `agent.timeline.set_subscription.request`, `session.events.set_subscription.request`, `subscription.release.request` | matching responses, `agent_update`, `agent_stream`, `rpc_error` |
| G2 | `agent_permission_response`, `cancel_agent_request` | `agent_permission_request`, `agent_permission_resolved`, `cancel_agent_response` |
| G3 | `resume_agent_request`, `refresh_agent_request` | `agent_resumed`, `agent_refreshed` |

The Spocky daemon advertises exactly the `features` the pinned daemon
advertises. It must not change which client path runs. The client validates
every frame with zod: a missing required field drops the frame.

Persistence facts that bind the slice: agents at
`$PASEO_HOME/agents/<cwd-slug>/<agentId>.json` with
`persistence {provider:"codex", sessionId, nativeHandle, metadata}`; workspace
ids `wks_` + 16 hex; timelines are in memory only and rebuild from Codex
`thread/read` after restart.

## 4. Unchanged-client differential harness

Lane `p3_slice_harness` builds it. Same script, two runs: `--daemon original`
and `--daemon spocky`.

1. Verify `.baselines/paseo-runtime` HEAD and clean tree. `git archive` into
   a disposable root and run `npm ci` plus `npm run build:server` there with
   Node 22.20.0. Never write into `.baselines`.
2. Per run, create a disposable root: `HOME`, `USERPROFILE`, `PASEO_HOME`,
   `CODEX_HOME`, and `project/` (git-initialized with a fixed author and date).
   Strip every inherited `PASEO_*` variable.
3. Start the scripted Responses stub (`spocky-slice-harness` bin) on a random
   loopback port. It replays fixed SSE scripts and records each request body.
4. Write `config.json`: `daemon.listen 127.0.0.1:P` (random, refuse 6767 and
   6768), relay disabled, and provider `codex` env `CODEX_HOME`,
   `OPENAI_BASE_URL`, `OPENAI_API_KEY=test-key`. If the pinned daemon rejects
   env on the built-in id, use the pinned `extends: "codex"` profile on both
   sides and record it.
5. Start the daemon in named tmux session `spocky-p3-<gate>-<side>-<epoch>`:
   original `node <archive>/packages/cli/dist/index.js daemon run`; Spocky
   `target/debug/spocky-daemon`. Record session, port, home, PID, and log path
   before launch.
6. Readiness: `paseo --host 127.0.0.1:P ls --json` until exit 0, at most 60 s.
7. Drive the gate's command script with the pinned CLI via
   `paseo --host 127.0.0.1:P --json ...`. Capture stdout, stderr, exit code,
   persisted home files, and stub request bodies per step.
8. Stop the exact tmux session and PID with forced-kill deadlines. Verify no
   survivors. Delete only the exact disposable root after digest capture.
9. Compare with `spocky-differential`. Normalize only documented generated ids
   (`wks_`, agent id, Codex thread id, `serverId`), wall-clock values, temporary
   paths, and ports. Each rule has a test. Never sort keys, never reorder, and
   never ignore stderr or exit codes. Any mismatch or count drop exits nonzero.

Gates, in order. A later gate's lane work may start, but it is not integrated
or claimed until the earlier gate passes.

| Gate | Scenario | Pass condition |
|---|---|---|
| G1 | `workspace create`, `run "<fixed prompt>" --provider codex --mode full-access`, `logs`, `ls -a`, `inspect` | real Codex happy path identical on both daemons |
| G2 | `--mode auto` with a scripted shell call that needs approval: `permit ls`, `permit allow`, `permit deny`, `stop` and cancel mid-turn | identical |
| G3 | daemon stop and start on the same home, `ls -a`, `inspect`, `send`, `logs` (timeline rebuilt from `thread/read`) | identical |
| G4 | failure (stub HTTP 500, missing codex), client disconnect and retry, old-state home made by the original daemon opened by Spocky and the reverse | identical |

## 5. Writer lanes

| Lane | Permitted paths (exclusive) | First deliverable | Acceptance commands | Model |
|---|---|---|---|---|
| `p3_contracts` | `crates/spocky-contracts/`, `scripts/phase3/contracts-*` | G1 messages and payloads as typed Rust with golden fixtures captured from the pinned zod schemas; byte-exact JSON roundtrip (key order kept) | `cargo test --locked -p spocky-contracts`; `cargo clippy --locked -p spocky-contracts --all-targets -- -D warnings`; `cargo fmt --package spocky-contracts -- --check` | Opus: optional, null, and missing semantics need judgment |
| `p3_daemon_transport` | `crates/spocky-daemon/` | `spocky-daemon` binary: listen precedence, `/ws`, admission, hello and `server_info`, ping, `paseo.pid`, `server-id`, `local-credential`; session handled through `spocky-session` | `cargo test --locked -p spocky-daemon`; clippy and fmt for `spocky-daemon` | Sonnet, with lead Opus review of admission code |
| `p3_session` | `crates/spocky-session/`, `crates/spocky-store/`, `crates/spocky-domain/` | dispatch and `rpc_error`, subscriptions, workspace registry files, agent create, send, wait, timeline, agent records | `cargo test --locked -p spocky-session -p spocky-store -p spocky-domain`; clippy and fmt for each | Opus: cross-module lifecycle |
| `p3_provider_codex` | `crates/spocky-provider-codex/` | `codex app-server` stdio JSON-RPC: `initialize`, `thread/start`, `turn/start`, notifications to timeline items; later `turn/interrupt`, approvals, `thread/resume`, `thread/read` | `cargo test --locked -p spocky-provider-codex` (tests drive the real pinned `codex` against the stub); clippy and fmt | Opus: novel protocol mapping |
| `p3_slice_harness` | `crates/spocky-slice-harness/`, `scripts/phase3/`, `evidence/phase3/gate*` | steps 1 to 9 above, run first with the original daemon on both sides to prove the harness detects no false mismatch, then G1 | `gtimeout --kill-after=30 1800 scripts/phase3/gate.sh g1` exits 0; `cargo test --locked -p spocky-slice-harness`; clippy and fmt | Opus: a weak gate produces false parity |

Rules for every lane: one file per signed commit, named paths only, no
`Cargo.toml` at root, no tests weakened, no placeholders, port 6767 never used,
disposable homes only, named tmux sessions for long services, Node 22.20.0 for
the original side. The lead reviews each commit range with the code-review
skill, routes fixes back, reruns the lane's acceptance commands on an
integration branch from main, then cherry-picks signed to main.

Slot conflict (fact): `p2_hub_triggers`, `p2_renderer_linux`, and
`p2_pglite_rust_host` are active. Five P3 lanes would make eight children
against the plan cap of three. Recommendation: launch `p3_contracts`,
`p3_slice_harness`, and `p3_provider_codex` first, since they need no other
lane to start. Start `p3_session` and `p3_daemon_transport` as P2 lanes
finish, or record a cap revision before launching them.

## 6. G1 lead decisions (2026-10-01)

Decisions on `evidence/phase3/gate-g1.md` "Deviations for lead decision" and
"Not compared", after read-only Opus review of harness range
`d6618ba..38b003a`.

| Item | Decision | Reason |
|---|---|---|
| `client_metadata` key reorder in stub request bodies | On hold since 2026-10-01 at the coordinator's challenge. Not accepted until the harness writer shows the origin with raw bytes. | The plan forbids normalizing order. Recorded so far: the field is inside the codex-to-stub HTTP request bodies, and the reviewer saw random key order across 12 original-versus-original sides with the same daemon and codex binary (inference: codex hash-map serialization). If the raw bytes show any daemon-controlled input changes the order, it is a parity bug for the Spocky daemon, not a transform. |
| Normalized classes beyond the plan list (project, client, Codex turn, window, installation, tools, message ids, daemon keypair, verified creation digests, CLI short id) | Accepted, except constant UUIDs. | Each is generated per run and tested. The recorded run normalized the nil and max UUIDs; the UUID shape must require version 4, 5, or 7 with a valid variant, and no rule may be emitted when both sides hold the same value. G1 parity runs wait for that fix. |
| Canonical enumeration order of captured state files | Accepted. | The harness lists files; listing order is not daemon output. File content and names stay exact. |
| `daemon.out` and `daemon.log*` not compared | Accepted for G1 only, recorded as a deviation. | Pino lines carry PIDs, metrics, and timings. Operator log parity belongs to capability `DOPS-001` and needs its own normalization rules before Phase 3 closes. |
| `codex-home` internals other than `config.toml` not compared | Accepted. | They are the external codex program's state. What the daemon sends codex is compared through the stub request records. |

Before a G1 parity claim: capture `project/` (files, `git status
--porcelain`, `HEAD`) and a `tmp/` listing; make the `READY` check match a
whole line; add compare-level negative tests for exit code, stub body, key
order, missing state file, and state content.
