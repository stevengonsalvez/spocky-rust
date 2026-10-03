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

## Not ported on purpose

- The activity tracker (`TerminalActivity`, `DTRM-003`) is excluded from
  `manager.rs`. List items carry no activity, and the manager differential
  compares them without it.
