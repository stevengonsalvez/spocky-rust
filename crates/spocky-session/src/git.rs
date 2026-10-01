//! Git subprocess runner from pinned Paseo `utils/run-git-command.ts`.
//!
//! Every call runs `git -c core.quotepath=false -c core.fsmonitor=false
//! <args>` without a shell, with stdin closed, and with the read-only
//! overlay `GIT_OPTIONAL_LOCKS=0` and `LC_ALL=C` on top of the daemon
//! environment minus the variables `createExternalCommandProcessEnv` drops.
//! Stdout is capped at 20 MiB (the process is killed and the truncated output
//! is returned as success), stderr at 2048 bytes, and the call is killed
//! after 30 s. Failure messages match the baseline text exactly.

// ponytail: no global 8-process / 64-per-second scheduler; add one when the
// session runs git concurrently enough for the limit to change ordering.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDOUT_BYTES: usize = 20 * 1024 * 1024;
const STDERR_LIMIT: usize = 2048;

/// `READ_ONLY_GIT_ENV`.
const READ_ONLY_GIT_ENV: [(&str, &str); 2] = [("GIT_OPTIONAL_LOCKS", "0"), ("LC_ALL", "C")];

/// Variables `createExternalCommandProcessEnv` removes before spawning.
const STRIPPED_ENV: [&str; 6] = [
    "PASEO_NODE_ENV",
    "PASEO_DESKTOP_MANAGED",
    "PASEO_SUPERVISED",
    "ELECTRON_RUN_AS_NODE",
    "ELECTRON_NO_ATTACH_CONSOLE",
    "ESBUILD_BINARY_PATH",
];

/// A completed git command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
}

/// A rejected git command, carrying the baseline error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitError {
    pub message: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitError {}

/// Options for one call. `accept_exit_codes` defaults to `[0]`.
#[derive(Debug, Clone)]
pub struct GitOptions<'a> {
    pub cwd: &'a Path,
    pub accept_exit_codes: &'a [i32],
    pub timeout: Duration,
}

impl<'a> GitOptions<'a> {
    #[must_use]
    pub const fn read_only(cwd: &'a Path) -> Self {
        Self {
            cwd,
            accept_exit_codes: &[0],
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// `runGitCommand(args, { cwd, envOverlay: READ_ONLY_GIT_ENV, ... })`.
///
/// # Errors
///
/// Returns the baseline message for a spawn failure, a timeout, or an exit
/// code outside `accept_exit_codes` when stdout was not truncated.
pub async fn run_git(args: &[&str], options: &GitOptions<'_>) -> Result<GitOutput, GitError> {
    let command_text = format!("git {}", args.join(" "));
    let mut command = Command::new("git");
    command
        .args(["-c", "core.quotepath=false", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(options.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in STRIPPED_ENV {
        command.env_remove(name);
    }
    for (name, value) in READ_ONLY_GIT_ENV {
        command.env(name, value);
    }
    let mut child = command.spawn().map_err(|error| GitError {
        message: spawn_error_message(&error),
    })?;
    let mut stdout = child.stdout.take().ok_or_else(|| GitError {
        message: "Git process did not expose piped stdout and stderr".to_owned(),
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| GitError {
        message: "Git process did not expose piped stdout and stderr".to_owned(),
    })?;

    let run = async {
        let (stdout_read, stderr_bytes) = tokio::join!(
            read_capped(&mut stdout, MAX_STDOUT_BYTES),
            read_capped(&mut stderr, STDERR_LIMIT)
        );
        let (stdout_bytes, truncated) = stdout_read;
        if truncated {
            let _ = child.start_kill();
        }
        let status = child.wait().await;
        (stdout_bytes, stderr_bytes.0, truncated, status)
    };
    let Ok((stdout_bytes, stderr_bytes, truncated, status)) =
        tokio::time::timeout(options.timeout, run).await
    else {
        return Err(GitError {
            message: format!(
                "Git command timed out after {}ms: {command_text}",
                options.timeout.as_millis()
            ),
        });
    };
    let status = status.map_err(|error| GitError {
        message: error.to_string(),
    })?;
    let exit_code = status.code();
    let output = GitOutput {
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        truncated,
        exit_code,
    };
    if !truncated && !options.accept_exit_codes.contains(&exit_code.unwrap_or(-1)) {
        let preview = match output.stderr.trim() {
            "" => "(no stderr)",
            text => text,
        };
        return Err(GitError {
            message: format!(
                "Git command failed: {command_text} (exit code: {}, signal: {})\n{preview}",
                exit_code.map_or_else(|| "null".to_owned(), |code| code.to_string()),
                signal_name(status)
            ),
        });
    }
    Ok(output)
}

/// Reads up to `limit` bytes and drains the rest; reports whether more arrived.
async fn read_capped(reader: &mut (impl AsyncRead + Unpin), limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut buffer = vec![0_u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let room = limit.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..read.min(room)]);
                if read > room {
                    overflow = true;
                }
            }
        }
    }
    (kept, overflow)
}

/// Node reports a missing binary as `spawn git ENOENT`.
fn spawn_error_message(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        "spawn git ENOENT".to_owned()
    } else {
        format!("spawn git {error}")
    }
}

#[cfg(unix)]
fn signal_name(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        None => "none".to_owned(),
        Some(9) => "SIGKILL".to_owned(),
        Some(15) => "SIGTERM".to_owned(),
        Some(2) => "SIGINT".to_owned(),
        Some(6) => "SIGABRT".to_owned(),
        Some(11) => "SIGSEGV".to_owned(),
        Some(13) => "SIGPIPE".to_owned(),
        Some(other) => format!("signal {other}"),
    }
}

#[cfg(not(unix))]
fn signal_name(_status: std::process::ExitStatus) -> String {
    "none".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{GitOptions, run_git};

    #[tokio::test]
    async fn failure_message_matches_baseline_format() {
        let directory = std::env::temp_dir();
        let error = run_git(
            &["config", "--get", "spocky.test.missing-key"],
            &GitOptions::read_only(&directory),
        )
        .await
        .expect_err("missing config key exits 1");
        assert_eq!(
            error.message,
            "Git command failed: git config --get spocky.test.missing-key (exit code: 1, signal: none)\n(no stderr)"
        );
    }

    #[tokio::test]
    async fn accepted_exit_codes_resolve() {
        let directory = std::env::temp_dir();
        let output = run_git(
            &["config", "--get", "spocky.test.missing-key"],
            &GitOptions {
                accept_exit_codes: &[0, 1],
                ..GitOptions::read_only(&directory)
            },
        )
        .await
        .expect("exit 1 accepted");
        assert_eq!(output.exit_code, Some(1));
        assert_eq!(output.stdout, "");
    }
}
