//! Shared support for the differential tests: runs a script under the pinned
//! Node against the pinned Paseo build, after checking the digest of every
//! dist module the script loads.
//!
//! The one copy: `spocky-contracts` tests include it with
//! `#[path = "support/pinned_node.rs"]`, and `spocky-provider-claude`'s
//! `tests/support/mod.rs` re-exports it through a path include.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST`; without them a test
//! FAILS unless `SPOCKY_ALLOW_SKIP=1` (exactly).

// Each test binary uses a different subset of this module.
#![allow(dead_code)]

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// The pinned Node and dist, or `None` when the test may skip.
pub fn pinned() -> Option<(OsString, PathBuf)> {
    match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => Some((node, PathBuf::from(dist))),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: differential not run");
            None
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Fails unless every `(path, sha256)` under `dist` is the pinned build.
pub fn assert_pinned_modules(dist: &Path, modules: &[(&str, &str)]) {
    for (path, expected) in modules {
        let bytes = std::fs::read(dist.join(path)).expect("pinned module");
        assert_eq!(
            sha256_hex(&bytes),
            *expected,
            "{path} is not the pinned build"
        );
    }
}

/// Node 22.20.0 from Paseo `.tool-versions`.
const PINNED_NODE_VERSION: &str = "v22.20.0";

/// `gtimeout` where installed, else `timeout`.
pub fn timeout_command() -> &'static str {
    if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    }
}

/// Runs `script` as an ES module with `dist` and `args` in `process.argv`,
/// bounded by `gtimeout`, and returns its stdout.
pub fn run_node(node: &OsString, dist: &Path, script: &str, args: &[String]) -> String {
    run_node_with_env(node, dist, script, args, &[])
}

/// [`run_node`] with extra environment variables (for example `TZ`).
pub fn run_node_with_env(
    node: &OsString,
    dist: &Path,
    script: &str,
    args: &[String],
    env: &[(&str, &str)],
) -> String {
    let timeout = timeout_command();
    let version = Command::new(timeout)
        .args(["--kill-after=5", "30"])
        .arg(node)
        .arg("--version")
        .output()
        .expect("pinned node --version");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        PINNED_NODE_VERSION
    );
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(node)
        .args(["--input-type=module", "-e", script])
        .arg(dist)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("node stdout is UTF-8")
}

/// Runs the test `test_name` of the current test binary as a child with
/// `TZ` set to `tz` and `child_env` present, bounded by `gtimeout`, and
/// returns its stdout lines that start with `prefix`. The Rust side of a
/// time-zone differential runs this way so `TZ` reaches its zone lookup the
/// way it reaches V8's.
pub fn run_self_child(test_name: &str, child_env: &str, tz: &str, prefix: &str) -> Vec<String> {
    let output = Command::new(timeout_command())
        .args(["--kill-after=5", "120"])
        .arg(std::env::current_exe().expect("test exe"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(child_env, "1")
        .env("TZ", tz)
        .output()
        .expect("run the child");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("child stdout is UTF-8")
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}
