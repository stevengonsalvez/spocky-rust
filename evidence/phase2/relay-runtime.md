# Relay runtime comparison

Status: passing local baseline and Rust runtime evidence. This does not select
the relay implementation or close `P2-RELAY-01`.

## Baseline runtime

- Relay commit: `3fc41c96c8c63f3a7109e832899cc57d473c4531`
- Elixir: `1.20.2`; OTP: `29.0.3`
- Container image: `elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79`
- Source: `.baselines/relay`, mounted read-only and copied inside the disposable container
- Command: `PASEO_OWNERSHIP_SURGE_COUNT=30 mix test test/paseo_relay_test.exs --seed 1`
- Result: 10 passed, 0 failed in 4.6 seconds

The focused baseline run uses real BEAM peers and Cowboy WebSockets. It covers
concurrent ownership, opaque reroute targets, owner death and takeover, ordered
bidirectional frames, duplicate owners during a partition, healing to one owner,
loser close `1012`, and a new `409` reroute from the losing listener.

## Rust runtime

The existing process harness still starts separate `paseo-relay-node` processes
for deterministic ownership, ciphertext forwarding, bounded pressure, process
termination, and generation recovery. Its three focused tests pass.

`NetworkNode` adds real loopback peer and WebSocket listeners. Peers pull
ownership snapshots directly, detect failed peer listeners after three bounded
connection failures, and reconcile duplicate owners after connectivity returns.
Its five focused tests prove:

1. Peer failure clears the unavailable owner without a controller `LOSE`
   command, then the survivor accepts takeover.
2. Text and binary WebSocket frames cross the owner unchanged and in order in
   both directions.
3. Partitioned peers accept duplicate owners; healing selects one owner, closes
   the loser with `1012 Session owner moved`, and returns `409` with the opaque
   winner target on a new upgrade.
4. Fragmented WebSocket upgrade headers complete within the handshake deadline.
5. A full 32-frame network queue closes only the slow consumer with `1013 Slow
   consumer`; the source remains live and forwards to a fresh healthy peer.

The same peer and WebSocket runtime also runs in separate
`paseo-relay-network-node` processes. Two focused tests prove automatic owner
loss and takeover after killing the owner process, plus partition healing,
loser close `1012`, and `409` reroute without a controller `LOSE` command.

The in-memory contract suite also passes five focused ownership, opacity,
pressure, capacity, and drain tests. Clippy passes for all relay targets with
warnings denied.

## Reproduction

```sh
scripts/phase2/relay-runtime.sh
```

The script rejects a dirty or incorrectly pinned baseline before running. It
mounts the baseline read-only, removes its disposable container, runs all three
Rust runtime suites serially, and prints raw artifact hashes.

## Raw evidence

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `evidence/raw/phase2/relay-baseline-runtime.log` | 18,970 | `9720735cc8162e23bd0bfad8be23b405bb7fbba0dbd7ae3b047c0f6e1ae6a709` |
| `evidence/raw/phase2/relay-runtime.log` | 2,188 | `f461d884068c15a8115663674997360b1fc0bffc8d0f10a24a7e972cd4fd01b7` |

The logs are ignored local artifacts. Baseline compiler warnings come from
locked third-party Syn, WebSockex, and DNSCluster dependencies; all tests pass.

## Remaining gaps

- Rust peer addresses are configured explicitly. Discovery, deployment adapter,
  and rolling topology changes are not exercised.
- Rust failure detection uses bounded TCP failures, not process monitors or an
  implementation selected for production.
- Rust conflict selection is deterministic by node ID. It matches tested
  outcomes, not Syn's internal conflict algorithm.
- The Rust WebSocket pilot does not implement the full Paseo control protocol,
  identifier limits, capacity ledger, readiness, metrics, or rolling drain.
- Linux service, production load, deployment, and paid-service evidence remain
  untested.
