//! `ClaudeAgentClient.getDiagnostic()` with the helpers it uses from
//! `diagnostic-utils.ts` and `utils/spawn.ts` (`execCommand`).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, js_text_from_utf16, js_text_utf16};
use spocky_contracts::text::js_trim;
use tokio::io::AsyncReadExt as _;

use crate::launch::{
    ClaudeRuntimeSettings, env_pairs, external_process_env, process_env, provider_env_overlay,
    resolve_launch_path, resolve_provider_launch,
};
use crate::process::send_signal;
use spocky_provider_codex::launch::ProviderCommand;

/// `DIAGNOSTIC_OUTPUT_CAP`.
const OUTPUT_CAP: usize = 4096;
/// `COMMAND_PROBE_TIMEOUT_MS`.
const PROBE_TIMEOUT: Duration = Duration::from_millis(3000);
/// `COMMAND_PROBE_MAX_BUFFER`.
const PROBE_MAX_BUFFER: usize = 32 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_millis(5000);

/// An error as `toDiagnosticErrorMessage` reads it.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticError {
    pub message: String,
    /// `error.code`, when a string or a number.
    pub code: Option<String>,
    pub signal: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl DiagnosticError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }
}

/// `truncateForDiagnostic(value)`.
fn truncate_for_diagnostic(value: &str) -> String {
    let trimmed = js_trim(value);
    let units: Vec<u16> = js_text_utf16(trimmed).collect();
    if units.len() <= OUTPUT_CAP {
        return trimmed.to_owned();
    }
    format!("{}…(truncated)", js_text_from_utf16(&units[..OUTPUT_CAP]))
}

fn push_if_non_empty(sections: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value
        && !js_trim(value).is_empty()
    {
        sections.push(format!("{label}: {}", js_trim(value)));
    }
}

fn push_truncated_if_non_empty(sections: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value
        && !js_trim(value).is_empty()
    {
        sections.push(format!("{label}: {}", truncate_for_diagnostic(value)));
    }
}

/// `toDiagnosticErrorMessage(error)` for an `Error`.
#[must_use]
pub fn to_diagnostic_error_message(error: &DiagnosticError) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !js_trim(&error.message).is_empty() {
        sections.push(js_trim(&error.message).to_owned());
    }
    push_if_non_empty(&mut sections, "exit code", error.code.as_deref());
    push_if_non_empty(&mut sections, "signal", error.signal.as_deref());
    push_truncated_if_non_empty(&mut sections, "stderr", error.stderr.as_deref());
    push_truncated_if_non_empty(&mut sections, "stdout", error.stdout.as_deref());
    if sections.is_empty() {
        "Unknown error".to_owned()
    } else {
        sections.join("\n")
    }
}

/// `formatProviderDiagnostic(providerName, entries)`.
fn format_provider_diagnostic(provider: &str, entries: &[(String, String)]) -> String {
    let mut lines = vec![provider.to_owned()];
    lines.extend(
        entries
            .iter()
            .map(|(label, value)| format!("  {label}: {value}")),
    );
    lines.join("\n")
}

/// The result of `execFile` with a string encoding.
struct ExecOutput {
    stdout: String,
    stderr: String,
}

struct ExecOptions {
    timeout: Duration,
    kill_signal: &'static str,
    max_buffer: Option<usize>,
}

/// The `code` Node's `getSystemErrorName` gives a spawn failure.
fn spawn_error(command: &str, error: &std::io::Error) -> DiagnosticError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => Some("ENOENT"),
        std::io::ErrorKind::PermissionDenied => Some("EACCES"),
        _ => None,
    };
    match code {
        Some(code) => DiagnosticError {
            message: format!("spawn {command} {code}"),
            code: Some(code.to_owned()),
            stdout: Some(String::new()),
            stderr: Some(String::new()),
            ..DiagnosticError::default()
        },
        None => DiagnosticError::message(error.to_string()),
    }
}

/// Reads a pipe to its end or `cap` bytes.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>, cap: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        let _ = pipe.take(cap).read_to_end(&mut bytes).await;
    }
    bytes
}

/// `execCommand(command, args, options)`: `execFile` with the given
/// environment; stdin stays open.
async fn exec_command(
    command: &str,
    args: &[String],
    env: &JsObject,
    options: &ExecOptions,
) -> Result<ExecOutput, DiagnosticError> {
    let mut child = tokio::process::Command::new(command);
    child
        .args(args)
        .env_clear()
        .envs(env_pairs(env))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child
        .spawn()
        .map_err(|error| spawn_error(command, &error))?;
    let _stdin = child.stdin.take();
    let cap = options.max_buffer.map_or(u64::MAX, |max| {
        u64::try_from(max).unwrap_or(u64::MAX / 4).saturating_mul(4) + 1
    });
    let stdout_task = tokio::spawn(read_capped(child.stdout.take(), cap));
    let stderr_task = tokio::spawn(read_capped(child.stderr.take(), cap));
    let pid = child.id();
    let mut killed = false;
    let status = if let Ok(status) = tokio::time::timeout(options.timeout, child.wait()).await {
        status
    } else {
        killed = true;
        if let Some(pid) = pid {
            send_signal(pid, options.kill_signal);
        }
        child.wait().await
    };
    let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).into_owned();
    let status = status.map_err(|error| DiagnosticError::message(error.to_string()))?;
    if let Some(max) = options.max_buffer {
        for (name, text) in [("stdout", &stdout), ("stderr", &stderr)] {
            if js_text_utf16(text).count() > max {
                let units: Vec<u16> = js_text_utf16(text).take(max).collect();
                let truncated = js_text_from_utf16(&units);
                let (stdout, stderr) = if name == "stdout" {
                    (truncated, stderr.clone())
                } else {
                    (stdout.clone(), truncated)
                };
                return Err(DiagnosticError {
                    message: format!("{name} maxBuffer length exceeded"),
                    code: Some("ERR_CHILD_PROCESS_STDIO_MAXBUFFER".to_owned()),
                    stdout: Some(stdout),
                    stderr: Some(stderr),
                    ..DiagnosticError::default()
                });
            }
        }
    }
    if status.success() && !killed {
        return Ok(ExecOutput { stdout, stderr });
    }
    let joined = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let (code, signal) = {
        use std::os::unix::process::ExitStatusExt;
        if killed {
            (None, Some(options.kill_signal.to_owned()))
        } else {
            (
                status.code().map(|code| code.to_string()),
                status.signal().map(crate::process::signal_name),
            )
        }
    };
    Err(DiagnosticError {
        message: format!("Command failed: {joined}\n{stderr}"),
        code,
        signal,
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
}

fn path_variable(env: &JsObject) -> String {
    env.get("PATH")
        .or_else(|| env.get("Path"))
        .and_then(spocky_contracts::js_value::JsValue::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn is_executable_file(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // ponytail: any execute bit, where the baseline asks access(2) for X_OK
    // as the effective user; differs only for a file executable by others.
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// `formatPathMatches(options)` for the one known binary name.
fn format_path_matches(binary: &str, path: &str) -> String {
    if binary.trim().is_empty() || binary.contains('/') || binary.contains('\\') {
        return "not checked".to_owned();
    }
    let mut matches: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for directory in path.split(':').filter(|entry| !entry.is_empty()) {
        let candidate = crate::project_dir::join_path(directory, binary);
        if seen.contains(&candidate) {
            continue;
        }
        seen.push(candidate.clone());
        if is_executable_file(&candidate) {
            matches.push(candidate);
        }
    }
    if matches.is_empty() {
        "none".to_owned()
    } else {
        matches.join("\n    ")
    }
}

/// `shellToken(value)`.
fn shell_token(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// `formatCommandProbeOutput(stdout, stderr)`.
fn format_probe_output(stdout: &str, stderr: &str) -> String {
    let mut sections = Vec::new();
    let out = truncate_for_diagnostic(stdout);
    let err = truncate_for_diagnostic(stderr);
    if !out.is_empty() {
        sections.push(out);
    }
    if !err.is_empty() {
        sections.push(format!("stderr: {err}"));
    }
    if sections.is_empty() {
        "(no output)".to_owned()
    } else {
        sections.join("\n")
    }
}

/// `runCommandProbe(command, args)`.
async fn run_command_probe(command: &str, args: &[String], env: &JsObject) -> String {
    let options = ExecOptions {
        timeout: PROBE_TIMEOUT,
        kill_signal: "SIGKILL",
        max_buffer: Some(PROBE_MAX_BUFFER),
    };
    match exec_command(command, args, env, &options).await {
        Ok(output) => format_probe_output(&output.stdout, &output.stderr),
        Err(error) => to_diagnostic_error_message(&error),
    }
}

/// `resolveCommandVersion(invocation)`.
async fn resolve_command_version(command: &str, args: &[String], env: &JsObject) -> String {
    let options = ExecOptions {
        timeout: VERSION_TIMEOUT,
        kill_signal: "SIGTERM",
        max_buffer: None,
    };
    match exec_command(command, args, env, &options).await {
        Ok(output) => {
            let stdout = js_trim(&output.stdout);
            let stderr = js_trim(&output.stderr);
            if !stdout.is_empty() {
                stdout.to_owned()
            } else if !stderr.is_empty() {
                stderr.to_owned()
            } else {
                "unknown".to_owned()
            }
        }
        Err(error) => format!("error: {}", to_diagnostic_error_message(&error)),
    }
}

/// `resolveClaudeAuth(launch, availability, runtimeSettings)`.
async fn resolve_claude_auth(
    executable: &str,
    launch_args: &[String],
    env: &JsObject,
) -> Option<String> {
    let mut args: Vec<String> = launch_args.to_vec();
    args.push("auth".to_owned());
    args.push("status".to_owned());
    let options = ExecOptions {
        timeout: VERSION_TIMEOUT,
        kill_signal: "SIGTERM",
        max_buffer: None,
    };
    let (stdout, stderr) = match exec_command(executable, &args, env, &options).await {
        Ok(output) => (output.stdout, output.stderr),
        Err(error) => {
            let stderr = error
                .stderr
                .filter(|stderr| !stderr.is_empty())
                .unwrap_or(error.message);
            (error.stdout.unwrap_or_default(), stderr)
        }
    };
    let combined = [stdout, stderr]
        .iter()
        .map(|part| js_trim(part).to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!combined.is_empty()).then_some(combined)
}

/// `ClaudeAgentClient.getDiagnostic()`: the diagnostic text.
pub async fn get_diagnostic(settings: Option<&ClaudeRuntimeSettings>) -> String {
    match build_diagnostic(settings).await {
        Ok(text) => text,
        Err(error) => format_provider_diagnostic(
            "Claude Code",
            &[("Error".to_owned(), to_diagnostic_error_message(&error))],
        ),
    }
}

#[allow(clippy::too_many_lines)] // One row after another, as the baseline lists them.
async fn build_diagnostic(
    settings: Option<&ClaudeRuntimeSettings>,
) -> Result<String, DiagnosticError> {
    let (command, args) = resolve_provider_launch(settings);
    let source = match settings.and_then(|settings| settings.command.as_ref()) {
        Some(ProviderCommand::Replace { .. }) => "override",
        Some(ProviderCommand::Append { .. }) => "append",
        _ => "default",
    };
    let base_env = process_env();
    let lookup_env = external_process_env(&base_env, &[]);
    let resolved_path = resolve_launch_path(&command, &lookup_env)
        .map_err(|error| DiagnosticError::message(error.message))?;
    let available = resolved_path.is_some();
    let auth = if available {
        let executable = resolved_path.as_deref().unwrap_or(&command);
        let env = external_process_env(&base_env, &[&provider_env_overlay(settings, None)]);
        resolve_claude_auth(executable, &args, &env).await
    } else {
        None
    };
    let path_value = path_variable(&base_env);
    let shell = base_env
        .get("SHELL")
        .and_then(spocky_contracts::js_value::JsValue::as_str)
        .unwrap_or("/bin/sh")
        .to_owned();
    let mut rows: Vec<(String, String)> = Vec::new();
    rows.push(("Command source".to_owned(), source.to_owned()));
    rows.push((
        "Configured command".to_owned(),
        std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    let daemon_path = truncate_for_diagnostic(&path_value);
    rows.push((
        "Daemon PATH".to_owned(),
        if daemon_path.is_empty() {
            "(empty)".to_owned()
        } else {
            daemon_path
        },
    ));
    rows.push(("Daemon shell".to_owned(), shell.clone()));
    rows.push((
        "PATH matches".to_owned(),
        format_path_matches("claude", &path_value),
    ));
    let probe_env = external_process_env(&base_env, &[]);
    rows.push((
        "which -a claude".to_owned(),
        run_command_probe(
            "/usr/bin/which",
            &["-a".to_owned(), "claude".to_owned()],
            &probe_env,
        )
        .await,
    ));
    let shell_name = Path::new(&shell)
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    rows.push((
        format!("{shell_name} -lc type -a claude"),
        run_command_probe(
            &shell,
            &[
                "-lc".to_owned(),
                format!("type -a {}", shell_token("claude")),
            ],
            &probe_env,
        )
        .await,
    ));
    rows.push((
        if source == "override" {
            "Binary (override)".to_owned()
        } else {
            "Binary".to_owned()
        },
        command.clone(),
    ));
    rows.push((
        "Resolved path".to_owned(),
        resolved_path
            .clone()
            .unwrap_or_else(|| "not found".to_owned()),
    ));
    let version = if available {
        let executable = resolved_path.as_deref().unwrap_or(&command);
        let mut version_args = args.clone();
        version_args.push("--version".to_owned());
        let env = external_process_env(&base_env, &[&provider_env_overlay(None, None)]);
        resolve_command_version(executable, &version_args, &env).await
    } else {
        "unknown".to_owned()
    };
    rows.push(("Version".to_owned(), version));
    if let Some(auth) = auth {
        rows.push(("Auth".to_owned(), auth));
    }
    Ok(format_provider_diagnostic("Claude Code", &rows))
}
