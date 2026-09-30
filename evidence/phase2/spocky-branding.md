# Spocky branding checkpoint

Task: `BRAND-SPOCKY-001`

Implementation commit: `4cce774259c974fb44ae78520488938a793ed5f8`

Evidence harness commit: `c185395401cb29eba6afa14d07995967122d924c`

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
| branded browser desktop RMSE | 0, exact |
| branded browser mobile RMSE | 0, exact |
| original repeat stability | desktop 0, mobile 0 |

The branded browser capture reports `Spocky` in candidate accessibility text.
Its raw JSON SHA-256 is
`47c8d92eb68deb253fb44a19d96160098578e49731c65ab8bd6eff0e3d127669`.
Desktop image SHA-256 is
`597095777e1d610387667c732b7c08624e4f135a6064e1b1b739ec1342f4dc7d`;
mobile image SHA-256 is
`37ff2c272ad311efe1fc2e22df94ecb75af3a5f74a47b2ee6c7b356e58d99075`.
The capture remains Chromium-only and both original and candidate retain the
offline-reload failure.

Metadata summary and digest are in
`evidence/raw/phase2/spocky-cargo-metadata-summary.json`. Original baseline
evidence was not edited.
