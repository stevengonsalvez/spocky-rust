//! Stable execution identity, byte-for-byte the baseline `durableExecutionId`.

use std::fmt::Write;

use sha2::{Digest, Sha256};

/// Derives the execution ID from trigger run, configuration revision, trigger name and step run.
///
/// The same inputs always give the same ID, so a crash between reserving an execution and handing
/// it to a daemon recovers the identical record instead of minting a second one.
#[must_use]
pub fn durable_execution_id(
    trigger_run_id: &str,
    configuration_revision_id: &str,
    trigger_name: &str,
    workflow_step_run_id: Option<&str>,
) -> String {
    let digest = Sha256::new()
        .chain_update(b"paseo-durable-execution-v1\0")
        .chain_update(trigger_run_id)
        .chain_update([0])
        .chain_update(configuration_revision_id)
        .chain_update([0])
        .chain_update(trigger_name)
        .chain_update([0])
        .chain_update(workflow_step_run_id.unwrap_or(""))
        .finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
