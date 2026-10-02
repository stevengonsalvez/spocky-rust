# Phase 3 G1 wiring: recorded deviations

Lane `p3_g1_wiring`, crate `crates/spocky-daemon-app`, 2026-10-01. This file
records behavior in the wiring crate that has no baseline counterpart, so a
reviewer can tell intended differences from parity defects.

## Rust-only safety nets

### A panicking request handler

`src/request.rs` `handle_request` wraps the dispatch call in `catch_unwind`.
A panic becomes the same frames a thrown handler error produces in
`session.ts` `handleRequest`:

1. `rpc_error` with `Request failed: <message>` and code `handler_error`.
2. `activity_log` with content `Error: <message>`.

`<message>` is the panic message when the payload is a string. Otherwise it is
the fixed text `handler panicked`, which the baseline never emits.

The baseline has no equivalent. A JavaScript handler throws an `Error` whose
own message fills these frames, and there is no panic. In Rust a panic is a
bug in a handler, not a modeled failure. This net keeps one buggy request from
taking down the connection thread that also serves `ping` and later requests.

It is unreachable in parity runs. Every slice handler returns its errors as
values, and a gate run that hit it would show an `rpc_error` the original
daemon never sends, so the gate would fail rather than hide it.

Test: `request::tests::a_panicking_handler_becomes_handler_error`.

## Fresh-home bootstrap state

The pinned original daemon (`node packages/cli/dist/index.js daemon run`
from the `5de45e2` build, Node v22.20.0) and `spocky-daemon` each booted on a
fresh disposable home with the same `config.json` (same loopback port, relay,
dictation, and voice mode off), served one pinned `paseo ls --json`, and were
stopped with SIGTERM. Both ran under `sandbox-exec` with the egress-deny
profile, in tmux on the lane socket, with only recorded PIDs signalled. Both
exited 0 and printed `[]`.

The home trees were listed as path, mode, size, and SHA-256:

| Entry | Original | Spocky |
|---|---|---|
| `.` | dir 700 | dir 700 |
| `config.json` | 600, 138 bytes, same SHA-256 | same |
| `runtime/`, `runtime/opencode/` | dir 755 | dir 755 |
| `runtime/opencode/paseo-a88cef53...872.mjs` | 644, 547817 bytes, SHA-256 `a88cef53578dcb32cfa4af17e13e44c84751a30e5c19f847733904dd84eda872` | identical |
| `schedules/` | dir 755 | dir 755 |
| `cli-client-id`, `daemon-keypair.json`, `server-id` | 600, same sizes | 600, same sizes, generated content differs per run |
| `daemon.log` | 644 | absent |

The generated files are the per-run identities the G1 harness normalizes.
`daemon.log` is the pino file log, written by the transport's logger; the G1
gate does not compare it (`gate-g1.md`, Not compared) and its parity belongs
to `DLOG-001`.

The `runtime/opencode` file comes from `assets/opencode-bridge-plugin.bundle.mjs`,
a byte-for-byte copy of the pinned build's
`server/agent/providers/opencode/bridge-plugin.bundle.mjs`, written
atomically before the daemon listens, as `OpenCodeBridge.start` does. The
`schedules` directory is the schedule store `ScheduleService.start` creates;
the schedule service itself is outside the slice.

Raw trees (untracked, scratchpad `fh2/`): original SHA-256
`f89b14d105e400e690f2d36b33213708a165fe63b0453b14debea268111259d5`, Spocky
SHA-256 `221efd587a63678b0921f2844ba856c5c4ecde5ab7e5451612e728df598abcfc`.

## agent_update wire differential

`crates/spocky-daemon-app/tests/differential/agent-update-differential.sh`
runs one pinned client (`connectToDaemon` from the pinned CLI build) against
each daemon. The client:

1. Subscribes with `fetch_agents_request` and `subscribe: {}`.
2. Creates a directory workspace.
3. Creates a codex agent in `full-access` mode with the G1 prompt, answered
   by the loopback Responses stub.
4. Waits for the agent to finish.

It records every `fetch_agents_response`, `agent_update` and
`agent.create.response` frame in arrival order. Per-run values are masked:
UUIDs, `wks_` and `prj_` ids, ISO timestamps, and the disposable root path.

Result on 2026-10-02 (branch `p3-g1-wiring` on main `5ea85994`):

- Both sides recorded 9 frames, and the masked sequences are byte-identical,
  key order included.
- The sequence is: `fetch_agents_response`; upserts `initializing`, `idle`,
  `running`, then `idle` again; `agent.create.response` with `running`; then
  upserts `running`, `running`, `idle`.
- The second `idle` is `forwardLiveAgent(snapshot)` after `createAgentCommand`
  returns (`session.ts:4308`). It carries the pre-prompt snapshot, so it
  repeats the earlier `updatedAt`, and it goes out before the create
  response, as the baseline awaits it.

Both daemons ran under `sandbox-exec` with the egress-deny profile, in tmux on
the lane socket, on disposable homes and random ports. Only recorded PIDs were
signalled.

The script checks each side before comparing:

- The stub answered exactly one scripted request, with nothing unscripted.
- The kernel log holds no sandbox `network-outbound` denial for any PID in
  the side's process tree. The tree is tracked from the daemon PID, plus each
  codex invocation's PID.

An exit trap stops the stub, the daemon and the exact tmux session, and
removes the disposable root, on any failure. The whole run is bounded at 900
seconds.

Run with these checks (out dir
`scratchpad/au-pkg3` of this lane's session, untracked):

- original: 1 scripted stub request, no egress from 25 tracked PIDs.
- spocky: 1 scripted stub request, no egress from 11 tracked PIDs.

| File | SHA-256 |
|---|---|
| `original/frames.jsonl` (raw) | `aded18b2c7f5553cc0d4acc9a6e3f81ed011b9d6bc212be70d474102bce339d6` |
| `spocky/frames.jsonl` (raw) | `e2d0eb3becdc2e5b30a98c367da1a1f578eb662de1bee9cc396f097b6ec7ea1e` |
| `original/masked.jsonl` | `d6b301c11fd83a9431cffc7046fafe62a9d5e153353308b129f0e9a0d92ac958` |
| `spocky/masked.jsonl` | `d6b301c11fd83a9431cffc7046fafe62a9d5e153353308b129f0e9a0d92ac958` |
