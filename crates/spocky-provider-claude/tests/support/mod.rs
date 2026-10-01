//! Shared support for the differential tests: runs a script under the pinned
//! Node against the pinned Paseo build, after checking the digest of every
//! dist module the script loads.
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

/// Runs `script` as an ES module with `dist` and `args` in `process.argv`,
/// bounded by `gtimeout`, and returns its stdout.
pub fn run_node(node: &OsString, dist: &Path, script: &str, args: &[String]) -> String {
    let version = Command::new(node)
        .arg("--version")
        .output()
        .expect("pinned node --version");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        PINNED_NODE_VERSION
    );
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(node)
        .args(["--input-type=module", "-e", script])
        .arg(dist)
        .args(args)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("node stdout is UTF-8")
}
