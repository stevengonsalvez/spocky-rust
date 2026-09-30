# Relay local runtime pilot

Status: passing local runtime evidence. This does not select the relay implementation or close `P2-RELAY-01`.

## Baseline

- Relay commit: `3fc41c96c8c63f3a7109e832899cc57d473c4531`
- Source: clean read-only checkout at `.baselines/relay`
- Referenced contracts: ownership convergence, opaque reroute target, owner-loss close `1012`, pressure close `1013`, ordered opaque forwarding
- Baseline runtime execution: unavailable because Elixir and Mix are not installed on this host

## Scenario

`runtime_process` starts separate `paseo-relay-node` operating-system processes with piped command channels. Each process holds an independent relay state replica. Commands have a two-second response deadline, and test cleanup kills and waits for every remaining child.

The three cases cover:

1. Two processes receive the same claim set in opposite order, converge on `alpha` generation 1, and return an opaque reroute from a `beta` landing.
2. XChaCha20-Poly1305 ciphertext enters and leaves an `alpha` process byte-for-byte. Process observations expose sequence and wire size only. Decryption occurs in the test process after forwarding.
3. A 64-byte link admits 48 bytes, rejects the next 32 bytes with `1013 SlowConsumer`, and removes the link. The test kills the owning process, reports the loss to the surviving replica, claims `beta` at generation 2, and forwards a new frame.

## Result

- Runtime tests: 3 passed, 0 failed
- Ownership processes: distinct PIDs recorded in raw trace
- Ciphertext: 62 wire bytes preserved byte-for-byte
- Plaintext in relay responses: absent
- Pressure boundary: 48 bytes admitted, 80-byte aggregate rejected against 64-byte limit
- Pressure close: `1013`
- Failure: owner child killed and reaped
- Recovery: survivor owns generation 2 and forwards 4 bytes
- Raw log size: 947 bytes
- Raw log SHA-256: `de6b29b9494f0a786d54e8d6630cf561033b75ec010318a14b03cd989b80e91d`
- Raw log: ignored local artifact at `evidence/raw/phase2/relay-runtime.log`

## Reproduction

```sh
scripts/phase2/relay-runtime.sh
```

The script rejects a dirty or incorrectly pinned relay baseline before running the Rust process test.

## Remaining gaps

- Replica commands are driven by the test controller. No peer discovery, network gossip, or automatic failure detector runs between Rust nodes.
- The process protocol is a pilot harness, not the Paseo WebSocket protocol.
- No live partition healing, duplicate-owner conflict, deployment adapter, readiness, metrics endpoint, or rolling drain is exercised.
- Original-versus-Rust runtime comparison remains blocked until Elixir and Mix are available.
- Linux service, production load, deployment, and paid-service evidence remain untested.
