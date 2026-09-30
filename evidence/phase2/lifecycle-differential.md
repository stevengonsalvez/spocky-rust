# Lifecycle cancellation differential

Status: passing pilot evidence. This does not close Phase 2 or establish full lifecycle parity.

## Baseline

- Paseo commit: `5de45e208690b0efc51c59a585ae9729325a9204`
- Source: disposable clean clone at `.baselines/paseo-runtime`
- Original implementation: `packages/server/src/server/agent/lifecycle-command.ts`
- Rust implementation: `crates/paseo-domain/src/lib.rs`

## Scenario

`cancel-lifecycle-cases` executes four cancellation states through the original `cancelAgentRunCommand` and the Rust `AgentLifecycleMachine`:

1. no in-flight run
2. acknowledged cancellation
3. run settled during cancellation
4. provider refusal

Each side emits acceptance, cancellation, resulting lifecycle, and semantic error fields. The comparison also covers stdout, stderr, exit code, and assertion counts. It uses no normalization rules.

## Result

- Original exit: `0`
- Rust exit: `0`
- Expected counts: four fixtures, sixteen assertions
- Differences: none
- Raw manifest SHA-256: `3dfb3b3928fcd23aa67f37a39ac66e64515d76bd91023d35a9c2fdee32b3f616`
- Raw manifest: ignored local artifact at `evidence/raw/phase2/lifecycle-differential.json`

## Reproduction

```sh
cargo build -p paseo-domain --bin paseo-lifecycle-driver
cargo run -p paseo-domain --example lifecycle_differential
```

The example rejects the run unless the disposable baseline clone is clean and exactly at the pinned Paseo commit.
The provider-refusal error is compared verbatim. Both drivers derive assertion counts from emitted cases.
