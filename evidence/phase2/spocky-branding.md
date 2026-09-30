# Spocky branding checkpoint

Task: `BRAND-SPOCKY-001`

Implementation commit: `4cce774259c974fb44ae78520488938a793ed5f8`

Safe boundary: browser exact-parity and embedded-schema checkpoints integrated;
selected-plugin expansion paused before cross-cutting edits.

## Result

- Cargo metadata resolves 14 packages, all named `spocky-*`.
- Owned Rust crate imports use `spocky_*`.
- Owned binaries, help text, UI title, Hub email copy, runtime notifications,
  scripts, and current task commands use Spocky.
- Legacy wire, auth, state, deep-link, package, and baseline identities remain
  unchanged under the audited compatibility inventory.
- Reference checkouts, frozen fixtures, raw evidence, and physical repository
  paths remain unchanged.

## Verification

| Gate | Result |
|---|---|
| `cargo metadata --no-deps --format-version 1` | 14 Spocky packages |
| `cargo fmt --all -- --check` | pass |
| `cargo check --workspace --all-targets` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| one targeted suite per renamed crate | 74 passed, 0 failed |
| post-audit affected tests | 23 passed, 0 failed |
| script syntax and capture contract tests | pass |

Metadata summary and digest are in
`evidence/raw/phase2/spocky-cargo-metadata-summary.json`. Original baseline
evidence was not edited.
