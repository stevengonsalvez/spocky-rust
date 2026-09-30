# Spocky Rust Exact Parity

This repository rebuilds Paseo-owned implementation surfaces as Spocky in Rust
against four immutable source baselines. Behavior, compatibility, state,
visuals, interactions, accessibility, performance, packaging, failure recovery,
and supported platforms must match before feature work begins.

## Product identity amendment

Stevie selected **Spocky** as the rewrite product identity on 2026-09-30 after
being informed of existing `spocky.ai` overlap. This is an authorized branding
exception to text and visual parity, not legal clearance or permission to
publish, buy a domain, rename remotes, or create mascot assets.

Task `BRAND-SPOCKY-001` owns the coordinated rename. Its safe boundary is after
the active exact-browser and embedded-schema checkpoints integrate. Cross-cutting
writers pause there. The lead then serially renames owned crates, modules,
packages, binaries, scripts, current commands, UI strings, and help text from
`paseo-*` or `paseo_*` to `spocky-*` or `spocky_*`.

Every remaining Paseo name is classified before change:

| Class | Treatment |
|---|---|
| Owned implementation | Rename to Spocky at the serialized boundary |
| Compatibility contract | Inventory, migrate, and test before changing |
| Upstream attribution or provenance | Preserve exact Paseo name and source |
| Historical evidence | Preserve command, capture, digest, and observation |
| Physical orchestration path | Keep stable while workers and goal run |

New product-facing identity defaults to Spocky. Existing `PASEO_*` inputs,
legacy state, protocol and wire names, crypto contexts, deep links, cookies,
auth and storage keys remain compatible until an explicit migration proves old
and new behavior. Never move or delete user state for branding. Branded evidence
is captured separately from frozen original baselines.

## Immutable baselines

| System | Commit | Local source |
|---|---|---|
| Paseo | `5de45e208690b0efc51c59a585ae9729325a9204` | `PASEO_REFERENCE_ROOT` or sibling `../paseo-rewrite` |
| Hub | `28f6c78833065fd282f9064f92a9aa61875dd359` | `.baselines/hub` |
| Distributed relay | `3fc41c96c8c63f3a7109e832899cc57d473c4531` | `.baselines/relay` |
| Importer | `8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5` | `.baselines/import` |

Never compare against a moving branch. Baseline directories are read-only inputs and are excluded from Git.

## Provision the baselines

Set `PASEO_REFERENCE_ROOT` only when the Paseo reference is not the sibling `paseo-rewrite` checkout. Verify its commit before any comparison:

```sh
git -C "${PASEO_REFERENCE_ROOT:-../paseo-rewrite}" rev-parse HEAD
```

Provision the other pinned sources inside this repository, then detach each checkout at its immutable commit:

```sh
git clone https://github.com/getpaseo/hub.git .baselines/hub
git -C .baselines/hub checkout --detach 28f6c78833065fd282f9064f92a9aa61875dd359
git clone https://github.com/getpaseo/paseo-relay.git .baselines/relay
git -C .baselines/relay checkout --detach 3fc41c96c8c63f3a7109e832899cc57d473c4531
git clone https://github.com/getpaseo/import.git .baselines/import
git -C .baselines/import checkout --detach 8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5
cargo test -p paseo-baseline --test pinned_sources
```

If a repository URL changes, copy an existing checkout into the matching `.baselines/` path and detach it at the recorded commit. Never substitute a newer commit.

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

## Build, test, run, and rollback

- Build current pilot crates with `cargo build --workspace`. No parity-qualified Spocky service binary exists yet.
- Run only the targeted commands recorded in `porting/tasks.json`. Full platform matrices belong in CI after their jobs exist.
- Run future services with a disposable home, random non-6767 port, and named tmux session. Record the exact home, port, session, commit, and log path in evidence before launch.
- Stop only the recorded tmux session. Delete only its exact disposable home after evidence capture.
- Roll back by checking out the last signed verified checkpoint and restoring the scenario's disposable state snapshot. Never roll back a production instance during parity work.
- Cloudflare relay, Hub, delivery, installers, and update paths remain owned implementation scope. Production deployment is outside this execution session.

## Routing and ownership

The lead owns shared contracts, the root Cargo workspace, cross-cutting schemas, the task ledger, integration, and shared evidence. Writers use isolated worktrees, branches, and exclusive paths. Every writer is explicitly assigned `gpt-5.6-sol` at medium effort unless a recorded blocker requires Sol high or xhigh. Astra is read-only and limited to adversarial review of frozen completed commits.

The Orca orchestration runtime was unavailable at execution start after one approved launch attempt. Runner-native goal and worker metadata are retained as routing evidence. This limitation does not prove an unobserved model assignment.

## Task states

Tasks move through `ready`, `implementing`, `verifying`, `reviewing`, `integrating`, and `done`. A blocked task names its exact unmet dependency and evidence. The durable ledger is [`porting/tasks.json`](porting/tasks.json).

## Current boundary

Phase 1 inventory and architecture freeze passed paired adversarial review at signed commit `2fa22be761a0fbec42fd759df69bff37e248824c`. Phase 2 differential harness and pilots are implementing. Its first two frozen candidates were rejected by paired adversarial review; repairs and runtime evidence continue. Pinned browser visuals still differ, and required Hub, renderer, platform, plugin, relay, audio, native, and delivery cases remain incomplete. Phase 3 remains blocked. No runtime parity milestone is complete. No runtime compatibility exception is accepted.
