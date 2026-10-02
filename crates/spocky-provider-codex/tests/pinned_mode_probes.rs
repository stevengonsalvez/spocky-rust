//! `resolveDefaultModeId` against the pinned Paseo build. A recording shim
//! stands in for Codex (it logs its argv and prints `codex-cli 0.159.0`); the
//! Rust provider and the pinned `CodexAppServerAgentClient` make the same
//! four calls and must return the same modes and make the same `--version`
//! probes per call: the memo without a signal, a fresh probe with one, and an
//! aborted signal that stops after the launch prefix probe and raises.

mod support;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spocky_provider_codex::{CodexProvider, ProviderCommand, ProviderRuntimeSettings};
use support::DisposableRoot;

/// A Codex stand-in that appends its argv to the log and prints a version.
fn recording_shim(root: &DisposableRoot) -> String {
    let shim = root.join("codex-shim");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho 'codex-cli 0.159.0'\n",
            root.join("argv.log").display()
        ),
    )
    .expect("write shim");
    Command::new("chmod")
        .arg("+x")
        .arg(&shim)
        .status()
        .expect("chmod shim");
    shim.to_string_lossy().into_owned()
}

/// The argv log split at the `# <call>` markers.
fn probes_per_call(log: &Path) -> Vec<(String, usize)> {
    let mut calls: Vec<(String, usize)> = Vec::new();
    for line in std::fs::read_to_string(log).expect("argv log").lines() {
        match line.strip_prefix("# ") {
            Some(call) => calls.push((call.to_owned(), 0)),
            None => calls.last_mut().expect("marker first").1 += 1,
        }
    }
    calls
}

fn mark(log: &Path, call: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .expect("argv log");
    writeln!(file, "# {call}").expect("marker");
}

fn rust_results(shim: &str, root: &DisposableRoot) -> String {
    let log = root.join("argv.log");
    let provider = CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: vec![shim.to_owned()],
            }),
            env: None,
        }),
        None,
        vec![("PATH".into(), std::env::var_os("PATH").unwrap_or_default())],
    );
    let mut results: Vec<Value> = Vec::new();
    let mut step = |call: &str, abort: Option<&(dyn Fn() -> bool + Sync)>| {
        mark(&log, call);
        results.push(match provider.resolve_default_mode_id(abort) {
            Ok(mode) => json!({"call": call, "mode": mode}),
            Err(_) => json!({"call": call, "aborted": true}),
        });
    };
    step("no-signal", None);
    step("no-signal-again", None);
    step("signal", Some(&|| false));
    step("aborted", Some(&|| true));
    serde_json::to_string(&results).unwrap()
}

fn pinned_results(shim: &str, root: &DisposableRoot) -> String {
    let pinned = support::pinned_paseo();
    let mut child = Command::new(&pinned.node)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/pinned_mode_probes.mjs"))
        .arg(&pinned.module)
        .arg(shim)
        .arg(root.join("argv.log"))
        .arg(root.project())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pinned node");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("wait for pinned node").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pinned mode probes exceeded 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("pinned output");
    assert!(
        output.status.success(),
        "pinned mode probes failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn resolve_default_mode_id_matches_pinned() {
    let rust_root = DisposableRoot::new("mode-probes-rust");
    let pinned_root = DisposableRoot::new("mode-probes-pinned");
    let (rust_shim, pinned_shim) = (recording_shim(&rust_root), recording_shim(&pinned_root));
    let rust = rust_results(&rust_shim, &rust_root);
    let pinned = pinned_results(&pinned_shim, &pinned_root);
    // Compared as text, with no parse and re-serialize in between.
    assert_eq!(rust, pinned, "modes differ from pinned");
    assert_eq!(
        probes_per_call(&rust_root.join("argv.log")),
        probes_per_call(&pinned_root.join("argv.log")),
        "--version probes per call differ from pinned"
    );
    // The memo answers without probing; a signal probes prefix and version;
    // an aborted signal stops after the prefix probe.
    assert_eq!(
        probes_per_call(&rust_root.join("argv.log")),
        [
            ("no-signal".to_owned(), 2),
            ("no-signal-again".to_owned(), 0),
            ("signal".to_owned(), 2),
            ("aborted".to_owned(), 1),
        ]
    );
}
