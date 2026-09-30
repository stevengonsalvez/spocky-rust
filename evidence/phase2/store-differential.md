# Stored agent differential

Status: passing pilot evidence. This does not close Phase 2 or establish full state parity.

## Baseline

- Paseo commit: `5de45e208690b0efc51c59a585ae9729325a9204`
- Source: disposable clean clone at `.baselines/paseo-runtime`
- Original implementation: `packages/server/src/server/agent/agent-storage.ts`
- Dependency install: frozen root `package-lock.json` through `npm ci`

## Scenario

`stored-agent-write-restart` sends one schema-valid stored agent to the original TypeScript `AgentStorage` and the Rust `AgentStore`. Each process writes the record, constructs a new store instance, reloads the record, and emits the same structured, recovery, count, stdout, stderr, exit-code, and persisted-file captures.

The fixture includes nested provider options, MCP configuration, runtime metadata, persistence metadata, a feature definition, and daemon ownership. The comparison uses no normalization rules.

## Result

- Original exit: `0`
- Rust exit: `0`
- Expected counts: one fixture, five assertions
- Differences: none
- Raw manifest SHA-256: `32e0866aee12ad95a3598e0b1f41f6062ec7ce7439b772cf5c64b9d9dbb0f7ed`
- Raw manifest: ignored local artifact at `evidence/raw/phase2/store-differential.json`

The first valid comparison exposed sorted Rust object keys in the persisted file. Enabling `serde_json` `preserve_order` made persisted bytes match the original insertion order.
Both drivers derive the five-assertion count from completed write, file, restart, identity, and value checks.

## Reproduction

```sh
npm run build:client --prefix .baselines/paseo-runtime
cargo build -p paseo-store --bin paseo-store-driver
cargo run -p paseo-store --example store_differential
```

The example rejects the run unless the disposable baseline clone is clean and exactly at the pinned Paseo commit.
