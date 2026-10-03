# Phase 4 workspace labels differential evidence

Lane `p4_daemon_services`, capability `DWLABEL-001`, 2026-10-03. Status:
the Rust port matches the pinned build on all 389 trace lines of 32
scenarios, on macOS x64 only.

## What is ported

`crates/spocky-workspace-labels` ports `server/workspace-labels` (index,
`internal/catalog-store`, `internal/sequence`, `internal/service`) at
5de45e2: the catalog file `projects/workspace-labels.json`, the crash journal
`projects/workspace-labels.transaction.json`, assignment, edit, delete and
count, the generation-tagged change journal with catch-up, and the injected
`writeCatalog`, `writeTransaction`, `removeTransaction` and `journalLimit`
options. `spocky-store` gains what `FileBackedWorkspaceRegistry.commitWorkspaceLabelMutation`
and `blockAllMutationsUntilRestart` need: `FileRegistry::commit_staged`, a
blocked flag that fails every mutation with the baseline's text, and the
`writeRecords` option.

## Command

```sh
SPOCKY_BUILD_GATE=/usr/bin/env scripts/phase4/daemon-svc-labels-differential.sh
```

run inside one build-gate slot (the gate wait was over an hour per call).
The script runs `cargo test`, `cargo clippy --all-targets -D warnings` and
`cargo fmt --check` for the crate with `SPOCKY_PINNED_NODE` (node 22.20.0,
digest checked by `scripts/phase3/pins.sh`) and `SPOCKY_PASEO_DIST` set and
`SPOCKY_ALLOW_SKIP` unset. It fails unless the worktree is clean outside
`logs/` and both normalized traces exist and are byte-identical. Clippy on
`spocky-store`, `spocky-session` and `spocky-message-receipts` (the direct
reverse dependents of the store change) is clean.

## How the comparison works

`tests/labels_scenarios.json` is one script of 32 scenarios.
`tests/labels_driver.mjs` runs it on the pinned build's built modules;
`tests/labels_differential.rs` runs it on the Rust service. Both print one
line per step with the step's result or error (`code`, `message`), and a
`dump` step prints every registry record, every workspace publication, every
catalog change event and every file under the scenario home. Raw lines must
be equal after normalization.

Normalization replaces only: the disposable root path with `<root>`; each
`randomUUID()` with `<uuid:N>`, N counting distinct values in order of first
appearance so a reused generation and a rotated one stay distinguishable; the
pid and `Date.now()` of a `writeFileAtomic` temp name with `<pid>` and `<ms>`;
and a generated `toISOString()` stamp with `<now>`. Timestamps the script
writes itself (`2026-08-14T`, `2099-`) are never masked. Results, error text,
ordering and file bytes are not normalized. The home dump lists every file,
dot-files included, so a leftover temp file would show. Two further tests
cover the normalizer.

Scenarios: the 25 cases of the baseline `index.test.ts` (normalization,
catch-up, name cycles, edit and delete, one commit for name and colour,
collisions, assignment and rollback both failing, catalog, journal and
workspace write failures before the commit point, lost acknowledgements at the
prepared, catalog, workspace and committed writes, uncertain outcome and
freeze, cleanup failures, listener and subscriber failures, bounded journal),
plus corrupt catalog and journal files (JSON and zod error text), recovery of a
prepared and of a stale committed journal, request edge cases (empty and
Unicode-whitespace names, missing and archived workspaces, unassign of an
unknown label), rename and delete rewrites in registry order, and reads while
storage is frozen.

## Raw evidence

Inputs and digests of the pinned modules are in `inputs.txt` of each run
(`evidence/raw/phase4/labels-<utc>/`, untracked):

| module | sha256 |
|---|---|
| server/workspace-labels/index.js | 2038e94cff0576cb3a0f7519bc003e1e12182add621f0feb8e26488156355f8a |
| server/workspace-labels/internal/catalog-store.js | 35f571009cb2ae1c687f4f875ef03a5b9c4b8103b56ecba7e73a0402ca0526eb |
| server/workspace-labels/internal/sequence.js | bd2910debb9ae0ce21aeeec4a2d069dad5fd96f8f8a8e2c8af650498089ac972 |
| server/workspace-labels/internal/service.js | d1a6fdc1ea0ea8cc9e40196b8185fcd1ab2836df7a46b61b249e0e4228f57c05 |
| server/workspace-registry.js | 30578109d7388b6d0cd0b76a1f1e5e19d1711a6ce13eedcdcb9fbc9eebcb9164 |
| server/atomic-file.js | 835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25 |

## Result

Run `labels-20261003T195210Z` at commit `d6e37471dbf3193552004ee01396165ac780943b`:
exit 0, `labels-20261003T195210Z passed`. Tests: 2 unit (`names`), 2
differential (`label_scenarios_match_the_pinned_build` and the normalizer
test), 0 doc tests; 4 passed, 0 failed, 0 ignored. Clippy and fmt clean. The
normalized traces are byte-identical (sha256
`55c8fa7585b6fe209637300abd8f4115ae26698e82b281dd3987db7023468e5f` for both
sides); the raw traces differ only in the root path, the generation uuids and
generated timestamps (node raw `1d538fd6...`, Rust raw `46be71cb...`).

Per-commit check: every commit from `7c76e71e` to `d6e37471` passes
`cargo clippy --all-targets -D warnings` and `cargo fmt --check` on its own
(the two `spocky-store` commits also on `spocky-session` and
`spocky-message-receipts`). Empty stub modules fail `rustfmt --check`, so the
manifest commit's stubs hold a one-line doc comment.

## Known gaps

- The service is synchronous. The baseline's `exclusive` promise queue becomes
  a mutex held for the whole operation; concurrent interleavings the node
  event loop produces (the "preserves concurrent workspace fields" case) are
  compared as a title update followed by an assignment, not as a race.
- The registry has no mutation listeners in Rust. A label commit reports its
  changed workspaces through `WorkspacePublisher`; the daemon wiring maps that
  to `workspace_update` messages. The scenario "workspace listener fails" is
  therefore only a no-op check on the Rust side (a Rust publisher cannot throw).
- Wire gaps left: label commits do not publish `workspace_update` (the daemon
  has no workspace subscription path yet); `subscription.release.request` is
  still unrouted, so the wire run does not release a subscription from the
  client; legacy-socket subscription rules are covered by unit tests only.
- `tests/labels_differential.rs` injects failures from the script, not from
  real disk faults, as the baseline test does.

## Wire differential (server_info and every label RPC)

`crates/spocky-daemon-app/tests/differential/labels-differential.sh`, run at
`4494c148` (debug `spocky-daemon`). A pinned client creates a workspace and
drives list (empty, caught-up cursor, expired cursor), an owned subscription,
assignments (new, existing with another colour, second, unassign of an unknown
label, empty name, missing workspace), updates (rename and recolour, collision,
unknown, no-op, empty name), delete inspect and delete (known and unknown).
Raw frame text comes from the client's `handleJsonPayload` hook, no key
reordering.

- 25 frames per side, byte-identical after masking (root path, uuids numbered
  by first appearance, timestamps). The first frame is `server_info` with
  `features.workspaceLabels:true`.
- Outcomes per step (ok or error code and text) match for all 20 steps.
- Persisted `workspace-labels.json` and `workspaces.json` are byte-identical
  after masking; no transaction journal is left behind.
- Masked frames sha256 `e51af580...` on both sides.

Per-commit check: the ten commits `3b19a5a6..4494c148` each pass
`cargo clippy --all-targets -D warnings` and `cargo fmt --check` on
`spocky-contracts`, `spocky-daemon` and `spocky-daemon-app`.
Tip tests pass for the same three crates (`--lib --tests`), which includes the
hello differential and the G2 differential exemption removal.

Shared lines touched: contracts `lib.rs` (mod line), `session.rs` (two untagged
variants, five deserialize arms); daemon-app `request.rs`, `authorization.rs`,
`session.rs` (Services.labels, validate hook, detach hook, route arm), `lib.rs`,
`Cargo.toml`, bin `spocky-daemon.rs`; daemon `daemon.rs`
(`workspace_labels: true`). The five label requests stay `Unmodeled` in the
generated `zod_schemas.rs`; `workspace_labels::check_session_message` judges
them.
