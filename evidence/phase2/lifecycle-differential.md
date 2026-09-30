# Full lifecycle runtime differential

Status: passing pilot evidence. This does not close Phase 2 or establish daemon lifecycle parity.

## Baseline

- Paseo commit: `5de45e208690b0efc51c59a585ae9729325a9204`
- Source: read-only pinned checkout at `.baselines/paseo-runtime`
- Original runtime: `AgentManager`, `AgentStorage`, lifecycle commands, and persistence loading
- Provider: original deterministic fake Codex client from the pinned test utilities
- Rust runtime: `AgentLifecycleMachine` and the lifecycle driver

## Scenario

`full-lifecycle-runtime` drives eight ordered phases through both runtimes:

1. create a live idle agent
2. stream an assistant response to completion
3. request and allow a tool permission, then verify its side effect
4. cancel an in-flight tool turn
5. close and persist the runtime for restart
6. resume the same provider session
7. archive the live agent
8. recover archived history without clearing the archive marker

The original side executes the pinned runtime manager and file-backed storage. Both sides emit phase observations, stream event order, process output, exit status, and measured counts. No normalization rules are applied.

## Result

- Original exit: `0`
- Rust exit: `0`
- Measured counts: eight fixtures, twenty-six assertions per side
- Differences: none
- Original structured-output SHA-256: `0bda70a954f974163e53ed0b208ad0986a87c3e8e0d15034a9ee5c2298ce8b40`
- Rust structured-output SHA-256: `0bda70a954f974163e53ed0b208ad0986a87c3e8e0d15034a9ee5c2298ce8b40`
- Raw manifest SHA-256: `b31fd353246e2b1b8930dada2418c0ac631ff497983aa86b738d124c8810ef94`
- Raw manifest: ignored local artifact at `evidence/raw/phase2/lifecycle-differential.json`

## Limits

- Provider behavior comes from Paseo's deterministic fake Codex session, not a production provider process.
- The Rust side remains a lifecycle domain pilot, not a daemon, WebSocket, or provider runtime.
- Crash recovery, concurrent lifecycle mutations, and production provider failures remain uncovered.

## Reproduction

```sh
cargo build -p paseo-domain --bin paseo-lifecycle-driver
cargo run -p paseo-domain --example lifecycle_differential
```

The example rejects a reference checkout whose tracked tree is dirty or whose HEAD differs from the pinned commit.
