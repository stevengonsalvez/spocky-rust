//! Shared harness for the daemon relay differential tests.
//!
//! An endpoint executes JSON operations and returns the entries each one produced.
//! [`NodeEndpoint`] drives the pinned TypeScript through
//! `scripts/phase4/relay-daemon-driver.mjs`; [`RustEndpoint`] drives the Rust port.
//! Entries render exactly as the driver's `JSON.stringify` does, so transcripts
//! compare as raw strings.
#![allow(dead_code, clippy::too_many_lines)]

mod entry;
mod rust;

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, channel},
    thread,
    time::Duration,
};

use serde_json::Value;

pub use rust::RustEndpoint;

pub const PASEO_COMMIT: &str = "5de45e208690b0efc51c59a585ae9729325a9204";
pub const NODE_VERSION: &str = "v22.20.0";

/// SHA-256 of every file the driver loads from the pinned commit and its lockfile install.
pub const PINNED_DIGESTS: [(&str, &str); 9] = [
    (
        "relay-transport.ts",
        "a117b5af8082c44434e020cd5955de131b629ea7fa128245882b6425adda870b",
    ),
    (
        "relay-runtime.ts",
        "d34ad4a26b405e145f60c2f9d3f9ad5b211a7b4c13857465bd71147950e7bd68",
    ),
    (
        "encrypted-relay-socket.ts",
        "a3b929ade066d2957c4d3558817c177254020c0d1e76bbcc665379be1295e223",
    ),
    (
        "physical-socket.ts",
        "b15a2818754bcb2b9264dca744c1ece97ee326e66d1f2764ea416e3f7fe87545",
    ),
    (
        "daemon-endpoints.ts",
        "d241e622ea036b333be521b651464173743db9ed5c1d556c4b90a2c82dd8fe20",
    ),
    (
        "ws/wrapper.mjs",
        "fe154662301fd558f935f9c217fccbe7a7dac02ff39e648a000589403ff27c7f",
    ),
    (
        "ws/package.json",
        "0e8b0104fec3e3b96704861c6ef3ad77c27add2cc016e49e111291b16b677f89",
    ),
    (
        "physical-socket.transpiled",
        "403d2136f05f21d080979760c739c5fded7ca9a8e45869ed0c5c1931bdc67c62",
    ),
    (
        "typescript/package.json",
        "822ef7ca6452205657b6288b066481ecf508bfbf43455d715cf7d3ec457561e6",
    ),
];

const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Locations of the pinned node binary, sources and dependencies.
pub struct Pinned {
    node: OsString,
    driver: PathBuf,
    paseo_root: PathBuf,
    node_modules: PathBuf,
}

/// The pinned inputs, or `None` when skipping was requested explicitly.
///
/// `SPOCKY_PINNED_NODE` names node 22.20.0. The sources come from `PASEO_REFERENCE_ROOT`,
/// else the `paseo-rewrite` sibling of the main checkout. Dependencies come from
/// `SPOCKY_PASEO_NODE_MODULES`, else the pinned build root of
/// `scripts/phase3/build-original.sh`. Every loaded file is digest-checked.
pub fn pinned() -> Option<Pinned> {
    let Some(node) = std::env::var_os("SPOCKY_PINNED_NODE") else {
        if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: daemon relay differential not run");
            return None;
        }
        panic!("set SPOCKY_PINNED_NODE (or SPOCKY_ALLOW_SKIP=1)");
    };
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let paseo_root = std::env::var_os("PASEO_REFERENCE_ROOT").map_or_else(
        || {
            let output = Command::new("git")
                .arg("-C")
                .arg(manifest)
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .output()
                .expect("run git");
            assert!(output.status.success(), "git rev-parse failed");
            let common = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
            common
                .parent()
                .and_then(Path::parent)
                .expect("main checkout has a parent")
                .join("paseo-rewrite")
        },
        PathBuf::from,
    );
    let node_modules = std::env::var_os("SPOCKY_PASEO_NODE_MODULES").map_or_else(
        || {
            PathBuf::from(format!(
                "/private/tmp/spocky-targets/p3_slice_harness/paseo-original-{PASEO_COMMIT}/node_modules"
            ))
        },
        PathBuf::from,
    );
    Some(Pinned {
        node,
        driver: manifest.join("../../scripts/phase4/relay-daemon-driver.mjs"),
        paseo_root,
        node_modules,
    })
}

/// One endpoint that executes operations.
pub trait Endpoint {
    fn op(&mut self, op: &Value) -> Vec<String>;
}

/// The pinned TypeScript in a node child process.
pub struct NodeEndpoint {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl NodeEndpoint {
    pub fn spawn(pinned: &Pinned) -> Self {
        let mut child = Command::new(&pinned.node)
            .arg(&pinned.driver)
            .arg(&pinned.paseo_root)
            .arg(&pinned.node_modules)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn pinned node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut endpoint = Self {
            child,
            stdin,
            lines,
        };
        let ready: Value =
            serde_json::from_str(&endpoint.next_line()).expect("driver ready line is JSON");
        assert_eq!(ready["ready"], true);
        assert_eq!(ready["node"], NODE_VERSION, "pinned node version");
        for (name, digest) in PINNED_DIGESTS {
            assert_eq!(ready["digests"][name], digest, "digest of pinned {name}");
        }
        endpoint
    }

    fn next_line(&mut self) -> String {
        match self.lines.recv_timeout(OPERATION_TIMEOUT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("node driver timed out"),
            Err(RecvTimeoutError::Disconnected) => panic!("node driver exited"),
        }
    }
}

impl Endpoint for NodeEndpoint {
    fn op(&mut self, op: &Value) -> Vec<String> {
        writeln!(self.stdin, "{op}").expect("write operation");
        self.stdin.flush().expect("flush operation");
        let mut entries = Vec::new();
        loop {
            let line = self.next_line();
            if line == "." {
                return entries;
            }
            entries.push(line);
        }
    }
}

impl Drop for NodeEndpoint {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An operation whose entries differ between the two endpoints.
pub struct Mismatch {
    pub op: Value,
    pub text: String,
}

/// Runs `ops` on both endpoints. Returns the shared transcript and every operation whose
/// entries differ, with both sides, so one run reports all divergences.
pub fn differential(
    node: &mut NodeEndpoint,
    rust: &mut RustEndpoint,
    ops: &[Value],
) -> (Vec<String>, Vec<Mismatch>) {
    let mut transcript = Vec::new();
    let mut mismatches = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let expected = node.op(op);
        let actual = rust.op(op);
        if expected != actual {
            mismatches.push(Mismatch {
                op: op.clone(),
                text: format!(
                    "operation {index} {op} differs\npinned: {expected:#?}\nrust:   {actual:#?}"
                ),
            });
        }
        transcript.push(format!("{op}"));
        transcript.extend(expected);
        transcript.push(".".to_owned());
    }
    (transcript, mismatches)
}
