//! Git subprocess runner from pinned Paseo `utils/run-git-command.ts`.
//!
//! Every call runs `git -c core.quotepath=false -c core.fsmonitor=false
//! <args>` without a shell, with stdin closed, and with the read-only
//! overlay `GIT_OPTIONAL_LOCKS=0` and `LC_ALL=C` on top of the daemon
//! environment minus the variables `createExternalCommandProcessEnv` drops.
//! Stdout is capped at 20 MiB (the process is killed and the truncated output
//! is returned as success), stderr at 2048 bytes, and the call is killed
//! after 30 s. Failure messages match the baseline text exactly.

// ponytail: the 8-process concurrency limit is kept; the 64-per-second
// start-rate limit is not, add it if a burst of probes ever exceeds it.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Semaphore;

/// `GitProcessScheduler` default: at most eight git processes at once.
static GIT_PROCESS_SLOTS: Semaphore = Semaphore::const_new(8);

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
    /// `maxOutputBytes`, 20 MiB by default.
    pub max_stdout_bytes: usize,
}

impl<'a> GitOptions<'a> {
    #[must_use]
    pub const fn read_only(cwd: &'a Path) -> Self {
        Self {
            cwd,
            accept_exit_codes: &[0],
            timeout: DEFAULT_TIMEOUT,
            max_stdout_bytes: MAX_STDOUT_BYTES,
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
    let _slot = GIT_PROCESS_SLOTS.acquire().await.map_err(|_| GitError {
        message: "Git process scheduler is closed".to_owned(),
    })?;
    let mut child = command.spawn().map_err(|error| GitError {
        message: spawn_error_message(&error),
    })?;
    let mut stdout = child.stdout.take().ok_or_else(|| GitError {
        message: "Git process did not expose piped stdout and stderr".to_owned(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| GitError {
        message: "Git process did not expose piped stdout and stderr".to_owned(),
    })?;

    let run = async {
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            read_capped(&mut stderr, STDERR_LIMIT, false).await.0
        });
        let (stdout_bytes, truncated) =
            read_capped(&mut stdout, options.max_stdout_bytes, true).await;
        if truncated {
            // The baseline kills with SIGKILL the moment output passes the cap.
            let _ = child.start_kill();
        }
        let status = child.wait().await;
        let stderr_bytes = stderr_task.await.unwrap_or_default();
        (stdout_bytes, stderr_bytes, truncated, status)
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

/// Reads up to `limit` bytes. With `stop_on_overflow`, returns as soon as
/// more arrives (stdout); otherwise drains and drops the excess (stderr).
async fn read_capped(
    reader: &mut (impl AsyncRead + Unpin),
    limit: usize,
    stop_on_overflow: bool,
) -> (Vec<u8>, bool) {
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
                    if stop_on_overflow {
                        break;
                    }
                }
            }
        }
    }
    (kept, overflow)
}

/// Node reports spawn failures as `spawn git <errno code>`.
fn spawn_error_message(error: &std::io::Error) -> String {
    let code = error
        .raw_os_error()
        .and_then(errno_name)
        .unwrap_or(match error.kind() {
            std::io::ErrorKind::NotFound => "ENOENT",
            std::io::ErrorKind::PermissionDenied => "EACCES",
            _ => "UNKNOWN",
        });
    format!("spawn git {code}")
}

/// libuv error names for the errno values a spawn can report.
fn errno_name(errno: i32) -> Option<&'static str> {
    Some(match errno {
        1 => "EPERM",
        2 => "ENOENT",
        7 => "E2BIG",
        8 => "ENOEXEC",
        12 => "ENOMEM",
        13 => "EACCES",
        20 => "ENOTDIR",
        24 => "EMFILE",
        35 if cfg!(target_os = "macos") => "EAGAIN",
        62 if cfg!(target_os = "macos") => "ELOOP",
        63 if cfg!(target_os = "macos") => "ENAMETOOLONG",
        11 if cfg!(target_os = "linux") => "EAGAIN",
        40 if cfg!(target_os = "linux") => "ELOOP",
        36 if cfg!(target_os = "linux") => "ENAMETOOLONG",
        _ => return None,
    })
}

/// Node's signal names (`os.constants.signals`) for the platform numbers.
#[cfg(unix)]
fn signal_name(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    let Some(signal) = status.signal() else {
        return "none".to_owned();
    };
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 if cfg!(target_os = "linux") => "SIGBUS",
        7 => "SIGEMT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 if cfg!(target_os = "linux") => "SIGUSR1",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        12 if cfg!(target_os = "linux") => "SIGUSR2",
        12 => "SIGSYS",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 if cfg!(target_os = "linux") => "SIGSTKFLT",
        16 => "SIGURG",
        17 if cfg!(target_os = "linux") => "SIGCHLD",
        17 => "SIGSTOP",
        18 if cfg!(target_os = "linux") => "SIGCONT",
        18 => "SIGTSTP",
        19 if cfg!(target_os = "linux") => "SIGSTOP",
        19 => "SIGCONT",
        20 if cfg!(target_os = "linux") => "SIGTSTP",
        20 => "SIGCHLD",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 if cfg!(target_os = "linux") => "SIGURG",
        23 => "SIGIO",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 if cfg!(target_os = "linux") => "SIGIO",
        29 => "SIGINFO",
        30 if cfg!(target_os = "linux") => "SIGPWR",
        30 => "SIGUSR1",
        31 if cfg!(target_os = "linux") => "SIGSYS",
        31 => "SIGUSR2",
        other => return format!("{other}"),
    };
    name.to_owned()
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
    async fn stdout_over_the_cap_resolves_truncated() {
        let directory = std::env::temp_dir();
        let output = run_git(
            &["--version"],
            &GitOptions {
                max_stdout_bytes: 5,
                ..GitOptions::read_only(&directory)
            },
        )
        .await
        .expect("truncated output resolves");
        assert!(output.truncated);
        assert_eq!(output.stdout, "git v");
    }

    #[test]
    fn spawn_errors_use_errno_names() {
        let missing = std::io::Error::from_raw_os_error(2);
        assert_eq!(super::spawn_error_message(&missing), "spawn git ENOENT");
        let denied = std::io::Error::from_raw_os_error(13);
        assert_eq!(super::spawn_error_message(&denied), "spawn git EACCES");
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
