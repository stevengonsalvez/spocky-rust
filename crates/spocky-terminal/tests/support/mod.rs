//! Shared harness for differentials against the pinned Paseo build.
//!
//! `SPOCKY_PINNED_NODE` names the Node 22.20.0 binary and `SPOCKY_PASEO_DIST`
//! the pinned server dist, either `packages/server/dist/server` or its parent
//! `packages/server/dist`. Without both, a differential FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly). Every module a test imports is checked
//! against its SHA-256 first, so a different build fails instead of passing.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The pinned `dist/server/terminal` modules with their SHA-256.
pub const PINNED_TERMINAL_MODULES: &[(&str, &str)] = &[
    (
        "terminal-output-coalescer.js",
        "c4c81d79dbfde46ac90c3d9ce07c2c99b4537a81a455f186a942627c692d14ed",
    ),
    (
        "terminal-size-ownership.js",
        "c364c915e89eea6d7bebf04aebf89f9574859029bf4d9ad7823007d3d607b006",
    ),
    (
        "terminal-restore.js",
        "ddef6a83bef7330d21c8910c48a08656a7ec9a22cc0a7ed1d6b329374913c640",
    ),
    (
        "terminal.js",
        "44bd0fb2a6ff3f22cef6d93bedec6588bee4fbfb9efdd39fc9996db6cf06a30e",
    ),
    (
        "terminal-worker-process.js",
        "6c97e73f0a7399d7ccf40666a0214667113ac02688589883b8aa8584615e58cf",
    ),
    (
        "terminal-capture.js",
        "29f93395cd168b5663c5ffe4303172cb334be2517cc423db41b9d54ca1b7a164",
    ),
];

pub struct Pinned {
    pub node: PathBuf,
    /// The pinned `dist/server/terminal` directory.
    pub terminal_dir: PathBuf,
}

/// The pinned Node and terminal module directory, or `None` when the caller
/// must skip because `SPOCKY_ALLOW_SKIP=1`.
pub fn pinned(what: &str) -> Option<Pinned> {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (PathBuf::from(node), PathBuf::from(dist)),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: {what} not run");
            return None;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    let direct = dist.join("terminal");
    let terminal_dir = if direct.is_dir() {
        direct
    } else {
        dist.join("server/terminal")
    };
    Some(Pinned { node, terminal_dir })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Fails unless every pinned module under `terminal_dir` has its digest.
pub fn assert_pinned_modules(terminal_dir: &Path) {
    for (path, expected) in PINNED_TERMINAL_MODULES {
        let bytes = std::fs::read(terminal_dir.join(path))
            .unwrap_or_else(|error| panic!("pinned module {path}: {error}"));
        assert_eq!(
            &sha256_hex(&bytes),
            expected,
            "{path} is not the pinned build"
        );
    }
}

/// Runs an ES module script on the pinned Node with a bounded wall clock and
/// returns its stdout. `args` follow the terminal module directory.
pub fn run_node(pinned: &Pinned, script: &str, args: &[&str]) -> String {
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&pinned.node)
        .args(["--input-type=module", "-e", script])
        .arg(&pinned.terminal_dir)
        .args(args)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 stdout")
}
