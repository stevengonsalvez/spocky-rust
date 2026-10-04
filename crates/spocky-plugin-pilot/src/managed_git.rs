//! Git as pinned Paseo's managed plugin source runs it: `runGitCommand` in
//! `utils/run-git-command.ts` (the command line, the failure and timeout
//! messages, the stderr cap) and the remote credential redaction of
//! `server/plugins/managed-source.ts`.
//!
//! The baseline reads at most 2048 bytes of stderr, and a failed command's
//! message is `Git command failed: git <args> (exit code: N, signal: S)`
//! followed by the trimmed stderr (`(no stderr)` when empty). Remote
//! credentials never reach git or a message: a remote with a username or
//! password is cloned through `url.<remote>.insteadOf` as
//! `https://paseo.invalid/plugin.git`, and any message has the remote and
//! each credential replaced.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use spocky_contracts::text::js_trim;
use spocky_contracts::url::Url;

use crate::{PluginError, run_bounded_raw};

/// `DEFAULT_STDERR_LIMIT`: the bytes of stderr a failed command reports.
const STDERR_LIMIT: usize = 2048;

/// What the baseline clones in place of a remote that carries credentials.
pub const PUBLIC_PLUGIN_REMOTE: &str = "https://paseo.invalid/plugin.git";

/// `formatGitCommand(args)`.
#[must_use]
pub fn format_git_command(args: &[&str]) -> String {
    let mut command = String::from("git");
    for argument in args {
        command.push(' ');
        command.push_str(argument);
    }
    command
}

/// The message of a git command that exited with a status the caller does not
/// accept; `exit_code` is `None` and `signal` a signal name when it was
/// killed.
#[must_use]
pub fn git_failure_message(
    args: &[&str],
    exit_code: Option<i32>,
    signal: Option<&str>,
    stderr: &[u8],
) -> String {
    let stderr = &stderr[..stderr.len().min(STDERR_LIMIT)];
    let stderr = String::from_utf8_lossy(stderr);
    let preview = match js_trim(&stderr) {
        "" => "(no stderr)",
        text => text,
    };
    let code = exit_code.map_or_else(|| "null".to_owned(), |code| code.to_string());
    format!(
        "Git command failed: {} (exit code: {code}, signal: {})\n{preview}",
        format_git_command(args),
        signal.unwrap_or("none"),
    )
}

/// The message of a git command that ran past its timeout.
#[must_use]
pub fn git_timeout_message(args: &[&str], timeout: Duration) -> String {
    format!(
        "Git command timed out after {}ms: {}",
        timeout.as_millis(),
        format_git_command(args)
    )
}

/// `redactRemoteCredentials(remote)`: a `http`, `https`, `ssh`, `git`, or
/// `file` URL without its username and password, in its normalized form; any
/// other remote unchanged. `None` is a URL the baseline's `new URL` rejects.
#[must_use]
pub fn redact_remote_credentials(remote: &str) -> Option<String> {
    if !is_url_remote(remote) {
        return Some(remote.to_owned());
    }
    let mut url = Url::parse(remote, None)?;
    url.set_username("");
    url.set_password("");
    Some(url.href())
}

fn is_url_remote(remote: &str) -> bool {
    ["http://", "https://", "ssh://", "git://", "file://"]
        .iter()
        .any(|scheme| remote.starts_with(scheme))
}

/// `redactRemoteError(error, remote)` over the message: the remote becomes its
/// public form, and a URL remote's username and password (and their decoded
/// forms) become `[redacted]`. `None` is a URL the baseline's `new URL`
/// rejects.
#[must_use]
pub fn redact_remote_error(message: &str, remote: &str) -> Option<String> {
    let public = redact_remote_credentials(remote)?;
    let mut message = message.replace(remote, &public);
    if !is_url_remote(remote) {
        return Some(message);
    }
    let url = Url::parse(remote, None)?;
    for credential in [url.username(), url.password()] {
        if credential.is_empty() {
            continue;
        }
        message = message.replace(&credential, "[redacted]");
        if let Some(decoded) = decode_uri_component(&credential)
            && decoded != credential
        {
            message = message.replace(&decoded, "[redacted]");
        }
    }
    Some(message)
}

/// `decodeURIComponent`: every `%XX` sequence decoded as UTF-8; `None` where
/// it throws `URIError` (a malformed escape or invalid UTF-8).
fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = |at: usize| {
                bytes
                    .get(at)
                    .and_then(|digit| char::from(*digit).to_digit(16))
            };
            let value = hex(index + 1)? * 16 + hex(index + 2)?;
            decoded.push(u8::try_from(value).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// The name Node reports for a signal that ended a child.
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        #[cfg(target_os = "macos")]
        10 => "SIGBUS",
        #[cfg(target_os = "macos")]
        12 => "SIGSYS",
        #[cfg(not(target_os = "macos"))]
        7 => "SIGBUS",
        #[cfg(not(target_os = "macos"))]
        10 => "SIGUSR1",
        #[cfg(not(target_os = "macos"))]
        12 => "SIGUSR2",
        other => return format!("SIG{other}"),
    };
    name.to_owned()
}

/// `runGitCommand(args, { cwd, envOverlay, timeout })` with the default
/// accepted exit codes: git run with `core.quotepath=false` and
/// `core.fsmonitor=false`, no stdin, `env_overlay` over the process
/// environment. A non-zero exit, a signal, and a timeout are all
/// [`PluginError::CommandFailed`] carrying the baseline's message.
pub fn run_git(
    args: &[&str],
    cwd: &Path,
    env_overlay: &[(&str, &str)],
    timeout: Duration,
) -> Result<Output, PluginError> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.quotepath=false", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .envs(env_overlay.iter().copied())
        .stdin(Stdio::null());
    let output = match run_bounded_raw(&mut command, timeout) {
        Err(PluginError::CommandTimedOut) => {
            return Err(PluginError::CommandFailed(git_timeout_message(
                args, timeout,
            )));
        }
        other => other?,
    };
    if output.status.success() {
        return Ok(output);
    }
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&output.status).map(signal_name);
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    Err(PluginError::CommandFailed(git_failure_message(
        args,
        output.status.code(),
        signal.as_deref(),
        &output.stderr,
    )))
}

/// `GIT_ENV`: git never prompts for credentials.
const GIT_ENV: (&str, &str) = ("GIT_TERMINAL_PROMPT", "0");

/// `clone(remote, checkoutRoot)` of `managed-source.ts`: `git clone
/// --no-checkout -- <remote> <checkout_root>` run in the parent of
/// `checkout_root`. A remote with credentials is cloned as
/// [`PUBLIC_PLUGIN_REMOTE`] through `url.<remote>.insteadOf`, and any failure
/// message has the remote and its credentials redacted.
pub fn clone_remote(
    remote: &str,
    checkout_root: &Path,
    timeout: Duration,
) -> Result<(), PluginError> {
    let invalid = || PluginError::CommandFailed("Invalid URL".to_owned());
    let public = redact_remote_credentials(remote).ok_or_else(invalid)?;
    let clone_remote = if public == remote {
        remote
    } else {
        PUBLIC_PLUGIN_REMOTE
    };
    let rewrite_key = format!("url.{remote}.insteadOf");
    let mut env = vec![GIT_ENV];
    if clone_remote != remote {
        env.extend([
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", rewrite_key.as_str()),
            ("GIT_CONFIG_VALUE_0", clone_remote),
        ]);
    }
    let root = std::path::absolute(checkout_root)?;
    let root = root.to_string_lossy();
    let parent = Path::new(&*root).parent().unwrap_or_else(|| Path::new("."));
    run_git(
        &["clone", "--no-checkout", "--", clone_remote, &root],
        parent,
        &env,
        timeout,
    )
    .map(drop)
    .map_err(|error| match error {
        PluginError::CommandFailed(message) => {
            PluginError::CommandFailed(redact_remote_error(&message, remote).unwrap_or(message))
        }
        other => other,
    })
}

/// `checkout(checkoutRoot, commit)`: `git checkout --detach <commit>`, then
/// `git submodule update --init --recursive`.
pub fn checkout_commit(
    checkout_root: &Path,
    commit: &str,
    timeout: Duration,
) -> Result<(), PluginError> {
    run_git(
        &["checkout", "--detach", commit],
        checkout_root,
        &[GIT_ENV],
        timeout,
    )?;
    run_git(
        &["submodule", "update", "--init", "--recursive"],
        checkout_root,
        &[GIT_ENV],
        timeout,
    )?;
    Ok(())
}

/// `revParse(cwd, ref)`: the trimmed stdout of `git rev-parse --verify <ref>`.
pub fn rev_parse(cwd: &Path, reference: &str, timeout: Duration) -> Result<String, PluginError> {
    let output = run_git(&["rev-parse", "--verify", reference], cwd, &[], timeout)?;
    let stdout = String::from_utf8(output.stdout).map_err(|_| PluginError::InvalidCommandOutput)?;
    Ok(js_trim(&stdout).to_owned())
}
