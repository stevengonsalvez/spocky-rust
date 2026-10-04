# Relay capacity, slice 3 part 1

Capability: CLOUD-RELAY-FLOW-021 (ordered delivery, backpressure, capacity, load shedding).
Crate: `spocky-relay`, module `capacity`: a sans-IO port of `PaseoRelay.Capacity`
(`relay@3fc41c96c8c63f3a7109e832899cc57d473c4531`).

## Port

The BEAM process is a `GenServer`. The port keeps its state maps and takes its inputs as
arguments: the caller, the liveness the scheduler reports for a holder, process exits
(`process_down`), timer firings (`expire`, `check`, `pressure_recheck`) and the memory reading.
What the process does to the world comes back as `Effect`s: reservation timers, monitors,
`:relay_memory_pressure` sends, and the metric increments. A runner owns time and sockets.

## Baseline

`scripts/phase4/relay-capacity-baseline.exs` drives the real `PaseoRelay.Capacity` (same Docker
image and pinned source as slice 1) from fake socket processes and prints one block per operation:
the reply, the inputs the relay read (`~`), the `:relay_memory_pressure` sends in the order the
relay made them (`@`, taken from a send trace of the Capacity process and compared unsorted), and
a state line: gauges, pressure victims and batch, the armed recheck count (the relay keeps a flag; the port counts the
`SchedulePressureRecheck` effects it has not yet consumed, so a double schedule prints 2 against the
relay's 1), the live monitors
(`Process.info(capacity, :monitors)`), the live reservation timers (`Process.read_timer`), the
order of the `active` and `blocked` trees, map sizes and metric counters.
`scripts/phase4/relay-capacity-differential.sh` runs it and replays the result through the crate;
the Rust side must print the same raw text.

The crate's effects are replayed, not discarded: the replay derives the live monitors, the live
reservation timers and the recheck flag from the `Monitor`, `Demonitor`, `StartReservationTimer`,
`CancelReservationTimer` and `SchedulePressureRecheck` effects and prints them in the state line, so
a missed demonitor, cancel or recheck would differ from the relay. The `check` operation asserts
that `ScheduleCheck` is emitted once. The delivery wait is compared by its observation count; its
duration is a clock reading.

## Result

```text
scripts/phase4/relay-capacity-differential.sh regenerate
relay capacity differential: 11514 operations: Rust identical to the pinned relay
```

- Operation script: 12 hand scenarios (lifecycle, limits, holder exits, ingress budget, delivery and
  blocked sources, shedding order, batch sizes with continued pressure and recovery, the batch
  cap, idle pressure, `start_delivery` under pressure) and 300 generated scenarios. The first
  version of this range had 13,931 operations; regenerating the script with the shorter pressure
  phases (the relay's 100 ms recheck) and the generator that runs on the port changed the count
  to 10,224, then 11,514 after the pressure-start cases, so the earlier figure no longer applies. 36,309 transcript lines. The generator runs
  each operation on the port while it writes the script, so most targets exist: 125 starts, 334
  attaches and 341 message admits succeed. Every error reason of the API appears, including `start_delivery` `:pressure` (4 cases).
- Pressure: shed batches 1, 2, 3, 4, 6, 8, 12, 16, 21, 24, 64, 128, 256, 512, 1,024 and others
  appear. The first batch is `ceil((memory - watermark) / 33554418)` clamped to 1..64; the hand and
  generated scenarios use watermarks that put the reading in the middle of a step (16, 48, 80 and
  112 MiB above it), plus a huge excess for the cap. The recovery clause (`capacity.ex:441`)
  clears pressure in the transcript, and pressure continues below the watermark.
- Multi-victim sends: 19 `@` lines list two or more sends in the order the relay made them (oldest
  blocked first, then the newest active).
- `cargo test -p spocky-relay` replays the committed transcript without Docker.
- `tests/capacity_boundaries.rs` pins what the differential cannot: a reading equal to the
  watermark, equal to the recovery level, and the batch rounding and cap, with exact readings.
  The relay reads `:erlang.memory(:total)` itself, so no run lands on those values.

## Inputs and timers

The BEAM memory reading is an input. The harness sets the watermark about half a step away from
the current reading so the first batch does not depend on drift, records the reading the relay used
(`pressure.memory` from `:sys.get_state`), and the replay gives it to the port. Two pinned runs
therefore differ in those numbers, and the check is the replay of a fresh capture.

The relay's own timers (`:check` after 1 s, `:pressure_recheck` 100 ms after a shed) cannot be
held back. The harness traces what the Capacity process receives; a scenario during which a timer
message arrived that the harness did not send is run again, so every block comes from a run the
harness alone drove. Pressure phases at the end of a scenario are kept short for the same reason.

## Findings

- A holder that attaches two connections overwrites its socket entry; releasing one leaves a stale
  key in the `active` tree and the next shed crashes the process (`BadMapError` in `pop_newest`,
  seen in the first run). The request process holds one connection, so the script avoids it. The
  port panics at the same lookup, before any state change.
- `admit_message` and `start_delivery` check `caller?`, which is always true through the public
  API (`self()` is passed); the port drops those branches. The dead-caller clauses
  (`live_local?(caller_pid(from))` on admit, `live_local?(socket)` on attach) are runner contract:
  a dead caller's call is dropped before dispatch; a call from a dead process cannot be made in
  the harness.

## Runner contract

- A crash of the Capacity process (the stale key above, or a mutation call that timed out and was
  killed, `capacity.ex:607-616`) restarts it with empty state, after the sends and metric
  increments of the batch's earlier victims already happened. The runner catches the panic,
  drains `take_effects`, and resets with `Capacity::new`.
- Callers see `{:error, :unavailable}` when Capacity is down (`capacity.ex:600-612`); the observe
  functions fall back to 0, an empty map and `:unavailable` (`:618-622`). `value/1` crashes on an
  unknown name (`:238-248`); the port exposes the gauges only.
- Time: the delivery wait difference is floored to microseconds once, from the native-unit
  (nanosecond) clock, as `System.convert_time_unit` does.

## Known gaps

- `PaseoRelay.Delivery.Writer`, `Delivery`, the socket's delivery pipeline (`pending` queue,
  `active` toggling, close codes 1013) and `Ownership` are the next ranges of this slice.
- Real timers (5 s reservation, 1 s check, 100 ms recheck) are runner work; the port exposes the
  firings as calls and the timer requests as effects.
- Not generated: a second attach by one holder (it crashes the relay), a re-shed after a stale
  blocked key, and holders alive but different from the caller are admitted only for processes
  that hold no other connection.
