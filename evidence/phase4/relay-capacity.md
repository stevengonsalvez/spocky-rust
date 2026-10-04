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
the reply, the inputs the relay read (`~`), the messages the fake sockets received (`@`), and a
state line (gauges, pressure victims and batch, the order of the `active` and `blocked` trees,
map sizes, metric counters). `scripts/phase4/relay-capacity-differential.sh` runs it and replays the
result through the crate; the Rust side must print the same raw text.

## Result

```text
scripts/phase4/relay-capacity-differential.sh regenerate
relay capacity differential: 13931 operations: Rust identical to the pinned relay
```

- Operation script: 6 hand scenarios (lifecycle, limits, holder exits, ingress budget, delivery
  and blocked sources, shedding order) and 300 generated scenarios (2 to 6 processes, 20 to 59
  operations: admit, attach, message admit, start, finish, cancel, release, expire, kill, status,
  watermark, check, recheck). 42,906 transcript lines; every error reason of the API appears.
- Pressure: shed batches 1, 2, 4, 8, 16, 72 and 1,024 appear, so the first-batch formula and both
  growth branches (relief based and doubling) are compared.
- `cargo test -p spocky-relay` replays the committed transcript without Docker.

## Inputs, not masks

The BEAM memory reading is an input. The harness sets the watermark 16 MiB below the current
reading so the first batch is insensitive to drift, records the reading the relay used
(`pressure.memory` from `:sys.get_state`), and the replay gives it to the port. Two pinned runs
therefore differ in those numbers, and the check is the replay of a fresh capture. The delivery
wait time is a clock reading; only its observation count is compared.

## Findings

- A holder that attaches two connections overwrites its socket entry; releasing one leaves a stale
  key in the `active` tree and the next shed crashes the process (`BadMapError` in `pop_newest`,
  seen in the first run). The request process holds one connection, so the script avoids it. The
  port panics at the same point.
- `admit_message` and `start_delivery` check `caller?`, which is always true through the public
  API (`self()` is passed); the port drops those branches.

## Known gaps

- `PaseoRelay.Delivery.Writer`, `Delivery`, the socket's delivery pipeline (`pending` queue,
  `active` toggling, close codes 1013) and `Ownership` are the next ranges of this slice.
- Real timers (5 s reservation, 1 s check, 100 ms recheck) are runner work; the port exposes the
  firings as calls and the timer requests as effects.
