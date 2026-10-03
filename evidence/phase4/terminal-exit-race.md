# Terminal exit versus the last output (DTRM-001)

Question: when a shell prints and exits at once, does pinned Paseo `5de45e2`
deliver that last output to a subscriber, or always drop it?

Answer: it is a race. Both outcomes are pinned behavior, so the Rust session
(`crates/spocky-terminal/src/session.rs`) chooses delivery and records the
choice as a deviation.

## Mechanism

`terminal.ts` queues each PTY chunk in the headless emulator and sends the
`output` message from the write callback, which runs on a later tick than the
parse. `onExit` sets `killed = true` first, and the write callback returns
early when `killed`. If the exit event is handled between the parse and the
callback, the output never reaches subscribers, while the buffer already holds
it. The exit info lines come from the buffer.

## Measurements

Pinned Node v22.20.0 (SHA-256
`1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931`), pinned
build at `5de45e2`, macOS x64, `scripts/phase4/terminal-exit-race.mjs`
(SHA-256 `8e90c0ab37de076970ec153af5d914121a53d05ca3def4bd81d1d092e4987c27`),
25 trials per row, 5 rows by 30 columns:

| script | trials | last output delivered | dropped | in exit lines |
|---|---|---|---|---|
| `sleep 0.4; printf END` | 25 | 13 | 12 | 25 |
| `sleep 0.4; printf END` | 25 | 16 | 9 | 25 |
| `sleep 0.4; printf END` | 25 | 12 | 13 | 25 |
| `sleep 0.4; printf END; sleep 0.3` | 25 | 25 | 0 | 25 |

Without a pause after the output, 41 of 75 trials deliver it and 34 drop it.
With a 0.3 s pause it is delivered in 25 of 25. The exit info always has it.

Command:

```sh
env -i PATH=/usr/bin:/bin HOME=/tmp node scripts/phase4/terminal-exit-race.mjs \
  <pinned dist/server/terminal> 25 'sleep 0.4; printf END'
```

## Consequence

The port parses the last chunk before it handles the exit, so subscribers
always get it. That is one of the two pinned outcomes. The session
differential (`tests/session.rs`) ends each scenario with a pause after its
last output so that it compares only the deterministic outcome.
