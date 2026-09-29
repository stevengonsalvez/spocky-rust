# Paseo Rust Exact Parity

This repository rebuilds Paseo-owned implementation surfaces in Rust against four immutable source baselines. Behavior, compatibility, state, visuals, interactions, accessibility, performance, packaging, failure recovery, and supported platforms must match before feature work begins.

## Immutable baselines

| System | Commit | Local source |
|---|---|---|
| Paseo | `5de45e208690b0efc51c59a585ae9729325a9204` | `/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite` |
| Hub | `28f6c78833065fd282f9064f92a9aa61875dd359` | `.baselines/hub` |
| Distributed relay | `3fc41c96c8c63f3a7109e832899cc57d473c4531` | `.baselines/relay` |
| Importer | `8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5` | `.baselines/import` |

Never compare against a moving branch. Baseline directories are read-only inputs and are excluded from Git.

## Execution order

1. Inventory every observable baseline capability and known defect.
2. Freeze protocol, binary, crypto, state, CLI, UI, platform, and failure fixtures.
3. Build the original-versus-Rust differential harness.
4. Complete contract and tool-selection pilots.
5. Prove the unchanged-client Codex vertical slice.
6. Expand by the dependency-ready task DAG.
7. Qualify every platform and mixed-version path.
8. Pass milestone and final adversarial review gates.

## Evidence contract

Every scenario retains raw and normalized outputs. Normalization is limited to documented generated IDs, wall-clock values, and temporary paths. Behavior, ordering, missing versus null semantics, errors, frames, state transitions, visuals, assertions, fixture counts, and executed-test counts are never normalized.

Tracked evidence manifests live under `evidence/`. Large raw captures live under `evidence/raw/` and remain untracked. Each manifest records its raw artifact digest and reproducible command.

## Safety

- Never start, stop, or restart the production daemon on port `6767`.
- Use disposable homes, ports, repositories, databases, accounts, credentials, and keys.
- Do not mutate production Hub, relay, provider, billing, deployment, release, or store state.
- Run long-lived services in named tmux sessions and stop exact identities only.
- Run targeted local tests. Full matrices belong in CI.
- Preserve all four baseline checkouts without edits.

## Routing and ownership

The lead owns shared contracts, the root Cargo workspace, cross-cutting schemas, the task ledger, integration, and shared evidence. Writers use isolated worktrees, branches, and exclusive paths. Every writer is explicitly assigned `gpt-5.6-sol` at medium effort unless a recorded blocker requires Sol high or xhigh. Astra is read-only and limited to adversarial review of frozen completed commits.

The Orca orchestration runtime was unavailable at execution start after one approved launch attempt. Runner-native goal and worker metadata are retained as routing evidence. This limitation does not prove an unobserved model assignment.

## Task states

Tasks move through `ready`, `implementing`, `verifying`, `reviewing`, `integrating`, and `done`. A blocked task names its exact unmet dependency and evidence. The durable ledger is [`porting/tasks.json`](porting/tasks.json).

## Current boundary

Phase 1 inventories are in progress. No parity milestone is complete. No runtime compatibility exception is accepted.

