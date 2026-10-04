# Relay delivery writer, slice 3 part 2

Capability: CLOUD-RELAY-FLOW-021 (ordered delivery, backpressure, load shedding).
Crate: `spocky-relay`, module `writer`: a sans-IO port of `PaseoRelay.Delivery.Writer`
(`relay@3fc41c96c8c63f3a7109e832899cc57d473c4531`).

## Port

The BEAM process is a `GenServer`. The port keeps its state (the active reservation, the queue of
payload reservations and control frames, the queued control bytes) and takes its inputs as calls:
`reserve`, `write`, `control`, `close`, `written` (the destination's acknowledgement),
`reservation_timeout`, `destination_down`, `source_down`, with the clock as an argument. What the
process does to the world comes back as `Effect`s: replies, frames and close messages to the
destination, reservation timers, monitors, metric increments and the stop. A runner owns time,
the destination and the sources.

## Baseline

`scripts/phase4/relay-writer-baseline.exs` starts the real `PaseoRelay.Delivery.Writer` with a fake
destination process and drives it from source processes. One block per operation:

- `d`: what the destination received, in order (frames with their whole payload, write barriers,
  close messages);
- `r`: the replies the sources got, in the order the Writer sent them (taken from its send trace);
  callers that got an exit instead of a reply have no send to order and follow, by name;
- `state`: alive, the active reservation, the queue (source and bytes, or control bytes), queued
  control bytes, live monitors and armed timers (`Process.info(writer, :monitors)`,
  `Process.read_timer`), and the metric counters.

Operations: `reserve`/`write` through the client functions and `reserve_raw`/`write_raw` through
`GenServer.call` (so the server sees a deadline that already passed), `control`, `ack`,
`timeout` (the `{:reservation_timeout, token}` message), `close`, `kill` of a source, `kill_dest`.
Deadlines are `far` or `past`.

The harness settles an operation without timing guesses: it traces what the Writer receives and
sends, waits until the source's call has reached the Writer, syncs with `:sys.get_state` until the
mailbox is empty, then waits for the `done` of exactly the sources the send trace says were
replied to (or every waiting caller if the Writer stopped), and for the destination's barrier echo.
An operation for a source that is waiting for a reply is skipped (`= skip`) on both sides.

## Result

```text
scripts/phase4/relay-writer-differential.sh regenerate
relay writer differential: 4781 operations: Rust identical to the pinned relay
```

- 14 hand scenarios (one reservation, queueing, invalid reservations and expired deadlines,
  control frames to the byte bound, control behind a reservation, an expired control deadline,
  reservation timeout, timeout during a write, source exits, a dead source in the queue,
  destination exit, close, a second write on one reservation, a queued raw reserve with a passed
  deadline) and 150 generated scenarios, 12,370 transcript lines.
- Outcomes in the transcript: `invalid_reservation`, `timeout`, `destination_closed`,
  `source_closed`, call exits, close 1013 `Slow consumer` (74), close 1013 `Delivery unavailable`
  (6), close from `Writer.close` (15), control queue overflow, binary and text frames.
- The transcript has no clock or memory input. `scripts/phase4/relay-writer-differential.sh` runs
  the pinned relay twice and diffs both against each other and against the committed transcript:
  three pinned runs are byte identical.
- `cargo test -p spocky-relay` replays the committed transcript without Docker.

## Findings

- A source that is already dead when it is granted a reservation is monitored and its exit message
  arrives at once, so the grant is followed by `source_closed` and the next entry. The port models
  the exit message in the same step.
- A second `write` on one reservation replaces the first caller's `from`; that caller is never
  answered and exits when the Writer stops.
- Time inside the Writer is `Deadline.remaining`, a function of the monotonic clock. The
  differential uses deadlines far in the future or already passed; deadlines that expire while
  waiting need real time and are runner work (the port takes `now`).

## Known gaps

- The Writer's timers (the reservation timeout) are driven by injecting the message; the real
  timer is runner work.
- `Delivery.deliver/4` (fan-out to several writers with `Task.async`), the socket's delivery
  pipeline (`pending` queue, close codes `Delivery unavailable` and `Data route unavailable`), and
  `Ownership` are the next ranges.
