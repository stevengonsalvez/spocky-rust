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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use spocky_contracts::text::js_trim;

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

/// A caller's abort signal, polled the way Paseo reads `signal.aborted`: true
/// once the signal has aborted.
pub type AbortCheck<'a> = &'a (dyn Fn() -> bool + Sync);

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
        let resolved = resolve_launch_path(&executable, base_env)?;
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
    let resolved = find_executable("codex", base_env)?.ok_or(CODEX_NOT_FOUND_MESSAGE)?;
    Ok(LaunchPrefix {
        command: resolved,
        args,
    })
}

fn resolve_launch_path(
    command: &str,
    base_env: &[(OsString, OsString)],
) -> Result<Option<String>, String> {
    if let Some(found) = find_executable(command, base_env)? {
        return Ok(Some(found));
    }
    if Path::new(command).is_absolute() && Path::new(command).exists() {
        return Ok(Some(command.to_owned()));
    }
    Ok(None)
}

/// `findExecutable(name)` on POSIX.
///
/// # Errors
/// Returns the `which` failure Paseo propagates: any outcome other than
/// success or exit code 1 (a missing command).
pub fn find_executable(
    name: &str,
    base_env: &[(OsString, OsString)],
) -> Result<Option<String>, String> {
    let trimmed = js_trim(name);
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Ok(probe_executable(trimmed, base_env).then(|| trimmed.to_owned()));
    }
    let candidates = if Path::new("/usr/bin/which").exists() {
        which_all(trimmed, base_env)?
    } else {
        path_search_all(trimmed, base_env)
    };
    Ok(candidates
        .into_iter()
        .find(|candidate| probe_executable(candidate, base_env)))
}

/// `enumerateCandidatesViaSystemWhich(name)`: exit code 1 means absent;
/// every other failure is thrown, since a failed lookup is not evidence of
/// absence.
fn which_all(name: &str, base_env: &[(OsString, OsString)]) -> Result<Vec<String>, String> {
    let mut command = Command::new("/usr/bin/which");
    command.arg("-a").arg(name);
    let outcome = run_bounded(command, base_env, WHICH_TIMEOUT, usize::MAX)
        .map_err(|error| format!("spawn /usr/bin/which {}", spawn_error_code(&error)))?;
    which_candidates(name, &outcome)
}

fn which_candidates(name: &str, outcome: &BoundedOutcome) -> Result<Vec<String>, String> {
    if outcome.timed_out || outcome.status_code != Some(0) {
        if !outcome.timed_out && outcome.status_code == Some(1) {
            return Ok(Vec::new());
        }
        return Err(format!(
            "Command failed: /usr/bin/which -a {name}\n{}",
            outcome.stderr
        ));
    }
    let mut seen = HashSet::new();
    Ok(js_trim(&outcome.stdout)
        .split('\n')
        .filter(|line| !line.is_empty())
        .filter(|line| seen.insert((*line).to_owned()))
        .map(str::to_owned)
        .collect())
}

/// The `which` npm package fallback when `/usr/bin/which` is absent: every
/// executable file named `name` on `PATH`, in order, without duplicates.
fn path_search_all(name: &str, base_env: &[(OsString, OsString)]) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let path = base_env
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    path.split(':')
        .map(|directory| Path::new(if directory.is_empty() { "." } else { directory }).join(name))
        .filter(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .map(|candidate| candidate.to_string_lossy().into_owned())
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

/// `probeExecutable`: runnable when `--version` exits (any code) or times out.
fn probe_executable(path: &str, base_env: &[(OsString, OsString)]) -> bool {
    let mut command = Command::new(path);
    command.arg("--version");
    // `classifyProbeError`: an exit (any code) or the probe's own timeout
    // kill counts as runnable; overflow, spawn errors, and death by another
    // signal do not.
    match run_bounded(command, base_env, PROBE_TIMEOUT, PROBE_MAX_BUFFER) {
        Ok(outcome) => !outcome.overflowed && (outcome.timed_out || outcome.status_code.is_some()),
        Err(_) => false,
    }
}

/// `resolveBinaryVersion`: trimmed `--version` stdout, `unknown` when empty,
/// `error: ...` on failure.
#[must_use]
pub fn resolve_binary_version(binary: &str, base_env: &[(OsString, OsString)]) -> String {
    resolve_binary_version_abortable(binary, base_env, None)
}

/// [`resolve_binary_version`] with Paseo's `signal`: an abort kills the probe,
/// which reports `error: The operation was aborted`.
#[must_use]
pub fn resolve_binary_version_abortable(
    binary: &str,
    base_env: &[(OsString, OsString)],
    abort: Option<AbortCheck<'_>>,
) -> String {
    let mut command = Command::new(binary);
    command.arg("--version");
    match run_bounded_abortable(command, base_env, VERSION_TIMEOUT, 1024 * 1024, abort) {
        Ok(outcome) if outcome.aborted => "error: The operation was aborted".to_owned(),
        Ok(outcome) if outcome.status_code == Some(0) && !outcome.timed_out => {
            let trimmed = js_trim(&outcome.stdout);
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

/// The launch gates a session is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexGates {
    pub goals_enabled: bool,
    pub auto_review_enabled: bool,
}

/// The probe inside `resolveGoalsEnabled`: `resolveCodexLaunchPrefix`, then
/// `resolveBinaryVersion`, then the 0.128.0 gate. Any failure is `false`.
/// The provider memoizes the result.
#[must_use]
pub fn probe_goals_enabled(
    settings: Option<&ProviderRuntimeSettings>,
    base_env: &[(OsString, OsString)],
) -> bool {
    resolve_launch_prefix(settings, base_env).is_ok_and(|prefix| {
        version_at_least(
            &resolve_binary_version(&prefix.command, base_env),
            GOALS_MIN_VERSION,
        )
    })
}

/// `probeAutoReviewEnabled(signal)`: `resolveCodexLaunchPrefix`,
/// `signal?.throwIfAborted()`, `resolveBinaryVersion(command, signal)`,
/// `signal?.throwIfAborted()`, then the 0.115.0 gate. A probe failure is
/// `false`, but if the signal has aborted by then Paseo rethrows
/// `signal.reason` instead, which is `None` here for the caller to raise.
#[must_use]
pub fn probe_auto_review_abortable(
    settings: Option<&ProviderRuntimeSettings>,
    base_env: &[(OsString, OsString)],
    abort: Option<AbortCheck<'_>>,
) -> Option<bool> {
    let aborted = || abort.is_some_and(|check| check());
    let enabled = match resolve_launch_prefix(settings, base_env) {
        Ok(prefix) => {
            if aborted() {
                return None;
            }
            version_at_least(
                &resolve_binary_version_abortable(&prefix.command, base_env, abort),
                AUTO_REVIEW_MIN_VERSION,
            )
        }
        Err(_) => false,
    };
    (!aborted()).then_some(enabled)
}

/// [`probe_auto_review_abortable`] for a signal that aborts when `deadline`
/// passes, which is how the catalog refresh's signal reaches the provider.
#[must_use]
pub fn probe_auto_review_enabled(
    settings: Option<&ProviderRuntimeSettings>,
    base_env: &[(OsString, OsString)],
    deadline: Option<Instant>,
) -> Option<bool> {
    match deadline {
        Some(deadline) => probe_auto_review_abortable(
            settings,
            base_env,
            Some(&move || Instant::now() >= deadline),
        ),
        None => probe_auto_review_abortable(settings, base_env, None),
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
    let trimmed = js_trim(value);
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
        .is_some_and(|key| !js_trim(key).is_empty());
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
    /// The caller's abort check fired and the child was killed.
    aborted: bool,
}

/// Runs a short command with `execFile` semantics: piped output, a timeout
/// that sends SIGKILL, and a per-stream byte limit that kills the child as
/// soon as either stream exceeds it.
fn run_bounded(
    command: Command,
    base_env: &[(OsString, OsString)],
    timeout: Duration,
    max_buffer: usize,
) -> std::io::Result<BoundedOutcome> {
    run_bounded_abortable(command, base_env, timeout, max_buffer, None)
}

/// [`run_bounded`] that also kills the child when `abort` reports true, as
/// `execFile`'s `signal` option does.
fn run_bounded_abortable(
    mut command: Command,
    base_env: &[(OsString, OsString)],
    timeout: Duration,
    max_buffer: usize,
    abort: Option<AbortCheck<'_>>,
) -> std::io::Result<BoundedOutcome> {
    command
        .env_clear()
        .envs(external_env(base_env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child
        .stdout
        .take()
        .map(|stream| Capture::start(stream, max_buffer, Arc::clone(&overflow)));
    let stderr = child
        .stderr
        .take()
        .map(|stream| Capture::start(stream, max_buffer, Arc::clone(&overflow)));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut aborted = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if abort.is_some_and(|check| check()) {
            aborted = true;
            let _ = child.kill();
            break child.wait()?;
        }
        if overflow.load(Ordering::SeqCst) {
            let _ = child.kill();
            break child.wait()?;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout.map(Capture::finish).unwrap_or_default();
    let stderr = stderr.map(Capture::finish).unwrap_or_default();
    Ok(BoundedOutcome {
        status_code: status.code(),
        overflowed: overflow.load(Ordering::SeqCst),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
        aborted,
    })
}

// ponytail: a grandchild that keeps the pipe open would hold Node's 'close'
// forever; we stop waiting after this and use what was read.
const OUTPUT_DRAIN_AFTER_EXIT: Duration = Duration::from_millis(1000);

/// One captured output stream.
struct Capture {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
}

impl Capture {
    fn start<R: Read + Send + 'static>(
        mut reader: R,
        max_buffer: usize,
        overflow: Arc<AtomicBool>,
    ) -> Self {
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (finished, done) = mpsc::channel();
        let sink = Arc::clone(&bytes);
        thread::spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let mut buffer = sink
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        buffer.extend_from_slice(&chunk[..read]);
                        if buffer.len() > max_buffer {
                            overflow.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
            let _ = finished.send(());
        });
        Self { bytes, done }
    }

    fn finish(self) -> Vec<u8> {
        let _ = self.done.recv_timeout(OUTPUT_DRAIN_AFTER_EXIT);
        self.bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
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

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }

    fn base_env() -> Vec<(OsString, OsString)> {
        std::env::vars_os().collect()
    }

    #[test]
    fn overflowing_output_kills_the_child_at_once() {
        let started = Instant::now();
        let outcome = run_bounded(
            shell("head -c 200000 /dev/zero; sleep 5"),
            &base_env(),
            Duration::from_secs(20),
            PROBE_MAX_BUFFER,
        )
        .expect("run");
        assert!(outcome.overflowed);
        assert!(!outcome.timed_out);
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_grandchild_holding_the_pipe_does_not_block_the_result() {
        let started = Instant::now();
        let outcome = run_bounded(
            shell("sleep 5 & echo hi"),
            &base_env(),
            Duration::from_secs(20),
            usize::MAX,
        )
        .expect("run");
        assert_eq!(outcome.status_code, Some(0));
        assert_eq!(outcome.stdout, "hi\n");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn probe_classification_matches_paseo() {
        let env = base_env();
        assert!(
            probe_executable("/usr/bin/false", &env),
            "nonzero exit is runnable"
        );
        assert!(!probe_executable("/nonexistent/codex", &env));
        let dir = std::env::temp_dir().join(format!("spocky-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("self-kill");
        std::fs::write(&script, "#!/bin/sh\nkill -9 $$\n").unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();
        assert!(
            !probe_executable(&script.to_string_lossy(), &env),
            "death by another signal is not runnable"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn which_exit_codes_follow_paseo() {
        let outcome = |code: Option<i32>, timed_out: bool, stdout: &str| BoundedOutcome {
            status_code: code,
            stdout: stdout.to_owned(),
            stderr: "boom".to_owned(),
            timed_out,
            overflowed: false,
            aborted: false,
        };
        assert_eq!(
            which_candidates(
                "codex",
                &outcome(Some(0), false, "/a/codex\n/b/codex\n/a/codex\n")
            ),
            Ok(vec!["/a/codex".to_owned(), "/b/codex".to_owned()])
        );
        assert_eq!(
            which_candidates("codex", &outcome(Some(1), false, "")),
            Ok(vec![])
        );
        assert_eq!(
            which_candidates("codex", &outcome(Some(2), false, "")),
            Err("Command failed: /usr/bin/which -a codex\nboom".to_owned())
        );
        assert_eq!(
            which_candidates("codex", &outcome(None, true, "")),
            Err("Command failed: /usr/bin/which -a codex\nboom".to_owned())
        );
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

    /// A Codex stand-in that appends its argv to `log` and prints `version`.
    fn recording_codex(dir: &Path, version: &str) -> (ProviderRuntimeSettings, std::path::PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let log = dir.join("argv.log");
        let script = dir.join("codex");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\necho '{version}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();
        let settings = ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: vec![script.to_string_lossy().into_owned()],
            }),
            env: None,
        };
        (settings, log)
    }

    fn logged(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn each_gate_probe_runs_the_prefix_then_the_version() {
        let dir = std::env::temp_dir().join(format!("spocky-gates-{}", std::process::id()));
        let (settings, log) = recording_codex(&dir, "codex-cli 0.120.0");
        let env = base_env();
        assert!(
            !probe_goals_enabled(Some(&settings), &env),
            "0.120.0 < 0.128.0"
        );
        assert_eq!(logged(&log), ["--version", "--version"]);
        assert_eq!(
            probe_auto_review_enabled(Some(&settings), &env, None),
            Some(true),
            "0.120.0 >= 0.115.0"
        );
        assert_eq!(logged(&log).len(), 4);
        // A passed deadline stops after the prefix probe, as Paseo's
        // `throwIfAborted` does.
        assert_eq!(
            probe_auto_review_enabled(Some(&settings), &env, Some(Instant::now())),
            None
        );
        assert_eq!(logged(&log).len(), 5);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_abort_stops_the_probe_after_the_prefix_and_after_the_version() {
        let dir = std::env::temp_dir().join(format!("spocky-abort-{}", std::process::id()));
        let (settings, log) = recording_codex(&dir, "codex-cli 0.159.0");
        let env = base_env();
        // Aborted from the start: the prefix probe runs, then
        // `throwIfAborted` stops before the version probe.
        assert_eq!(
            probe_auto_review_abortable(Some(&settings), &env, Some(&|| true)),
            None
        );
        assert_eq!(logged(&log).len(), 1);
        // Aborted once the version probe has started: both probes ran, and
        // the abort (which also kills the running probe) still raises.
        assert_eq!(
            probe_auto_review_abortable(Some(&settings), &env, Some(&|| logged(&log).len() >= 3)),
            None
        );
        assert_eq!(logged(&log).len(), 1 + 2);
        // Never aborted: the answer.
        assert_eq!(
            probe_auto_review_abortable(Some(&settings), &env, Some(&|| false)),
            Some(true)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_abort_kills_a_running_version_probe() {
        let dir = std::env::temp_dir().join(format!("spocky-abort-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("codex");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();
        let started = Instant::now();
        let after = started + Duration::from_millis(300);
        let version = resolve_binary_version_abortable(
            &script.to_string_lossy(),
            &base_env(),
            Some(&move || Instant::now() >= after),
        );
        assert_eq!(version, "error: The operation was aborted");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "killed, not waited out"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_codex_disables_both_gates() {
        let settings = ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: vec!["/nonexistent/codex".to_owned()],
            }),
            env: None,
        };
        assert!(!probe_goals_enabled(Some(&settings), &base_env()));
        assert_eq!(
            probe_auto_review_enabled(Some(&settings), &base_env(), None),
            Some(false)
        );
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
