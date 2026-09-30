# Relay runtime comparison

Status: passing local baseline and Rust runtime evidence. This does not select
the relay implementation or close `P2-RELAY-01`.

## Baseline runtime

- Relay commit: `3fc41c96c8c63f3a7109e832899cc57d473c4531`
- Elixir: `1.20.2`; OTP: `29.0.3`
- Container image: `elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79`
- Source: `.baselines/relay`, mounted read-only and copied inside the disposable container
- Command: `PASEO_OWNERSHIP_SURGE_COUNT=30 mix test test/paseo_relay_test.exs --seed 1`
- Result: 10 passed, 0 failed in 8.0 seconds

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
Its three focused tests prove:

1. Peer failure clears the unavailable owner without a controller `LOSE`
   command, then the survivor accepts takeover.
2. Text and binary WebSocket frames cross the owner unchanged and in order in
   both directions.
3. Partitioned peers accept duplicate owners; healing selects one owner, closes
   the loser with `1012 Session owner moved`, and returns `409` with the opaque
   winner target on a new upgrade.

The in-memory contract suite also passes five focused ownership, opacity,
pressure, capacity, and drain tests. Clippy passes for all relay targets with
warnings denied.

## Reproduction

```sh
scripts/phase2/relay-runtime.sh
```

The script rejects a dirty or incorrectly pinned baseline before running. It
mounts the baseline read-only, removes its disposable container, runs both Rust
runtime suites serially, and prints raw artifact hashes.

## Raw evidence

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `evidence/raw/phase2/relay-baseline-runtime.log` | 19,402 | `2bacdb670c5eb34fa4b7f99d63d47cb9c0aa4d98f094a0599bf51bd14cab8b0f` |
| `evidence/raw/phase2/relay-runtime.log` | 1,720 | `f357947f7a1d7b41bd6af8ae1fe8394aaf393210d0240c23bcde906dc68fc480` |

The logs are ignored local artifacts. Baseline compiler warnings come from
locked third-party Syn, WebSockex, and DNSCluster dependencies; all tests pass.

## Remaining gaps

- Baseline peers are separate BEAM processes. Rust network peers are threads in
  one test process; the separate-process Rust harness remains controller-driven.
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
