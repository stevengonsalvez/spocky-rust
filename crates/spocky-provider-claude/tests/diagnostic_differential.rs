//! Differential check of `getDiagnostic`: the pinned `ClaudeAgentClient` and
//! `get_diagnostic` run in the same cleared environment, with fake `claude`
//! binaries on `PATH`, and must print the same diagnostic text. The scratch
//! directory is masked as `<tmp>`.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_provider_claude::diagnostic::get_diagnostic;
use spocky_provider_claude::launch::ClaudeRuntimeSettings;
use spocky_provider_codex::launch::ProviderCommand;

const CHILD_SCENARIO: &str = "diagnostic-child.json";
const CHILD_OUT: &str = "diagnostic-child.out";

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/agent/providers/claude/agent.js",
    "c8e8e12df50c2bb45b5d33d08bb4c070e454b5919a9ad62932ffa89337f57e69",
)];

const NODE_SCRIPT: &str = r#"
import { readFileSync } from "node:fs";
const [dist, scenarioFile] = process.argv.slice(1);
const { ClaudeAgentClient } = await import(`${dist}/server/agent/providers/claude/agent.js`);
const quiet = () => {
  const logger = {};
  for (const level of ["trace", "debug", "info", "warn", "error", "fatal"]) logger[level] = () => {};
  logger.child = () => logger;
  return logger;
};
const scenario = JSON.parse(readFileSync(scenarioFile, "utf8"));
const client = new ClaudeAgentClient({
  logger: quiet(),
  ...(scenario.command ? { runtimeSettings: { command: scenario.command } } : {}),
});
const { diagnostic } = await client.getDiagnostic();
process.stdout.write(diagnostic);
"#;

/// A fake `claude`: its `--version` and `auth status` output.
fn fake_claude(version: &str, auth_stdout: &str, auth_stderr: &str, auth_exit: i32) -> String {
    format!(
        "#!/bin/sh\ncase \"$*\" in\n  *\"auth status\"*)\n    printf '%s' '{auth_stdout}'\n    printf '%s' '{auth_stderr}' >&2\n    exit {auth_exit};;\n  *--version*)\n    echo '{version}';;\nesac\n"
    )
}

/// `(name, scenario JSON)`; `@A` and `@B` stand for two bin directories.
fn scenarios() -> Vec<(&'static str, String)> {
    let one = |extra: &str| format!(r#"{{"dirs":["a"],{extra}"path":["@A"]}}"#);
    vec![
        ("present", one(r#""command":null,"#)),
        (
            "two_binaries",
            r#"{"dirs":["a","b"],"command":null,"path":["@A","@B"]}"#.to_owned(),
        ),
        (
            "missing",
            r#"{"dirs":[],"command":null,"path":[]}"#.to_owned(),
        ),
        (
            "append",
            one(r#""command":{"mode":"append","args":["--flag"]},"#),
        ),
        (
            "override",
            one(r#""command":{"mode":"replace","argv":["@A/claude","--x"]},"#),
        ),
        (
            "override_missing",
            r#"{"dirs":[],"command":{"mode":"replace","argv":["/no/such/claude"]},"path":[]}"#
                .to_owned(),
        ),
        ("auth_fails", one(r#""command":null,"authFail":true,"#)),
    ]
}

fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

/// The scenario with its directories created and placeholders resolved.
fn prepare(spec: &str, scratch: &Path) -> (JsValue, String) {
    let parsed = parse(spec).expect("scenario JSON");
    let mut path_entries: Vec<String> = Vec::new();
    for (index, dir) in parsed
        .get("dirs")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let name = dir.as_str().unwrap_or_default();
        let bin = scratch.join(format!("bin-{name}"));
        std::fs::create_dir_all(&bin).expect("bin dir");
        let fail = parsed.get("authFail") == Some(&JsValue::Bool(true));
        let script = if fail {
            fake_claude("2.1.101 (Claude Code)", "partial out", "not logged in", 1)
        } else {
            fake_claude(
                &format!("2.1.1{index}0 (Claude Code)"),
                "Logged in as test@example.com",
                "",
                0,
            )
        };
        let file = bin.join("claude");
        std::fs::write(&file, script).expect("script");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let resolve = |text: &str| {
        text.replace("@A", &scratch.join("bin-a").to_string_lossy())
            .replace("@B", &scratch.join("bin-b").to_string_lossy())
    };
    for entry in parsed
        .get("path")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        path_entries.push(resolve(entry.as_str().unwrap_or_default()));
    }
    path_entries.push("/usr/bin".to_owned());
    path_entries.push("/bin".to_owned());
    let resolved = parse(&resolve(&stringify(&parsed))).expect("resolved scenario");
    (resolved, path_entries.join(":"))
}

fn run_pinned(
    node: &std::ffi::OsString,
    dist: &Path,
    file: &Path,
    scratch: &Path,
    path: &str,
) -> String {
    let output = Command::new(timeout_binary())
        .env_clear()
        .env("HOME", scratch.join("home"))
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .env("LANG", "C")
        .args(["--kill-after=5", "60"])
        .arg(node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(dist)
        .arg(file)
        .current_dir(scratch)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8")
}

fn run_rust(file: &Path, scratch: &Path, path: &str) -> String {
    std::fs::copy(file, scratch.join(CHILD_SCENARIO)).expect("child scenario");
    let out = scratch.join(CHILD_OUT);
    let _ = std::fs::remove_file(&out);
    let output = Command::new(timeout_binary())
        .env_clear()
        .env("HOME", scratch.join("home"))
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .env("LANG", "C")
        .args(["--kill-after=5", "60"])
        .arg(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "child_prints_the_diagnostic",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(scratch)
        .output()
        .expect("run the child");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_to_string(out).unwrap_or_default()
}

fn settings_of(command: Option<&JsValue>) -> Option<ClaudeRuntimeSettings> {
    let command = command.filter(|command| matches!(command, JsValue::Object(_)))?;
    let strings = |key: &str| -> Vec<String> {
        command
            .get(key)
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(JsValue::as_str)
            .map(str::to_owned)
            .collect()
    };
    let provider_command = match command.get("mode").and_then(JsValue::as_str) {
        Some("replace") => ProviderCommand::Replace {
            argv: strings("argv"),
        },
        Some("append") => ProviderCommand::Append {
            args: strings("args"),
        },
        _ => ProviderCommand::Default,
    };
    Some(ClaudeRuntimeSettings {
        command: Some(provider_command),
        ..ClaudeRuntimeSettings::default()
    })
}

/// The child half: prints the diagnostic of the scenario in its working
/// directory. Does nothing outside the differential.
#[test]
fn child_prints_the_diagnostic() {
    let Ok(text) = std::fs::read_to_string(CHILD_SCENARIO) else {
        return;
    };
    let scenario = parse(&text).expect("JSON");
    let settings = settings_of(scenario.get("command"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let diagnostic = runtime.block_on(get_diagnostic(settings.as_ref()));
    std::fs::write(CHILD_OUT, diagnostic).expect("write");
}

#[test]
fn diagnostics_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp")
        .join(format!("spocky-diagnostic-diff-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("home")).expect("scratch");
    let mut failures = Vec::new();
    for (name, spec) in scenarios() {
        let _ = std::fs::remove_dir_all(scratch.join("bin-a"));
        let _ = std::fs::remove_dir_all(scratch.join("bin-b"));
        let (scenario, path) = prepare(&spec, &scratch);
        let file = scratch.join(format!("{name}.json"));
        std::fs::write(&file, stringify(&scenario)).expect("scenario file");
        let mask = |text: String| text.replace(&scratch.to_string_lossy().into_owned(), "<tmp>");
        let expected = mask(run_pinned(&node, &dist, &file, &scratch, &path));
        let actual = mask(run_rust(&file, &scratch, &path));
        assert!(
            expected.starts_with("Claude Code\n"),
            "{name}: pinned run printed {expected}"
        );
        if expected != actual {
            failures.push(format!("{name}\n--- node\n{expected}\n--- rust\n{actual}"));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} diagnostics differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
