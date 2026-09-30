# Spocky branding inventory

Decision: Stevie selected Spocky on 2026-09-30 after disclosure of existing
`spocky.ai` overlap. This inventory controls `BRAND-SPOCKY-001`.

## Owned implementation

Rename now at the serialized boundary:

- all 14 Cargo package and crate directory names from `paseo-*` to `spocky-*`;
- Rust crate imports from `paseo_*` to `spocky_*`;
- owned binary names, source filenames, workspace dependencies, scripts, and
  current task commands;
- owned product display strings, app names, help text, test fixtures, and new
  disposable artifact names;
- new product-facing app, CLI, service, package, and state identity.

## Compatibility contract

Preserve until a separately tested migration proves old and new behavior:

- `PASEO_*` environment inputs and their existing error text;
- `paseo://` deep links;
- `.paseo-hub.lock` and existing state or configuration names;
- wire discriminants such as `paseo_frame` and `paseo_close`;
- `X-Paseo-*` headers, protocol names, crypto contexts, cookies, auth keys, and
  storage keys;
- `@getpaseo/*` plugin imports and manifest `paseo` requirement fields;
- legacy app bundle, executable, identifier, and updater inputs used by
  original-versus-candidate compatibility fixtures.

New Spocky aliases require explicit old-state, mixed-version, and rollback tests.
No user state moves or deletions are authorized.

## Upstream attribution and provenance

Preserve exact names:

- immutable Paseo, Hub, relay, and importer repository URLs and commit hashes;
- `getpaseo` package and source imports used by pinned baselines;
- baseline labels, checkout paths, license and copyright notices;
- statements describing the source product or observed baseline behavior.

## Historical evidence

Preserve exact content:

- raw captures, hashes, screenshots, logs, frozen fixtures, and digests;
- commands already executed and commit-linked result descriptions;
- baseline email, UI, protocol, package, and error text retained as comparison
  evidence;
- historical worktree, session, artifact, and temporary-path observations.

Branded evidence uses new paths and hashes. Original evidence is never rewritten.

## Physical orchestration paths

Keep stable during the active goal:

- `/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust`;
- `/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust-worktrees/*`;
- the reference checkout, current worktrees, tmux names, runner threads, and
  native goal ID.

Repository or parent-directory relocation is a separate final coordinated task.

## Audited retained patterns

| Retained pattern | Locations | Class | Reason |
|---|---|---|---|
| `PASEO_*` | Rust drivers, tests, capture scripts | Compatibility | Existing automation and hosts provide these inputs |
| `paseo://` | browser and native runtime contracts | Compatibility | Existing deep links must continue to open |
| `paseo_frame`, `paseo_close`, `X-Paseo-*` | plugin and Hub protocols | Compatibility | Mixed-version peers require exact wire names |
| `paseo_session`, `paseo_pk_`, `/api/auth/paseo/*` | Hub auth | Compatibility | Cookies, credentials, and routes are externally addressed |
| `.paseo-hub.lock`, `paseo_hub_*`, `paseo:*` | Hub storage | Compatibility | Existing state must reopen without deletion |
| `paseo-plugin.json`, `requirements.paseo`, `@getpaseo/*` | plugin runtime | Compatibility | Existing third-party plugins use this manifest API |
| `Paseo.app`, `sh.paseo.desktop`, `paseo-*-package-v1` | delivery pilot | Compatibility | Frozen updater and rollback scenarios exercise legacy packages |
| `paseo@<commit>`, `.baselines/paseo-runtime` | differential harness | Provenance | Labels identify the immutable source baseline |
| `b"Paseo"`, workspace named `Paseo` | crypto and UI fixtures | Historical evidence | Exact frozen payloads remain comparison inputs |
| `getpaseo/paseo`, reference paths and commits | docs and tooling | Provenance | Source attribution and physical checkouts stay exact |
| `paseo-rust`, `paseo-rust-worktrees` | absolute paths | Physical path | Active goal and runner metadata depend on them |
| original raw captures and logs | `evidence/raw/**` | Historical evidence | Hashes and observations are immutable |

`BASELINE_LOGO_PATH` remains rendered only to preserve original visual geometry.
Spocky artwork requires separate approval and branded visual evidence before
release. It is not a product name or compatibility identifier.

## Verification

- `cargo metadata --no-deps --format-version 1` lists only intended
  `spocky-*` owned packages;
- every `spocky-*` package resolves after directory and dependency renames;
- targeted tests for affected packages preserve existing counts;
- every remaining `Paseo`, `paseo`, or `PASEO` reference matches one category
  above;
- no frozen capture, baseline checkout, digest, or physical orchestration path
  changes during the rename.

Verified at `4cce774259c974fb44ae78520488938a793ed5f8`: Cargo metadata lists 14
`spocky-*` packages, workspace check and clippy pass, 74 per-crate targeted tests
pass, and 23 post-audit targeted tests pass.
