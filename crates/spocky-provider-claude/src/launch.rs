//! Launching Claude Code: `resolveClaudeBinary`, `resolveClaudeCodeVersion`,
//! `isAvailable`, the provider env (`createProviderEnv`,
//! `createProviderEnvSpec`, `createExternalProcessEnv`), and the spawn
//! command `query.ts` builds from the SDK's spawn options
//! (`resolveClaudeSpawnCommand` plus `spawnProcess`).
//!
//! Environments are [`JsObject`]s of strings so their key order is the
//! JavaScript object order the baseline passes to `child_process.spawn`.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_provider_codex::launch::{ProviderCommand, find_executable};
use spocky_session::agent_sdk::AgentError;

use crate::model_manifest::parse_claude_code_version;

/// The not-found error `resolveClaudeBinary` throws.
pub const CLAUDE_NOT_FOUND_MESSAGE: &str = "Claude binary not found. Install Claude Code (https://github.com/anthropics/claude-code) and ensure it is available in your shell PATH.";

/// Env keys a running Claude Code session leaks (`PARENT_SESSION_ENV_VARS`).
const PARENT_SESSION_ENV_VARS: [&str; 4] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_AGENT_SDK_VERSION",
];

/// Daemon runtime-control keys never passed to external processes.
const RUNTIME_CONTROL_ENV_KEYS: [&str; 6] = [
    "PASEO_NODE_ENV",
    "PASEO_DESKTOP_MANAGED",
    "PASEO_SUPERVISED",
    "ELECTRON_RUN_AS_NODE",
    "ELECTRON_NO_ATTACH_CONSOLE",
    "ESBUILD_BINARY_PATH",
];

/// `ProviderRuntimeSettings` for the Claude provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeRuntimeSettings {
    pub command: Option<ProviderCommand>,
    /// `env: Record<string, string>` in object key order.
    pub env: Option<JsObject>,
    pub disallowed_tools: Option<Vec<String>>,
}

/// `process.env` as an object of strings, in `environ` order.
///
/// The pinned Claude Agent SDK runs `process.env.NoDefaultCurrentDirectoryInExePath = "1"`
/// when it is imported, so every later reader of `process.env` (query options,
/// the spawned Claude Code, the diagnostic probes) sees it, appended after the
/// variables the process started with. The Rust process cannot change its own
/// environment (`unsafe_code` is forbidden), so the record carries it instead.
#[must_use]
pub fn process_env() -> JsObject {
    let mut env = JsObject::new();
    for (key, value) in std::env::vars_os() {
        env.insert(
            key.to_string_lossy().into_owned(),
            JsValue::String(value.to_string_lossy().into_owned()),
        );
    }
    env.insert(
        "NoDefaultCurrentDirectoryInExePath",
        JsValue::String("1".to_owned()),
    );
    env
}

/// `Object.assign(target, source)` for env records; `undefined` values are
/// kept as slots, as JavaScript does.
fn assign(target: &mut JsObject, source: &JsObject) {
    for (key, value) in source.iter() {
        target.insert(key, value.clone());
    }
}

/// `createProviderEnvSpec({ runtimeSettings, overlays })`'s `envOverlay`.
#[must_use]
pub fn provider_env_overlay(
    settings: Option<&ClaudeRuntimeSettings>,
    launch_env: Option<&JsObject>,
) -> JsObject {
    let mut overlay = JsObject::new();
    if let Some(env) = settings.and_then(|settings| settings.env.as_ref()) {
        assign(&mut overlay, env);
    }
    if let Some(env) = launch_env {
        assign(&mut overlay, env);
    }
    for key in PARENT_SESSION_ENV_VARS {
        overlay.insert(key, JsValue::Undefined);
    }
    overlay
}

/// `createExternalProcessEnv(baseEnv, ...overlays)`: the merge without
/// runtime-control keys and `undefined` values.
#[must_use]
pub fn external_process_env(base: &JsObject, overlays: &[&JsObject]) -> JsObject {
    let mut merged = base.clone();
    for overlay in overlays {
        assign(&mut merged, overlay);
    }
    let mut env = JsObject::new();
    for (key, value) in merged.iter() {
        if RUNTIME_CONTROL_ENV_KEYS.contains(&key) || matches!(value, JsValue::Undefined) {
            continue;
        }
        env.insert(key, value.clone());
    }
    env
}

/// `createProviderEnv({ baseEnv, runtimeSettings, overlays: [launchEnv] })`.
#[must_use]
pub fn provider_env(
    base: &JsObject,
    settings: Option<&ClaudeRuntimeSettings>,
    launch_env: Option<&JsObject>,
) -> JsObject {
    external_process_env(base, &[&provider_env_overlay(settings, launch_env)])
}

/// An env record as `(key, value)` pairs for a child process.
#[must_use]
pub fn env_pairs(env: &JsObject) -> Vec<(OsString, OsString)> {
    env.iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(|value| (OsString::from(key), OsString::from(value)))
        })
        .collect()
}

/// `resolveProviderLaunch({ commandConfig, defaultBinary: "claude" })`:
/// the command and its leading arguments.
#[must_use]
pub fn resolve_provider_launch(settings: Option<&ClaudeRuntimeSettings>) -> (String, Vec<String>) {
    match settings.and_then(|settings| settings.command.as_ref()) {
        Some(ProviderCommand::Replace { argv }) => (
            argv.first().cloned().unwrap_or_default(),
            argv.iter().skip(1).cloned().collect(),
        ),
        Some(ProviderCommand::Append { args }) => ("claude".to_owned(), args.clone()),
        Some(ProviderCommand::Default) | None => ("claude".to_owned(), Vec::new()),
    }
}

/// `checkProviderLaunchAvailable(launch).resolvedPath`: `findExecutable`,
/// then an existing absolute path.
///
/// # Errors
///
/// A failed `which` lookup, which the baseline rethrows.
pub fn resolve_launch_path(command: &str, env: &JsObject) -> Result<Option<String>, AgentError> {
    let pairs = env_pairs(env);
    if let Some(found) = find_executable(command, &pairs).map_err(AgentError::new)? {
        return Ok(Some(found));
    }
    let path = Path::new(command);
    Ok((path.is_absolute() && path.exists()).then(|| command.to_owned()))
}

/// `resolveClaudeBinary(runtimeSettings)`.
///
/// # Errors
///
/// [`CLAUDE_NOT_FOUND_MESSAGE`], or a failed `which` lookup.
pub fn resolve_claude_binary(
    settings: Option<&ClaudeRuntimeSettings>,
    process_env: &JsObject,
) -> Result<String, AgentError> {
    let (command, _) = resolve_provider_launch(settings);
    let env = external_process_env(process_env, &[]);
    match resolve_launch_path(&command, &env)? {
        Some(resolved) => Ok(resolved),
        None => Err(AgentError::new(CLAUDE_NOT_FOUND_MESSAGE)),
    }
}

/// `isAvailable()`.
///
/// # Errors
///
/// A failed `which` lookup, which the baseline rethrows.
pub fn is_available(
    settings: Option<&ClaudeRuntimeSettings>,
    process_env: &JsObject,
) -> Result<bool, AgentError> {
    let (command, _) = resolve_provider_launch(settings);
    let env = external_process_env(process_env, &[]);
    Ok(resolve_launch_path(&command, &env)?.is_some())
}

/// `resolveClaudeCodeVersion(runtimeSettings)`: `claude --version` with a
/// five second timeout, parsed to `major.minor.patch`.
///
/// # Errors
///
/// The baseline's errors for a missing binary, a failed run, or
/// unparseable output.
pub async fn resolve_claude_code_version(
    settings: Option<&ClaudeRuntimeSettings>,
    process_env: &JsObject,
) -> Result<String, AgentError> {
    let (command, args) = resolve_provider_launch(settings);
    let lookup_env = external_process_env(process_env, &[]);
    let Some(resolved) = resolve_launch_path(&command, &lookup_env)? else {
        return Err(AgentError::new(
            "Claude binary not found while resolving Claude Code version",
        ));
    };
    let env = external_process_env(process_env, &[&provider_env_overlay(settings, None)]);
    let mut child = tokio::process::Command::new(&resolved);
    child
        .args(&args)
        .arg("--version")
        .env_clear()
        .envs(env_pairs(&env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let joined = std::iter::once(resolved.as_str())
        .chain(args.iter().map(String::as_str))
        .chain(std::iter::once("--version"))
        .collect::<Vec<_>>()
        .join(" ");
    let output = tokio::time::timeout(Duration::from_secs(5), child.output())
        .await
        .map_err(|_| AgentError::new(format!("Command failed: {joined}\n")))?
        .map_err(|error| AgentError::new(error.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(AgentError::new(format!(
            "Command failed: {joined}\n{stderr}"
        )));
    }
    let version = parse_claude_code_version(&format!("{stdout}\n{stderr}")).ok_or_else(|| {
        AgentError::new("Unable to parse Claude Code version from --version output")
    })?;
    Ok(version
        .iter()
        .map(|part| spocky_contracts::js_value::js_number(*part))
        .collect::<Vec<_>>()
        .join("."))
}

/// `resolveClaudeSpawnCommand(spawnOptions, runtimeSettings)`.
#[must_use]
pub fn resolve_spawn_command(
    sdk_command: &str,
    sdk_args: &[String],
    settings: Option<&ClaudeRuntimeSettings>,
) -> (String, Vec<String>) {
    match settings.and_then(|settings| settings.command.as_ref()) {
        Some(ProviderCommand::Append { args }) => (
            sdk_command.to_owned(),
            sdk_args.iter().chain(args).cloned().collect(),
        ),
        Some(ProviderCommand::Replace { argv }) => (
            argv.first().cloned().unwrap_or_default(),
            argv.iter().skip(1).chain(sdk_args).cloned().collect(),
        ),
        Some(ProviderCommand::Default) | None => (sdk_command.to_owned(), sdk_args.to_vec()),
    }
}

/// The env `query.ts` spawns Claude Code with: `spawnProcess` merges the
/// SDK's env with the provider env overlay again and drops parent-session
/// and runtime-control keys.
#[must_use]
pub fn spawn_env(
    sdk_env: &JsObject,
    settings: Option<&ClaudeRuntimeSettings>,
    launch_env: Option<&JsObject>,
) -> JsObject {
    external_process_env(sdk_env, &[&provider_env_overlay(settings, launch_env)])
}
