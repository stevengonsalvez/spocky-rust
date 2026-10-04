//! `runGitCommand` failure and timeout messages, and the remote credential
//! redaction of `managed-source.ts`, in node v22.20.0 against
//! `spocky_plugin_pilot::managed_git`. The node side runs the pinned
//! `utils/run-git-command.js` for real and evaluates the pinned
//! `redactRemoteCredentials` and `redactRemoteError` source, so every case
//! prints the same message text, or the same `THROW`, in both.

#![cfg(unix)]

#[path = "../../spocky-contracts/tests/support/pinned_node.rs"]
mod support;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_plugin_pilot::PluginError;
use spocky_plugin_pilot::managed_git::{redact_remote_credentials, redact_remote_error, run_git};

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const { runGitCommand } = await import(`${dist}/utils/run-git-command.js`);
const { git, redact } = JSON.parse(readFileSync(file, "utf8"));
const source = readFileSync(`${dist}/server/plugins/managed-source.js`, "utf8");
const pick = (name) => {
  const start = source.indexOf(`function ${name}(`);
  return source.slice(start, source.indexOf("\n}\n", start) + 3);
};
const { redactRemoteCredentials, redactRemoteError } = new Function(
  `${pick("redactRemoteCredentials")}\n${pick("redactRemoteError")}\nreturn { redactRemoteCredentials, redactRemoteError };`,
)();
const out = [];
for (const { args, env, timeout, cwd } of git) {
  try {
    await runGitCommand(args, { cwd, envOverlay: env, timeout });
    out.push("OK");
  } catch (error) {
    out.push(error.message);
  }
}
for (const { remote, message } of redact) {
  let publicRemote;
  try { publicRemote = redactRemoteCredentials(remote); } catch { publicRemote = "THROW"; }
  let redacted;
  try { redacted = redactRemoteError(new Error(message), remote).message; } catch { redacted = "THROW"; }
  out.push(JSON.stringify([publicRemote, redacted]));
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

/// A git command: arguments, an alias (`!` shell) the overlay defines as
/// `alias.case`, the timeout in milliseconds, and whether it runs in an
/// initialized repository.
struct GitCase {
    args: &'static [&'static str],
    alias: Option<&'static str>,
    timeout_ms: u32,
    in_repository: bool,
}

const GIT_CASES: &[GitCase] = &[
    GitCase {
        args: &["rev-parse", "--verify", "HEAD"],
        alias: None,
        timeout_ms: 30_000,
        in_repository: true,
    },
    GitCase {
        args: &[
            "checkout",
            "--detach",
            "0000000000000000000000000000000000000000",
        ],
        alias: None,
        timeout_ms: 30_000,
        in_repository: true,
    },
    GitCase {
        args: &["show-ref", "--verify", "--quiet", "refs/heads/none"],
        alias: None,
        timeout_ms: 30_000,
        in_repository: true,
    },
    GitCase {
        args: &["status"],
        alias: None,
        timeout_ms: 30_000,
        in_repository: true,
    },
    GitCase {
        args: &["status"],
        alias: None,
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!printf '\\n  spaced  \\n' >&2; exit 1"),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case", "--flag", "two words"],
        alias: Some("!exit 7"),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!head -c 5000 /dev/zero | tr '\\0' x >&2; exit 3"),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some(
            "!printf x >&2; head -c 1100 /dev/zero | tr '\\0' e | sed 's/e/\\xc3\\xa9/g' >&2; exit 4",
        ),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!printf '   \\n\\t ' >&2; exit 5"),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!sleep 5"),
        timeout_ms: 300,
        in_repository: false,
    },
    GitCase {
        args: &["case", "--long"],
        alias: Some("!sleep 5"),
        timeout_ms: 1500,
        in_repository: false,
    },
    // `-c` settings reach a child of git through GIT_CONFIG_PARAMETERS.
    GitCase {
        args: &["case"],
        alias: Some(
            "!git config --get core.quotepath >&2; git config --get core.fsmonitor >&2; exit 1",
        ),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!kill -TERM $$"),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!kill -KILL $$"),
        timeout_ms: 30_000,
        in_repository: false,
    },
    GitCase {
        args: &["case"],
        alias: Some("!exit 0"),
        timeout_ms: 30_000,
        in_repository: false,
    },
];

/// `(remote, message)` pairs for the redaction helpers.
const REDACT_CASES: &[(&str, &str)] = &[
    (
        "https://user:pa%20ss@example.com/x.git",
        "fatal: unable to access 'https://user:pa%20ss@example.com/x.git/': user pa ss failed",
    ),
    ("https://u:p@h/r", "u:p and p and u appear: https://u:p@h/r"),
    ("https://u:p@h/r", "no credentials here"),
    ("ssh://git@host/r.git", "ssh://git@host/r.git git denied"),
    ("https://user%zz:pw@h/", "user%zz pw user%zz:pw@h"),
    ("git@github.com:o/r.git", "git@github.com:o/r.git failed"),
    ("file:///tmp/x", "file:///tmp/x is not a repo"),
    (
        "https://%E2%82%AC:%F0%9F@h/",
        "\u{20ac} %E2%82%AC %F0%9F @h",
    ),
    ("HTTPS://U:P@H/R", "HTTPS://U:P@H/R U P"),
    ("https://[::1]:80/", "https://[::1]:80/ down"),
    (
        "https://h:80/a/../b",
        "https://h:80/a/../b gone https://h:80/b",
    ),
    ("https://U@h/", "U U U@h"),
    ("https://a:b@h/", "https://a:b@h/ a b a:b"),
    ("https://a%3Ab:c%2Fd@h/p", "a:b c/d a%3Ab c%2Fd"),
    ("https://@h/", "@h"),
    ("https://", "anything"),
    ("https://u:p@", "u:p"),
    ("git://u:p@h/r", "git://u:p@h/r"),
    ("http://%75:%70@h/r", "u p %75 %70"),
    ("https://caf\u{e9}:pw@h/", "caf\u{e9} pw"),
    ("not a url", "not a url failed"),
    ("", "empty remote"),
];

fn json_string(text: &str) -> JsValue {
    JsValue::String(text.to_owned())
}

fn init_repository(directory: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(directory)
        .stdout(Stdio::null())
        .status()
        .expect("git init");
    assert!(status.success());
}

fn node_cases(repository: &Path, scratch: &Path) -> JsValue {
    let git = GIT_CASES
        .iter()
        .map(|case| {
            let mut env = JsObject::new();
            if let Some(alias) = case.alias {
                env.insert("GIT_CONFIG_COUNT", json_string("1"));
                env.insert("GIT_CONFIG_KEY_0", json_string("alias.case"));
                env.insert("GIT_CONFIG_VALUE_0", json_string(alias));
            }
            let mut object = JsObject::new();
            object.insert(
                "args",
                JsValue::Array(case.args.iter().map(|arg| json_string(arg)).collect()),
            );
            object.insert("env", JsValue::Object(env));
            object.insert("timeout", JsValue::Number(f64::from(case.timeout_ms)));
            let cwd = if case.in_repository {
                repository
            } else {
                scratch
            };
            object.insert("cwd", json_string(&cwd.display().to_string()));
            JsValue::Object(object)
        })
        .collect();
    let redact = REDACT_CASES
        .iter()
        .map(|(remote, message)| {
            let mut object = JsObject::new();
            object.insert("remote", json_string(remote));
            object.insert("message", json_string(message));
            JsValue::Object(object)
        })
        .collect();
    let mut all = JsObject::new();
    all.insert("git", JsValue::Array(git));
    all.insert("redact", JsValue::Array(redact));
    JsValue::Object(all)
}

fn rust_git(case: &GitCase, repository: &Path, scratch: &Path) -> String {
    let env: Vec<(&str, &str)> = case
        .alias
        .map(|alias| {
            vec![
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "alias.case"),
                ("GIT_CONFIG_VALUE_0", alias),
            ]
        })
        .unwrap_or_default();
    let cwd = if case.in_repository {
        repository
    } else {
        scratch
    };
    match run_git(
        case.args,
        cwd,
        &env,
        Duration::from_millis(u64::from(case.timeout_ms)),
    ) {
        Ok(_) => "OK".to_owned(),
        Err(PluginError::CommandFailed(message)) => message,
        Err(other) => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn git_messages_and_redaction_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = std::env::temp_dir().join(format!("spocky-managed-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let repository = scratch.join("repository");
    std::fs::create_dir_all(&repository).expect("create scratch");
    init_repository(&repository);
    let file = scratch.join("cases.json");
    std::fs::write(&file, stringify(&node_cases(&repository, &scratch))).expect("write cases");
    let printed = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    let expected = spocky_contracts::js_value::parse(printed.trim()).expect("node output");
    let expected = expected.as_array().expect("array");
    assert_eq!(
        expected.len(),
        GIT_CASES.len() + REDACT_CASES.len(),
        "node printed a different number of results"
    );
    let mut mismatches = Vec::new();
    for (index, case) in GIT_CASES.iter().enumerate() {
        let wanted = expected[index].as_str().expect("string");
        let actual = rust_git(case, &repository, &scratch);
        if actual != wanted {
            mismatches.push(format!(
                "git {:?} {:?}\n  node: {wanted:?}\n  rust: {actual:?}",
                case.args, case.alias
            ));
        }
    }
    for (offset, (remote, message)) in REDACT_CASES.iter().enumerate() {
        let wanted = expected[GIT_CASES.len() + offset].as_str().expect("string");
        let public = redact_remote_credentials(remote);
        let redacted = redact_remote_error(message, remote);
        let actual = match (public, redacted) {
            (Some(public), Some(redacted)) => stringify(&JsValue::Array(vec![
                json_string(&public),
                json_string(&redacted),
            ])),
            _ => stringify(&JsValue::Array(vec![
                json_string("THROW"),
                json_string("THROW"),
            ])),
        };
        if actual != wanted {
            mismatches.push(format!(
                "redact {remote:?} {message:?}\n  node: {wanted}\n  rust: {actual}"
            ));
        }
    }
    std::fs::remove_dir_all(&scratch).expect("remove scratch");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
