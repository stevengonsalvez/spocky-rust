//! G2 approval differential against the pinned Paseo build. Each scenario
//! replays one recorded `codex app-server` stdio session (real codex 0.159.0,
//! `tests/fixtures/g2_approvals.json`) to the Rust provider and to the pinned
//! `CodexAppServerAgentClient`, drives the same approval action on both, and
//! requires identical session events, pending permissions before and after,
//! and identical client-to-Codex JSON lines (the approval decisions and
//! `turn/interrupt` included). Nothing is normalized: both sides read the
//! same replayed bytes.

mod support;

use support::replay_differential;

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn allowed_command_matches_pinned() {
    replay_differential("g2_approvals.json", "allow", "Run echo", "allow");
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn denied_command_matches_pinned() {
    replay_differential("g2_approvals.json", "deny", "Run echo", "deny");
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn deny_with_interrupt_matches_pinned() {
    replay_differential(
        "g2_approvals.json",
        "deny_interrupt",
        "Run echo",
        "deny_interrupt",
    );
}

#[test]
#[ignore = "drives the pinned Paseo client; run with --include-ignored"]
fn interrupt_while_approval_pending_matches_pinned() {
    replay_differential(
        "g2_approvals.json",
        "interrupt_pending",
        "Run echo",
        "interrupt",
    );
}
