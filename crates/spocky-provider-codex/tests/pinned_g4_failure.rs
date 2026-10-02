//! G4 upstream failure against the pinned Paseo build. The recorded session
//! is real codex 0.159.0 whose model endpoint answered HTTP 500
//! (`tests/fixtures/g4_upstream_500.json`); the Rust provider and the pinned
//! `CodexAppServerAgentClient` get the same bytes and must emit the same
//! session events and send the same lines to Codex.

mod support;

use support::replay_differential;

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn upstream_500_matches_pinned() {
    replay_differential("g4_upstream_500.json", "upstream_500", "Say hello", "none");
}
