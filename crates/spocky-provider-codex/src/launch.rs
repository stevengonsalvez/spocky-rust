//! Launching `codex app-server`.
//!
//! Ports the pinned Paseo launch path: `resolveCodexLaunchPrefix` with
//! `resolveProviderLaunch` and `findExecutable` (`/usr/bin/which -a` plus a
//! `--version` probe per candidate), the `goals` version gate
//! (`resolveBinaryVersion`, Codex 0.128.0 or newer), the auto-review gate
//! (0.115.0 or newer), the provider env overlay (`createProviderEnvSpec` and
//! `createExternalProcessEnv`), the initialize params, and the custom
//! `extends: "codex"` provider config.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

pub const CODEX_PROVIDER: &str = "codex";
pub const CODEX_NOT_FOUND_MESSAGE: &str = "Codex binary not found. Install the Codex CLI (https://github.com/openai/codex) and ensure it is available in your shell PATH.";

const PROBE_TIMEOUT: Duration = Duration::from_millis(2000);
const WHICH_TIMEOUT: Duration = Duration::from_millis(3000);
const VERSION_TIMEOUT: Duration = Duration::from_millis(5000);
const PROBE_MAX_BUFFER: usize = 64 * 1024;
const GOALS_MIN_VERSION: [u64; 3] = [0, 128, 0];
const AUTO_REVIEW_MIN_VERSION: [u64; 3] = [0, 115, 0];

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

/// `ProviderCommand` from `@getpaseo/protocol/provider-config`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProviderCommand {
    #[default]
    Default,
    Append {
        args: Vec<String>,
    },
    Replace {
        argv: Vec<String>,
    },
}

/// `ProviderRuntimeSettings` subset the Codex provider reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderRuntimeSettings {
    pub command: Option<ProviderCommand>,
    pub env: Option<BTreeMap<String, String>>,
}

/// A user provider profile that `extends` a built-in provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProvider {
    pub id: String,
    pub label: String,
    pub extends: String,
}

/// The resolved executable and leading arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPrefix {
    pub command: String,
    pub args: Vec<String>,
}

/// `resolveCodexLaunchPrefix`.
///
/// # Errors
/// Returns [`CODEX_NOT_FOUND_MESSAGE`] when no runnable Codex is found.
pub fn resolve_launch_prefix(
    settings: Option<&ProviderRuntimeSettings>,
    base_env: &[(OsString, OsString)],
) -> Result<LaunchPrefix, String> {
    let command = settings.and_then(|settings| settings.command.as_ref());
    if let Some(ProviderCommand::Replace { argv }) = command {
        let executable = argv.first().cloned().unwrap_or_default();
        let resolved = resolve_launch_path(&executable, base_env);
        if resolved.is_none() {
            return Err(CODEX_NOT_FOUND_MESSAGE.to_owned());
        }
        return Ok(LaunchPrefix {
            command: executable,
            args: argv.iter().skip(1).cloned().collect(),
        });
    }
    let args = match command {
        Some(ProviderCommand::Append { args }) => args.clone(),
        _ => Vec::new(),
    };
    let resolved = find_executable("codex", base_env).ok_or(CODEX_NOT_FOUND_MESSAGE)?;
    Ok(LaunchPrefix {
        command: resolved,
        args,
    })
}

fn resolve_launch_path(command: &str, base_env: &[(OsString, OsString)]) -> Option<String> {
    if let Some(found) = find_executable(command, base_env) {
        return Some(found);
    }
    if Path::new(command).is_absolute() && Path::new(command).exists() {
        return Some(command.to_owned());
    }
    None
}

/// `findExecutable(name)` on POSIX.
#[must_use]
pub fn find_executable(name: &str, base_env: &[(OsString, OsString)]) -> Option<String> {
    let trimmed = crate::transport::js_trim(name);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return probe_executable(trimmed, base_env).then(|| trimmed.to_owned());
    }
    which_all(trimmed, base_env)
        .into_iter()
        .find(|candidate| probe_executable(candidate, base_env))
}

fn which_all(name: &str, base_env: &[(OsString, OsString)]) -> Vec<String> {
    let mut command = Command::new("/usr/bin/which");
    command.arg("-a").arg(name);
    let Ok(outcome) = run_bounded(command, base_env, WHICH_TIMEOUT, usize::MAX) else {
        return Vec::new();
    };
    if outcome.status_code != Some(0) {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    crate::transport::js_trim(&outcome.stdout)
        .split('\n')
        .filter(|line| !line.is_empty())
        .filter(|line| seen.insert((*line).to_owned()))
        .map(str::to_owned)
        .collect()
}

/// `probeExecutable`: runnable when `--version` exits (any code) or times out.
fn probe_executable(path: &str, base_env: &[(OsString, OsString)]) -> bool {
    let mut command = Command::new(path);
    command.arg("--version");
    match run_bounded(command, base_env, PROBE_TIMEOUT, PROBE_MAX_BUFFER) {
        Ok(outcome) => !outcome.overflowed,
        Err(_) => false,
    }
}

/// `resolveBinaryVersion`: trimmed `--version` stdout, `unknown` when empty,
/// `error: ...` on failure.
#[must_use]
pub fn resolve_binary_version(binary: &str, base_env: &[(OsString, OsString)]) -> String {
    let mut command = Command::new(binary);
    command.arg("--version");
    match run_bounded(command, base_env, VERSION_TIMEOUT, 1024 * 1024) {
        Ok(outcome) if outcome.status_code == Some(0) && !outcome.timed_out => {
            let trimmed = crate::transport::js_trim(&outcome.stdout);
            if trimmed.is_empty() {
                "unknown".to_owned()
            } else {
                trimmed.to_owned()
            }
        }
        Ok(outcome) if outcome.timed_out => format!("error: Command failed: {binary} --version"),
        Ok(outcome) => format!(
            "error: Command failed: {binary} --version\n{}",
            outcome.stderr
        ),
        Err(error) => format!("error: {error}"),
    }
}

/// `codexVersionAtLeast`: first `N.N.N` in the output compared with `min`.
#[must_use]
pub fn version_at_least(version_output: &str, min: [u64; 3]) -> bool {
    let Some(parsed) = parse_version(version_output) else {
        return false;
    };
    for index in 0..3 {
        if parsed[index] > min[index] {
            return true;
        }
        if parsed[index] < min[index] {
            return false;
        }
    }
    true
}

fn parse_version(output: &str) -> Option<[u64; 3]> {
    let bytes = output.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if bytes[start].is_ascii_digit()
            && let Some(version) = version_at(&output[start..])
        {
            return Some(version);
        }
        start += 1;
    }
    None
}

fn version_at(text: &str) -> Option<[u64; 3]> {
    let mut parts = [0_u64; 3];
    let mut rest = text;
    for (index, part) in parts.iter_mut().enumerate() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        *part = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        if index < 2 {
            rest = rest.strip_prefix('.')?;
        }
    }
    Some(parts)
}

/// Codex launch gates resolved once per provider client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexGates {
    pub goals_enabled: bool,
    pub auto_review_enabled: bool,
}

/// `resolveGoalsEnabled` and `resolveAutoReviewEnabled`.
#[must_use]
pub fn resolve_gates(
    settings: Option<&ProviderRuntimeSettings>,
    base_env: &[(OsString, OsString)],
) -> CodexGates {
    let Ok(prefix) = resolve_launch_prefix(settings, base_env) else {
        return CodexGates {
            goals_enabled: false,
            auto_review_enabled: false,
        };
    };
    let version = resolve_binary_version(&prefix.command, base_env);
    CodexGates {
        goals_enabled: version_at_least(&version, GOALS_MIN_VERSION),
        auto_review_enabled: version_at_least(&version, AUTO_REVIEW_MIN_VERSION),
    }
}

/// The env a provider child receives: base env, then `runtimeSettings.env`,
/// then the launch env, minus parent-session and runtime-control keys.
#[must_use]
pub fn provider_env(
    base_env: &[(OsString, OsString)],
    settings: Option<&ProviderRuntimeSettings>,
    launch_env: Option<&BTreeMap<String, String>>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = base_env.to_vec();
    let mut set = |key: &str, value: &str| {
        let key = OsString::from(key);
        if let Some(entry) = env.iter_mut().find(|(existing, _)| *existing == key) {
            entry.1 = OsString::from(value);
        } else {
            env.push((key, OsString::from(value)));
        }
    };
    for overlay in [settings.and_then(|s| s.env.as_ref()), launch_env]
        .into_iter()
        .flatten()
    {
        for (key, value) in overlay {
            set(key, value);
        }
    }
    env.retain(|(key, _)| {
        let key = key.to_string_lossy();
        !PARENT_SESSION_ENV_VARS.contains(&key.as_ref())
            && !RUNTIME_CONTROL_ENV_KEYS.contains(&key.as_ref())
    });
    env
}

/// External-process env for probes: base env minus runtime-control keys.
fn external_env(base_env: &[(OsString, OsString)]) -> Vec<(OsString, OsString)> {
    base_env
        .iter()
        .filter(|(key, _)| !RUNTIME_CONTROL_ENV_KEYS.contains(&key.to_string_lossy().as_ref()))
        .cloned()
        .collect()
}

/// `codex app-server` arguments: launch prefix args, `app-server`, and
/// `--enable goals` when the goals gate passes.
#[must_use]
pub fn app_server_args(prefix: &LaunchPrefix, goals_enabled: bool) -> Vec<String> {
    let mut args = prefix.args.clone();
    args.push("app-server".to_owned());
    if goals_enabled {
        args.push("--enable".to_owned());
        args.push("goals".to_owned());
    }
    args
}

/// Spawns `codex app-server` with piped stdio in its own process group, as
/// Paseo's `detached: true` spawn does.
///
/// # Errors
/// Returns Node's `spawn <command> <code>` wording when the spawn fails.
pub fn spawn_app_server(
    prefix: &LaunchPrefix,
    goals_enabled: bool,
    env: &[(OsString, OsString)],
) -> Result<Child, String> {
    let mut command = Command::new(&prefix.command);
    command
        .args(app_server_args(prefix, goals_enabled))
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|error| format!("spawn {} {}", prefix.command, spawn_error_code(&error)))
}

fn spawn_error_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_owned(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
        _ => error.to_string(),
    }
}

/// `buildCodexAppServerInitializeParams`: the reserved non-originating
/// client name keeps Codex's default CLI identity.
#[must_use]
pub fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "codex_app_server_daemon",
            "title": "Codex App Server Daemon",
            "version": "0.0.0",
        },
        "capabilities": {
            "experimentalApi": true,
            "mcpServerOpenaiFormElicitation": true,
        },
    })
}

/// `normalizeOpenAICompatibleBaseUrl`.
#[must_use]
pub fn normalize_openai_compatible_base_url(value: &str) -> Option<String> {
    let trimmed = crate::transport::js_trim(value);
    if trimmed.is_empty() {
        return None;
    }
    let without_slashes = trimmed.trim_end_matches('/');
    if without_slashes.ends_with("/v1") {
        Some(without_slashes.to_owned())
    } else {
        Some(format!("{without_slashes}/v1"))
    }
}

/// `buildCodexCustomProviderConfig`: model provider config for a profile
/// that extends `codex` and sets `OPENAI_BASE_URL`.
#[must_use]
pub fn custom_provider_config(
    settings: Option<&ProviderRuntimeSettings>,
    custom: Option<&CustomProvider>,
) -> Option<Map<String, Value>> {
    let custom = custom.filter(|custom| custom.extends == CODEX_PROVIDER)?;
    let env = settings.and_then(|settings| settings.env.as_ref());
    let base_url = env.and_then(|env| env.get("OPENAI_BASE_URL"))?;
    let base_url = normalize_openai_compatible_base_url(base_url)?;
    let mut provider = Map::new();
    provider.insert("name".to_owned(), json!(custom.label));
    provider.insert("base_url".to_owned(), json!(base_url));
    provider.insert("wire_api".to_owned(), json!("responses"));
    let has_key = env
        .and_then(|env| env.get("OPENAI_API_KEY"))
        .is_some_and(|key| !crate::transport::js_trim(key).is_empty());
    if has_key {
        provider.insert("env_key".to_owned(), json!("OPENAI_API_KEY"));
        provider.insert("requires_openai_auth".to_owned(), json!(false));
    }
    let mut providers = Map::new();
    providers.insert(custom.id.clone(), Value::Object(provider));
    let mut config = Map::new();
    config.insert("model_provider".to_owned(), json!(custom.id));
    config.insert("model_providers".to_owned(), Value::Object(providers));
    Some(config)
}

struct BoundedOutcome {
    status_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    overflowed: bool,
}

/// Runs a short command with `execFile` semantics: piped output, a timeout
/// that sends SIGKILL, and a stdout byte limit.
fn run_bounded(
    mut command: Command,
    base_env: &[(OsString, OsString)],
    timeout: Duration,
    max_buffer: usize,
) -> std::io::Result<BoundedOutcome> {
    command
        .env_clear()
        .envs(external_env(base_env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(read_in_background);
    let stderr = child.stderr.take().map(read_in_background);
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    Ok(BoundedOutcome {
        status_code: status.code(),
        overflowed: stdout.len() > max_buffer,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}

fn read_in_background<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(env: &[(&str, &str)]) -> ProviderRuntimeSettings {
        ProviderRuntimeSettings {
            command: None,
            env: Some(
                env.iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ),
        }
    }

    fn custom() -> CustomProvider {
        CustomProvider {
            id: "custom-codex".to_owned(),
            label: "Custom Codex".to_owned(),
            extends: "codex".to_owned(),
        }
    }

    // Paseo: "configures Codex app-server to use a custom provider base URL".
    #[test]
    fn custom_provider_config_matches_paseo() {
        let mut provider = custom();
        provider.id = "codex-iisb".to_owned();
        let config = custom_provider_config(
            Some(&settings(&[
                ("OPENAI_API_KEY", "sk-custom"),
                ("OPENAI_BASE_URL", "https://custom-relay.example.com"),
            ])),
            Some(&provider),
        )
        .map(Value::Object);
        assert_eq!(
            serde_json::to_string(&config).unwrap(),
            r#"{"model_provider":"codex-iisb","model_providers":{"codex-iisb":{"name":"Custom Codex","base_url":"https://custom-relay.example.com/v1","wire_api":"responses","env_key":"OPENAI_API_KEY","requires_openai_auth":false}}}"#
        );
    }

    // Paseo: "does not append v1 twice for custom Codex provider base URLs".
    #[test]
    fn base_url_v1_suffix_is_not_doubled() {
        assert_eq!(
            normalize_openai_compatible_base_url("https://custom-relay.example.com/v1/"),
            Some("https://custom-relay.example.com/v1".to_owned())
        );
        assert_eq!(normalize_openai_compatible_base_url("  "), None);
    }

    #[test]
    fn custom_provider_config_requires_extends_codex_and_base_url() {
        let mut other = custom();
        other.extends = "claude".to_owned();
        let with_url = settings(&[("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")]);
        assert_eq!(custom_provider_config(Some(&with_url), Some(&other)), None);
        assert_eq!(
            custom_provider_config(Some(&settings(&[])), Some(&custom())),
            None
        );
        let config = custom_provider_config(Some(&with_url), Some(&custom())).unwrap();
        assert_eq!(
            config["model_providers"]["custom-codex"],
            json!({"name": "Custom Codex", "base_url": "http://127.0.0.1:9/v1", "wire_api": "responses"})
        );
    }

    // Paseo: "builds app-server env from launch-context env overrides".
    #[test]
    fn provider_env_layers_overlays_and_strips_control_keys() {
        let base = vec![
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("OPENAI_API_KEY"), OsString::from("base")),
            (OsString::from("CLAUDECODE"), OsString::from("1")),
            (OsString::from("PASEO_NODE_ENV"), OsString::from("test")),
        ];
        let runtime = settings(&[("OPENAI_API_KEY", "runtime"), ("CODEX_HOME", "/r")]);
        let launch: BTreeMap<String, String> =
            [("CODEX_HOME".to_owned(), "/launch".to_owned())].into();
        let env = provider_env(&base, Some(&runtime), Some(&launch));
        let lookup = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(lookup("PATH").as_deref(), Some("/bin"));
        assert_eq!(lookup("OPENAI_API_KEY").as_deref(), Some("runtime"));
        assert_eq!(lookup("CODEX_HOME").as_deref(), Some("/launch"));
        assert_eq!(lookup("CLAUDECODE"), None);
        assert_eq!(lookup("PASEO_NODE_ENV"), None);
    }

    #[test]
    fn version_gates() {
        assert!(version_at_least("codex-cli 0.159.0", GOALS_MIN_VERSION));
        assert!(version_at_least("codex-cli 0.128.0", GOALS_MIN_VERSION));
        assert!(!version_at_least("codex-cli 0.127.9", GOALS_MIN_VERSION));
        assert!(!version_at_least("unknown", GOALS_MIN_VERSION));
        assert!(version_at_least(
            "v1.2 then 0.115.0",
            AUTO_REVIEW_MIN_VERSION
        ));
    }

    #[test]
    fn app_server_args_append_goals_flag() {
        let prefix = LaunchPrefix {
            command: "/usr/local/bin/codex".to_owned(),
            args: vec!["--profile".to_owned(), "x".to_owned()],
        };
        assert_eq!(
            app_server_args(&prefix, true),
            ["--profile", "x", "app-server", "--enable", "goals"]
        );
        assert_eq!(
            app_server_args(&prefix, false),
            ["--profile", "x", "app-server"]
        );
    }

    // Paseo: "initializes Codex app-server without making Paseo the request originator".
    #[test]
    fn initialize_params_use_the_non_originating_client_name() {
        assert_eq!(
            serde_json::to_string(&initialize_params()).unwrap(),
            r#"{"clientInfo":{"name":"codex_app_server_daemon","title":"Codex App Server Daemon","version":"0.0.0"},"capabilities":{"experimentalApi":true,"mcpServerOpenaiFormElicitation":true}}"#
        );
    }
}
