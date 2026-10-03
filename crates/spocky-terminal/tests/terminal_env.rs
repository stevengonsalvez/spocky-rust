//! Terminal environment: fixed cases and a differential against the pinned
//! `buildTerminalEnvironment`. Node starts from a fixed environment and
//! reports the `process.env` it exposes; the Rust builder starts from those
//! same entries, so key order, overrides, removal of
//! runtime control keys, `PATH` prepending, `PASEO_HOOK_CLI` resolution, and
//! the zsh `ZDOTDIR` swap must match entry for entry.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::terminal_env::{
    TerminalEnvironmentInput, build_terminal_environment, external_process_env,
    external_process_path, prepare_zsh_runtime_dir, resolve_posix,
};

fn object(entries: &[(&str, Option<&str>)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(
            *key,
            value.map_or(JsValue::Undefined, |value| {
                JsValue::String(value.to_owned())
            }),
        );
    }
    object
}

fn keys(object: &JsObject) -> Vec<&str> {
    object.iter().map(|(key, _)| key).collect()
}

#[test]
fn external_env_drops_runtime_control_and_unset_keys() {
    let base = object(&[
        ("B", Some("1")),
        ("PASEO_SUPERVISED", Some("1")),
        ("A", Some("2")),
    ]);
    let overlay = object(&[("A", None), ("2", Some("x")), ("C", Some("3"))]);
    let env = external_process_env(&base, &[&overlay]);
    assert_eq!(keys(&env), ["2", "B", "C"]);
}

#[test]
fn resolves_paths_like_node() {
    assert_eq!(resolve_posix("/w", "a/../b//c/"), "/w/b/c");
    assert_eq!(resolve_posix("/w", "/x/./y/.."), "/x");
    assert_eq!(resolve_posix("/w", ".."), "/");
    assert_eq!(
        external_process_path("/A/app.asar/node_modules/x.asarx"),
        "/A/app.asar.unpacked/node_modules/x.asarx"
    );
    assert_eq!(
        external_process_path("/A/b.asarq/c.asar"),
        "/A/b.asarq/c.asar.unpacked"
    );
}

#[test]
fn zsh_gets_a_private_runtime_zdotdir() {
    let dir = std::env::temp_dir().join(format!("spocky-terminal-zsh-{}", std::process::id()));
    let source = dir.join("source");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join(".zshenv"), "env").expect("zshenv");
    std::fs::write(source.join("paseo-integration.zsh"), "integration").expect("integration");
    let process_env = object(&[("ZDOTDIR", Some("/home/z"))]);
    let env = object(&[]);
    let input = TerminalEnvironmentInput {
        shell: "/bin/zsh",
        process_env: &process_env,
        env: &env,
        paseo_cli_bin_dir: None,
        paseo_hook_cli_path: None,
        cwd: "/",
    };
    let built = build_terminal_environment(&input, || {
        prepare_zsh_runtime_dir(&source, &dir, "user", 42)
    })
    .expect("env");
    let runtime = dir.join("user-paseo-zsh-42");
    assert_eq!(
        stringify(&JsValue::Object(built)),
        format!(
            "{{\"ZDOTDIR\":\"{}\",\"TERM\":\"xterm-256color\",\"TERM_PROGRAM\":\"kitty\",\"PASEO_ZSH_ZDOTDIR\":\"/home/z\"}}",
            runtime.display()
        )
    );
    let mode = std::fs::metadata(&runtime)
        .expect("runtime")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    let file = std::fs::metadata(runtime.join(".zshenv")).expect("zshenv copy");
    assert_eq!(file.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::read_to_string(runtime.join("paseo-integration.zsh")).expect("copy"),
        "integration"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const NODE_SCRIPT: &str = r#"
const [terminalDir, casesJson] = process.argv.slice(1);
const { buildTerminalEnvironment } = await import(`${terminalDir}/terminal.js`);
const out = JSON.parse(casesJson).map((input) => {
  const env = buildTerminalEnvironment({
    shell: input.shell,
    env: input.env,
    paseoCliBinDir: input.binDir,
    paseoHookCliPath: input.hookCli,
  });
  return Object.entries(env);
});
const { userInfo } = await import("node:os");
process.stdout.write(JSON.stringify([Object.entries(process.env), out, process.pid, userInfo().username]));
"#;

struct Case {
    shell: &'static str,
    env: &'static [(&'static str, &'static str)],
    bin_dir: Option<&'static str>,
    hook_cli: Option<&'static str>,
}

const BASE_ENV: &[(&str, &str)] = &[
    ("HOME", "/home/spocky"),
    ("PATH", "/opt/bin::/usr/bin:/bin:/opt/bin"),
    ("PASEO_NODE_ENV", "production"),
    ("ELECTRON_RUN_AS_NODE", "1"),
    ("TERM", "dumb"),
    ("7", "seven"),
    ("ZED", "z"),
];

const CASES: &[Case] = &[
    Case {
        shell: "/bin/bash",
        env: &[],
        bin_dir: None,
        hook_cli: None,
    },
    Case {
        shell: "/bin/sh",
        env: &[("PASEO_WORKSPACE_ID", "ws"), ("ZED", "over"), ("10", "ten")],
        bin_dir: Some("/opt/bin"),
        hook_cli: Some("/App/Resources/app.asar/node_modules/cli/bin/paseo"),
    },
    // `__proto__` is dropped by Object.assign's setter; empty strings are
    // falsy, so neither prepends a PATH entry nor sets PASEO_HOOK_CLI.
    Case {
        shell: "/bin/sh",
        env: &[("__proto__", "x"), ("EMPTY", "")],
        bin_dir: Some(""),
        hook_cli: Some(""),
    },
    // zsh gets a private ZDOTDIR; the original is kept, or empty when unset.
    Case {
        shell: "/bin/zsh",
        env: &[],
        bin_dir: Some("/opt/bin"),
        hook_cli: Some("/cli/paseo"),
    },
    Case {
        shell: "/opt/homebrew/bin/zsh",
        env: &[("ZDOTDIR", "/home/z")],
        bin_dir: None,
        hook_cli: None,
    },
    Case {
        shell: "/usr/local/bin/fish",
        env: &[("ESBUILD_BINARY_PATH", "/x"), ("Path", "/a:/b")],
        bin_dir: Some("/new/bin"),
        hook_cli: Some("relative/../cli"),
    },
];

fn case_json(case: &Case) -> JsValue {
    let mut env = JsObject::new();
    for (key, value) in case.env {
        env.insert(*key, JsValue::String((*value).to_owned()));
    }
    let optional = |value: Option<&str>| {
        value.map_or(JsValue::Null, |value| JsValue::String(value.to_owned()))
    };
    let mut object = JsObject::new();
    object.insert("shell", JsValue::String(case.shell.to_owned()));
    object.insert("env", JsValue::Object(env));
    object.insert("binDir", optional(case.bin_dir));
    object.insert("hookCli", optional(case.hook_cli));
    JsValue::Object(object)
}

#[test]
fn terminal_environment_matches_pinned_builder() {
    let Some(pinned) = support::pinned("terminal environment differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let cwd = std::env::temp_dir().join(format!("spocky-terminal-env-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("cwd");
    let cwd = cwd.canonicalize().expect("canonical cwd");
    let input = stringify(&JsValue::Array(CASES.iter().map(case_json).collect()));

    // The child environment has no usable PATH, so name the bounding
    // `timeout` binary by its absolute path from this process's PATH.
    let timeout = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .flat_map(|dir| [dir.join("gtimeout"), dir.join("timeout")])
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH");
    let mut command = Command::new(timeout);
    command
        .args(["--kill-after=5", "120"])
        .arg(&pinned.node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&pinned.terminal_dir)
        .arg(&input)
        .env_clear()
        .current_dir(&cwd);
    for (key, value) in BASE_ENV {
        command.env(key, value);
    }
    command.env("TMPDIR", &cwd);
    let output = command.output().expect("run pinned node");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = parse(&String::from_utf8(output.stdout).expect("utf8")).expect("node json");
    let reported = reported.as_array().expect("pair");
    let expected = stringify(&reported[1]);
    let node_pid = format!("{}", reported[2].as_f64().expect("pid"))
        .parse::<u32>()
        .expect("pid");
    let username = reported[3].as_str().expect("username").to_owned();
    let zsh_source = pinned.terminal_dir.join("shell-integration/zsh");

    // The builder reads `process.env` as Node exposes it, which is not the
    // environment passed in (macOS adds `__CF_USER_TEXT_ENCODING`), so the
    // Rust side starts from the entries Node reported.
    let mut process_env = JsObject::new();
    for entry in reported[0].as_array().expect("entries") {
        let pair = entry.as_array().expect("entry");
        process_env.insert(pair[0].as_str().expect("key"), pair[1].clone());
    }
    let cwd_text = cwd.to_string_lossy().into_owned();
    // The pinned run wrote its zsh runtime directory under this TMPDIR.
    let tmp = cwd.clone();
    let actual = stringify(&JsValue::Array(
        CASES
            .iter()
            .map(|case| {
                let mut env = JsObject::new();
                for (key, value) in case.env {
                    env.insert(*key, JsValue::String((*value).to_owned()));
                }
                let built = build_terminal_environment(
                    &TerminalEnvironmentInput {
                        shell: case.shell,
                        process_env: &process_env,
                        env: &env,
                        paseo_cli_bin_dir: case.bin_dir,
                        paseo_hook_cli_path: case.hook_cli,
                        cwd: &cwd_text,
                    },
                    || prepare_zsh_runtime_dir(&zsh_source, &tmp, &username, node_pid),
                )
                .expect("env");
                JsValue::Array(
                    built
                        .iter()
                        .map(|(key, value)| {
                            JsValue::Array(vec![JsValue::String(key.to_owned()), value.clone()])
                        })
                        .collect(),
                )
            })
            .collect(),
    ));
    let _ = std::fs::remove_dir_all(&cwd);
    assert_eq!(actual, expected);
}
