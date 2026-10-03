//! Shared harness for differentials against the pinned Paseo build.
//!
//! `SPOCKY_PINNED_NODE` names the Node 22.20.0 binary and `SPOCKY_PASEO_DIST`
//! the pinned server dist, either `packages/server/dist/server` or its parent
//! `packages/server/dist`. Without both, a differential FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly). The Node binary must report `v22.20.0`.
//! Every module a test imports, protocol modules included, is checked against
//! its SHA-256 first, so a different build fails instead of passing.

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

/// The pinned `packages/protocol/dist` modules the terminal modules import,
/// relative to that directory, with their SHA-256.
pub const PINNED_PROTOCOL_MODULES: &[(&str, &str)] = &[
    (
        "terminal-input-mode.js",
        "44181dee20a6b937de7ddcaf6bcbbd3d7fd035bad7863011ca5b9b57cbadca42",
    ),
    (
        "terminal-snapshot.js",
        "288b8e30e97e0d4ed489d9ef96cb3c9b42f1987b1d6b8514083600a9a886dc25",
    ),
    (
        "binary-frames/terminal.js",
        "b06cc568118e46345e29bf426f5223cef846af2435eadbfee99e1ba660baebb7",
    ),
    (
        "binary-frames/demux.js",
        "0e960d177cc2133916247d713cb6064a751391c76bb626b523f1726916ea1aad",
    ),
    (
        "binary-frames/file-transfer.js",
        "4bf534d8a0b7de9b74ddf29ee8f105cabef2e68ea47fb94466412d9214ef5387",
    ),
    (
        "messages.js",
        "bd22155340099ad027b9daa670139c91ab9cde626662526e0563077956b6cbe1",
    ),
    (
        "binary-frames/index.js",
        "3af06230bf356743235f87317f388d9205a83dfb736be02ddb53b408347c1f58",
    ),
    (
        "terminal-activity.js",
        "27dedd07115476c8c68601bd3036b87f5591bb353e04c2baba7de0b56554a539",
    ),
];

/// `dist/server` modules the terminal worker imports, relative to
/// `dist/server`, with their SHA-256.
pub const PINNED_SERVER_MODULES: &[(&str, &str)] = &[
    (
        "terminal/terminal-manager.js",
        "8c821246d9ffdb49e7b797a5d20ca5a409ccb3062b902c49d9f8dcf7a1f69e1a",
    ),
    (
        "terminal/activity/terminal-activity-tracker.js",
        "2af0cec69b58dd354ffcdf3073b2d919ace74f9b4e9c5d1149c281490880134e",
    ),
    (
        "server/paseo-env.js",
        "376ef21a79563a728c048821ae0ab7abbd70e103024bcf6df421887dcff71609",
    ),
    (
        "server/path-utils.js",
        "d84b5eca7ca19d5134b4e94eb6d909e9531fb8993ffbdfdfd70a9cd7e2c3595a",
    ),
    (
        "server/private-files.js",
        "2dcbf8742613352d3346c692f0d77173981dffa2a7d2ea237ef067af8b1004d8",
    ),
    (
        "executable-resolution/executable-resolution.js",
        "660b90fe267769302e06645e33475cf7bb6ec9b3272ca901540ce4fbe5eb2af2",
    ),
];

/// node-pty 1.2.0-beta.15 under `packages/server/node_modules/node-pty`.
pub const PINNED_NODE_PTY_FILES: &[(&str, &str)] = &[
    (
        "package.json",
        "f4a19bbc7cf4c2e35c081e6a18a3d11d21c54c2f3cc25bfe48142742a781b71d",
    ),
    (
        "lib/unixTerminal.js",
        "63b605fb25c4e6d237493cd1799b01a8d880d89c78850108aae1853d04b4cdf0",
    ),
    (
        "lib/terminal.js",
        "771f5388103c3eb7bc698b589f41c08f06044d96de609eec45c9a79e4ee4fc00",
    ),
    (
        "lib/utils.js",
        "807bc62c8b702377d591136862309718ba48fb2b9eca54f4b25fbe43838c8fc6",
    ),
];

/// `strip-ansi` 7.1.2 and `ansi-regex` 6.2.2 under
/// `packages/server/node_modules`, which `terminal-capture.js` calls.
pub const PINNED_STRIP_ANSI_FILES: &[(&str, &str)] = &[
    (
        "strip-ansi/index.js",
        "c5bb23b3ca69e97ddefdb76724b1a7936ac18b5e47c3fe3c5391969d6e6d06f8",
    ),
    (
        "ansi-regex/index.js",
        "705ea426fac3c94d1290359e7d5a9d5dc53762c6ddee16f4852ba6aca47442cc",
    ),
];

/// The macOS x64 native pieces of node-pty; other platforms ship their own.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const PINNED_NODE_PTY_NATIVE: &[(&str, &str)] = &[
    (
        "prebuilds/darwin-x64/pty.node",
        "811f4a357e260fc0e520c250a3de5990483761d82afd635effbdf4b793b8a0e9",
    ),
    (
        "prebuilds/darwin-x64/spawn-helper",
        "a3bed36ae3ed83b2ac2fce475bd578c51f5b536041f0b2dfe116987f565e9758",
    ),
];
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub const PINNED_NODE_PTY_NATIVE: &[(&str, &str)] = &[];

pub struct Pinned {
    pub node: PathBuf,
    /// The pinned `dist/server/terminal` directory.
    pub terminal_dir: PathBuf,
    /// The pinned `packages/protocol/dist` directory.
    pub protocol_dir: PathBuf,
}

/// `packages/protocol/dist` beside the server package of `terminal_dir`.
pub fn protocol_dir(terminal_dir: &Path) -> PathBuf {
    terminal_dir.join("../../../../protocol/dist")
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
    let version = Command::new(&node)
        .arg("--version")
        .output()
        .expect("run pinned node --version");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.20.0",
        "SPOCKY_PINNED_NODE must be the pinned node"
    );
    let direct = dist.join("terminal");
    let terminal_dir = if direct.is_dir() {
        direct
    } else {
        dist.join("server/terminal")
    };
    let protocol_dir = protocol_dir(&terminal_dir);
    Some(Pinned {
        node,
        terminal_dir,
        protocol_dir,
    })
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

/// Fails unless every pinned module has its digest: the terminal and server
/// modules, the protocol modules they import, and node-pty.
pub fn assert_pinned_modules(terminal_dir: &Path) {
    let server_dir = terminal_dir.join("..");
    let protocol_dir = protocol_dir(terminal_dir);
    let node_pty_dir = terminal_dir.join("../../../node_modules/node-pty");
    let modules_dir = terminal_dir.join("../../../node_modules");
    let tables: [(&Path, &[(&str, &str)]); 6] = [
        (&modules_dir, PINNED_STRIP_ANSI_FILES),
        (terminal_dir, PINNED_TERMINAL_MODULES),
        (&server_dir, PINNED_SERVER_MODULES),
        (&protocol_dir, PINNED_PROTOCOL_MODULES),
        (&node_pty_dir, PINNED_NODE_PTY_FILES),
        (&node_pty_dir, PINNED_NODE_PTY_NATIVE),
    ];
    for (dir, modules) in tables {
        for (path, expected) in modules {
            let bytes = std::fs::read(dir.join(path))
                .unwrap_or_else(|error| panic!("pinned module {path}: {error}"));
            assert_eq!(
                &sha256_hex(&bytes),
                expected,
                "{path} is not the pinned build"
            );
        }
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

/// Runs a Node script file on the pinned Node with a bounded wall clock and
/// returns its output.
pub fn run_node_file(
    pinned: &Pinned,
    script: &Path,
    args: &[&std::ffi::OsStr],
) -> std::process::Output {
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&pinned.node)
        .arg(script)
        .args(args)
        .output()
        .expect("run pinned node script")
}
