//! The scoped `client-heartbeat-pong` transform (g4-retry only).
//!
//! The pinned client pings the daemon on a 10 s liveness timer and the
//! daemon answers with a bare `{"type":"pong"}`. How many of those arrive,
//! and between which other frames, follows the wall clock, so two runs of the
//! same daemon disagree (g4-retry self-check 20261003T162836Z). The probe
//! records every frame; this transform removes the lines of a step's stdout
//! that are exactly the bare pong, and counts them per side. A pong with any
//! other text, and every other frame, stays.

use crate::side::SideRun;

/// Id of the heartbeat pong transform.
pub const HEARTBEAT_TRANSFORM: &str = "client-heartbeat-pong";

/// The only gate this transform applies to.
pub const HEARTBEAT_GATE: &str = "g4-retry";

/// The one frame text that is removed, compared whole.
pub const BARE_PONG: &str = r#"{"type":"pong"}"#;

/// The side with the bare pongs removed from each step's stdout, and each
/// step's `step-<nn>-<name>/stdout` name with the count removed (0 included).
#[must_use]
pub fn without_heartbeat_pongs(side: &SideRun) -> (SideRun, Vec<(String, usize)>) {
    let mut side = side.clone();
    let mut removed = Vec::new();
    for (index, step) in side.steps.iter_mut().enumerate() {
        let text = String::from_utf8_lossy(&step.stdout).into_owned();
        let kept: Vec<&str> = text.split('\n').filter(|line| *line != BARE_PONG).collect();
        let count = text.split('\n').count() - kept.len();
        if count > 0 {
            step.stdout = kept.join("\n").into_bytes();
        }
        removed.push((format!("step-{:02}-{}/stdout", index + 1, step.name), count));
    }
    (side, removed)
}
