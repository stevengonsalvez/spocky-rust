//! G4 stream retry against the pinned Paseo build. The recorded session is
//! real codex 0.159.0 whose model endpoint dropped the response stream twice
//! before it answered (`tests/fixtures/g4_stream_retry.json`): Codex reports
//! each drop as an `error` notification with `willRetry: true`, then the turn
//! completes. The Rust provider and the pinned `CodexAppServerAgentClient`
//! get the same bytes and must emit the same session events and send the same
//! lines to Codex.

mod support;

use support::replay_differential;

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn stream_retry_matches_pinned() {
    replay_differential("g4_stream_retry.json", "stream_retry", "Say hello", "none");
}
