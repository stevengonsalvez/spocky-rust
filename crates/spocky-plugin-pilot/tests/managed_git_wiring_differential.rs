//! `clone` and `checkout` of the pinned `managed-source.ts` against
//! `managed_git::clone_remote` and `checkout_commit`: the same failing and
//! passing git operations, run for real on both sides, must print the same
//! message (or both succeed). The node side evaluates the pinned `clone`,
//! `checkout`, and redaction source with the pinned `runGitCommand`.

#![cfg(unix)]

#[path = "../../spocky-contracts/tests/support/pinned_node.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_plugin_pilot::PluginError;
use spocky_plugin_pilot::managed_git::{checkout_commit, clone_remote};

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const path = await import("node:path");
const { runGitCommand } = await import(`${dist}/utils/run-git-command.js`);
const { cases } = JSON.parse(readFileSync(file, "utf8"));
const source = readFileSync(`${dist}/server/plugins/managed-source.js`, "utf8");
const pick = (name, prefix = "") => {
  const start = source.indexOf(`${prefix}function ${name}(`);
  return source.slice(start, source.indexOf("\n}\n", start) + 3);
};
const { clone, checkout } = new Function(
  "runGitCommand", "GIT_ENV", "GIT_TIMEOUT_MS", "path",
  `${pick("redactRemoteCredentials")}\n${pick("redactRemoteError")}\n${pick("clone", "async ")}\n${pick("checkout", "async ")}\nreturn { clone, checkout };`,
)(runGitCommand, { GIT_TERMINAL_PROMPT: "0" }, 30000, path);
const out = [];
for (const [kind, a, b] of cases) {
  try {
    if (kind === "clone") await clone(a, b); else await checkout(a, b);
    out.push("OK");
  } catch (error) {
    out.push(error.message);
  }
}
process.stdout.write(JSON.stringify(out) + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "utils/run-git-command.js",
        "c67b35b6917d37ff4a3f6dfbc7377785b61a886e75176dafedd414a4ca0e67b2",
    ),
    (
        "server/plugins/managed-source.js",
        "7da584c54c4bed96b55b81e9100d1ea1302612ea2451c24cc1eb1d30c06ef63e",
    ),
];

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A committed repository, and one whose commit has a submodule that cannot
/// be fetched.
fn repositories(scratch: &Path) -> (PathBuf, String, PathBuf, String) {
    let plain = scratch.join("plain");
    std::fs::create_dir_all(&plain).expect("create");
    git(&plain, &["init", "-q"]);
    std::fs::write(plain.join("a.txt"), "a").expect("write");
    git(&plain, &["add", "a.txt"]);
    git(&plain, &["commit", "-q", "-m", "one"]);
    let plain_commit = git(&plain, &["rev-parse", "HEAD"]);
    let modules = scratch.join("modules");
    std::fs::create_dir_all(&modules).expect("create");
    git(&modules, &["init", "-q"]);
    std::fs::write(
        modules.join(".gitmodules"),
        "[submodule \"sub\"]\n\tpath = sub\n\turl = /nonexistent/spocky-submodule\n",
    )
    .expect("write");
    git(&modules, &["add", ".gitmodules"]);
    git(
        &modules,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{plain_commit},sub"),
        ],
    );
    git(&modules, &["commit", "-q", "-m", "two"]);
    let modules_commit = git(&modules, &["rev-parse", "HEAD"]);
    (plain, plain_commit, modules, modules_commit)
}

#[test]
fn clone_and_checkout_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = std::env::temp_dir().join(format!("spocky-managed-wire-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch");
    let (plain, plain_commit, modules, modules_commit) = repositories(&scratch);
    let path = |name: &str| scratch.join(name).display().to_string();
    let plain_remote = plain.display().to_string();
    // (kind, first, second): clone remote into path, or checkout commit in directory.
    let cases: Vec<(&str, String, String)> = vec![
        ("clone", "/nonexistent/spocky-remote".into(), path("c1")),
        (
            "clone",
            "file:///nonexistent/spocky-remote".into(),
            path("c2"),
        ),
        (
            "clone",
            "http://user:p%40ss@127.0.0.1:1/r.git".into(),
            path("c3"),
        ),
        ("clone", "http://user@127.0.0.1:1/r.git".into(), path("c4")),
        ("clone", "https://".into(), path("c5")),
        ("clone", plain_remote.clone(), path("c6")),
        ("clone", format!("file://{plain_remote}"), path("c7")),
        ("checkout", plain.display().to_string(), "0".repeat(40)),
        ("checkout", plain.display().to_string(), plain_commit),
        ("checkout", modules.display().to_string(), modules_commit),
        ("checkout", scratch.display().to_string(), "HEAD".into()),
    ];
    let file = scratch.join("cases.json");
    let json = JsValue::Object({
        let mut object = spocky_contracts::js_value::JsObject::new();
        object.insert(
            "cases",
            JsValue::Array(
                cases
                    .iter()
                    .map(|(kind, a, b)| {
                        JsValue::Array(vec![
                            JsValue::String((*kind).to_owned()),
                            JsValue::String(a.clone()),
                            JsValue::String(b.clone()),
                        ])
                    })
                    .collect(),
            ),
        );
        object
    });
    std::fs::write(&file, stringify(&json)).expect("write cases");
    let printed = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    let expected = parse(printed.trim()).expect("node output");
    let expected = expected.as_array().expect("array");
    assert_eq!(expected.len(), cases.len());
    let timeout = Duration::from_secs(30);
    let mut mismatches = Vec::new();
    for (index, (kind, first, second)) in cases.iter().enumerate() {
        let result = if *kind == "clone" {
            let _ = std::fs::remove_dir_all(second);
            clone_remote(first, Path::new(second), timeout)
        } else {
            // The first run registered the submodule; forget it so this run
            // prints the same progress lines.
            let _ = Command::new("git")
                .args(["config", "--remove-section", "submodule.sub"])
                .current_dir(first)
                .output();
            let _ = std::fs::remove_dir_all(Path::new(first).join(".git/modules"));
            checkout_commit_in(first, second, timeout)
        };
        let actual = match result {
            Ok(()) => "OK".to_owned(),
            Err(PluginError::CommandFailed(message)) => message,
            Err(other) => panic!("unexpected error {other:?}"),
        };
        let wanted = expected[index].as_str().expect("string");
        if without_timings(&actual) != without_timings(wanted) {
            mismatches.push(format!(
                "{kind} {first:?} {second:?}\n  node: {wanted:?}\n  rust: {actual:?}"
            ));
        }
    }
    std::fs::remove_dir_all(&scratch).expect("remove scratch");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Node ran each checkout against its own copy of the state, so the repository
/// is reset to detached `HEAD` between sides by the commit being absolute.
fn checkout_commit_in(directory: &str, commit: &str, timeout: Duration) -> Result<(), PluginError> {
    checkout_commit(Path::new(directory), commit, timeout)
}

/// curl's `after N ms` differs run to run.
fn without_timings(message: &str) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(index) = rest.find("after ") {
        let (head, tail) = rest.split_at(index + "after ".len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && tail[digits..].starts_with(" ms") {
            out.push('#');
            rest = &tail[digits..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}
