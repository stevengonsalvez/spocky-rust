# Terminal port: known deviations from the pinned baseline

Pinned baseline: Paseo `5de45e2`, Node 22.20.0. Each entry names the
baseline behavior, what the port does instead, and why the difference cannot
be reached by a test or is unsafe to reproduce. Everything not listed here is
compared as text against the pinned dist by the differentials in
`crates/spocky-terminal/tests/`.

## Sanctioned (behavior differs on purpose)

| Area | Baseline | Port | Reason |
| --- | --- | --- | --- |
| `CSI < n u` (kitty keyboard pop) in `input_mode.rs` | Loops `n` times, so `\x1b[<2000000000u` runs longer than 20 s and `Infinity` never returns | Stops when the flag stack is empty | The final state is identical. The baseline cost is a hang, not an output. Small counts are tested against the pinned dist |
| Child environment in `pty.rs` | `execve` with the env block | `/usr/bin/env -i K=V...` in front of `spocky-pty-helper` | `unsafe_code` is forbidden in this workspace, so the helper cannot call `execve` with a custom envp. The env is visible in the argv of the helper for the moment between spawn and exec, and a very large env is limited by ARG_MAX |
| Inherited descriptors in `spocky-pty-helper` | node-pty closes inherited fds (`CLOEXEC_DEFAULT` on macOS, an fd sweep on Linux) | Descriptors are not closed | Needs unsafe or a platform-specific close range. Signal dispositions are reset, which is the part that changes behavior (`pty.rs` `reset_signal_dispositions`) |
| `kill` after exit in `pty.rs` | node-pty always calls `process.kill`, which throws `ESRCH` on a reaped pid | The signal is skipped | Signalling a recycled pid is unsafe. The caller sees no error in either case |
| termios setup in `pty.rs` | Zeroed structure with the node-pty flags | Starts from `tcgetattr` then sets the same flags | The flags are the same. Fields node-pty leaves zero (control characters) keep the tty defaults here, which no test observes yet |
| `FrameDecoder` in `v8_serialize.rs` | Frames are read by Node's IPC channel | The decoder buffers a pending frame without a size cap | Reviewer P3 finding, kept as is. The peer is the pinned worker we spawn, not a remote client |
| `exit_lines.rs` cut inside a surrogate pair | `slice` keeps a lone surrogate | The cut becomes U+FFFD | Rust strings cannot hold a lone surrogate. The cut happens only for a line longer than the exit-line limit |
| `limit 0` in `exit_lines.rs` and `snapshot.rs` | `slice(-0)` returns everything | Returns an empty list | Only the fixed limit 12 is ever passed |
| Exit race | The baseline can handle the PTY exit before the parse of the output just before it, which drops that output | The port always parses first | See `terminal-exit-race.md`: pinned runs show the output delivered when the child lingers, and the drop is a race |
| Handler replies in `handlers.rs` | Written to the PTY during the xterm parse | Queued and written after `Terminal::write` | The session actor writes them before any later input, so the PTY sees the same order |

## Proposed rows for the approved divergence table

`porting/inventory-summary.md` is the lead's file, so these rows are for the
lead to add (reviewer verdict on `5647b109^..d37d6344`, 2026-10-03). The code
comment in `input_mode.rs` cites DIV-005.

| ID | Divergence | Decision |
|---|---|---|
| DIV-005 | Kitty keyboard `CSI < n u` (pop `n` entries of the flag stack). The pinned `terminal-input-mode.ts:183-186` pops `count` times, so a huge or infinite count never returns: `\x1b[<2000000000u` ran past 20 s on pinned Node 22.20.0. `spocky-terminal` `input_mode.rs` stops at the empty stack. Popping an empty stack gives flags 0 every time, so the final state is the same. | Coordinator decision, 2026-10-03: ledger it and do not port the loop. A safety divergence, like DIV-002: reproducing it would hang the session. `tests/input_mode.rs` compares counts up to the stack depth and small overshoots with the pinned tracker. |
| DIV-006 | A shell that prints and exits at once. In pinned `terminal.ts` the PTY exit can be handled between the parse of the last output and its write callback, which drops that output for subscribers while the exit lines still hold it: `sleep 0.4; printf END` delivered the output in 9 to 16 of 25 runs. `spocky-terminal` `session.rs` always parses first, so it always delivers. | Traceability row, not an approval: delivery is one of the two pinned outcomes. Evidence in `evidence/phase4/terminal-exit-race.md`. The session differential ends every script with `sleep 0.5` so that it compares the one deterministic outcome. |

## Controller (`controller.rs`)

The handlers are the pinned promise code with its awaits made explicit
(`ControllerHost`). Where the port gives up the baseline's microtask counts:

| Area | Baseline | Port | Reason |
| --- | --- | --- | --- |
| Awaits on settled manager results (`getTerminals`, `captureTerminal`, `getAllTerminalSessions`) | Each `await` yields one or more microtasks | The handler runs through at the call, except the one `await` of the directory refresh and the stream completion, which are modelled as one tick each | The tick in the refresh is what makes a burst of change events coalesce (two emits for a `killAll`), and the one in the completion orders a stream exit after the caller's next statement. Both are compared with the pinned controller. Other ticks only reorder a handler against unrelated microtasks, which no host-visible message depends on |
| `unsubscribe_*_request` | The dispatch promise settles after the owner's `stop`, which for a stream waits for an in-flight snapshot read | Settles when the owner is released | Nothing is replied after the release. The differential leaves these dispatches untracked |
| A listed terminal that is removed while a list or refresh is in flight | `toTerminalInfo` reads the object, which keeps its last title | The title read when the list was taken is kept | A terminal killed within one microtask window of the request |
| `ownership` | `SessionDelivery` | Not ported here: `ControllerHost::begin_owner`, `release_owner`, `release_legacy_slot` are the seam. The differential plays the delivery with a recording host and compares it with the pinned `SessionDelivery` | Owner delivery belongs to the session lane |

## Not ported on purpose

- The activity tracker (`TerminalActivity`, `DTRM-003`) is excluded from
  `manager.rs`. List items carry no activity, and the manager differential
  compares them without it.
- A failed `createTerminal` in `manager.rs` (the token is dropped and the error
  returned) is not compared: a missing cwd or command does not fail the
  baseline create, it starts a shell that exits at once, so the differential
  has no deterministic trigger for it.
