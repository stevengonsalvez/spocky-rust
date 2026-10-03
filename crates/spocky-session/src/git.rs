//! Git subprocess runner from pinned Paseo `utils/run-git-command.ts`.
//!
//! Every call runs `git -c core.quotepath=false -c core.fsmonitor=false
//! <args>` without a shell, with stdin closed, and with the read-only
//! overlay `GIT_OPTIONAL_LOCKS=0` and `LC_ALL=C` on top of the daemon
//! environment minus the variables `createExternalCommandProcessEnv` drops.
//! Stdout is capped at 20 MiB (the process is killed and the truncated output
//! is returned as success), stderr at 2048 bytes, and the call is killed
//! after 30 s. Failure messages match the baseline text exactly.

// Scheduling follows `GitProcessScheduler`: FIFO admission up to the
// concurrency limit, held until the process exits, then a strict
// `p-throttle` start-rate window. Only the normal priority exists here; the
// high-priority queue serves callers outside the slice.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt};

use spocky_store::js_value::js_text_to_utf8;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::text::bytes_text;
use spocky_contracts::number::js_to_number;

/// `DEFAULT_GIT_PROCESS_POLICY`.
const DEFAULT_MAX_PROCESSES_PER_SECOND: usize = 64;
const DEFAULT_MAX_PROCESS_CONCURRENCY: usize = 8;
const THROTTLE_INTERVAL_MS: u64 = 1_000;

/// `resolveGitProcessPolicy` from the environment: `(per second, concurrency)`.
fn process_policy() -> (usize, usize) {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|value| positive_integer(&value))
    };
    (
        read("PASEO_GIT_MAX_PROCESSES_PER_SECOND").unwrap_or(DEFAULT_MAX_PROCESSES_PER_SECOND),
        read("PASEO_GIT_MAX_PROCESS_CONCURRENCY")
            .or_else(|| read("PASEO_GIT_CONCURRENCY"))
            .unwrap_or(DEFAULT_MAX_PROCESS_CONCURRENCY),
    )
}

/// `parsePositiveInteger`: `Number(value)` must be an integer above zero.
/// A count past 2^53 - 1 is refused too, as no `usize` here holds it.
fn positive_integer(value: &str) -> Option<usize> {
    let number = js_to_number(value);
    if number.fract() != 0.0 || number <= 0.0 || number > 9_007_199_254_740_991.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to be a positive safe integer above"
    )]
    Some(number as usize)
}

fn admission() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(process_policy().1))
}

/// p-throttle 8.1.0 strict mode without weights.
#[derive(Debug, Default)]
struct StrictThrottle {
    ticks: VecDeque<(u64, u64)>,
    next_id: u64,
}

impl StrictThrottle {
    /// `strictDelay`: returns the delay in ms and the tick to restamp on start.
    fn delay(&mut self, now: u64, limit: usize) -> (u64, Option<u64>) {
        if self
            .ticks
            .back()
            .is_some_and(|(_, time)| now.saturating_sub(*time) > THROTTLE_INTERVAL_MS)
        {
            self.ticks.clear();
        }
        let capacity = limit.max(1);
        self.next_id += 1;
        let id = self.next_id;
        if self.ticks.len() < capacity {
            self.ticks.push_back((id, now));
            return (0, None);
        }
        let oldest = self.ticks.front().map_or(now, |(_, time)| *time);
        let most_recent = self.ticks.back().map_or(now, |(_, time)| *time);
        let base = oldest + THROTTLE_INTERVAL_MS;
        let min_spacing = THROTTLE_INTERVAL_MS.div_ceil(u64::try_from(capacity).unwrap_or(1));
        let next = if base <= most_recent {
            most_recent + min_spacing
        } else {
            base
        };
        self.ticks.pop_front();
        self.ticks.push_back((id, next));
        (next.saturating_sub(now), Some(id))
    }

    /// Records the actual start time of a delayed call (`tickRecord.time = Date.now()`).
    fn restamp(&mut self, id: u64, now: u64) {
        if let Some(tick) = self.ticks.iter_mut().find(|(tick_id, _)| *tick_id == id) {
            tick.1 = now;
        }
    }
}

fn throttle() -> &'static Mutex<StrictThrottle> {
    static THROTTLE: OnceLock<Mutex<StrictThrottle>> = OnceLock::new();
    THROTTLE.get_or_init(Mutex::default)
}

fn clock_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Waits for the start-rate window, as `startThrottled` does before spawning.
async fn throttle_start() {
    let limit = process_policy().0;
    let (delay, tick) = throttle()
        .lock()
        .map_or((0, None), |mut state| state.delay(clock_ms(), limit));
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if let (Some(id), Ok(mut state)) = (tick, throttle().lock()) {
        state.restamp(id, clock_ms());
    }
}

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
        // The arguments are JavaScript text; node encodes them to UTF-8.
        .args(args.iter().map(|argument| js_text_to_utf8(argument)))
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
    let _slot = admission().acquire().await.map_err(|_| GitError {
        message: "Git process scheduler is closed".to_owned(),
    })?;
    throttle_start().await;
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
        stdout: bytes_text(&stdout_bytes),
        stderr: bytes_text(&stderr_bytes),
        truncated,
        exit_code,
    };
    if !truncated && !options.accept_exit_codes.contains(&exit_code.unwrap_or(-1)) {
        let preview = match crate::text::js_trim(&output.stderr) {
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
pub(crate) fn errno_name(errno: i32) -> Option<&'static str> {
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

    /// A process argument that is JavaScript text is passed as UTF-8: a lone
    /// surrogate is U+FFFD, as node's `execFile` encodes it.
    #[tokio::test]
    async fn arguments_encode_a_lone_surrogate_as_node_does() {
        let directory = std::env::temp_dir();
        let lone = spocky_store::js_value::js_text_from_utf16(&[0xD800]);
        let output = run_git(
            &["check-ref-format", "--branch", &format!("a{lone}b")],
            &GitOptions::read_only(&directory),
        )
        .await
        .expect("a name with U+FFFD is a valid branch name");
        assert_eq!(output.stdout.trim_end(), "a\u{FFFD}b");
    }

    /// Output holding U+10FFFF comes back as JavaScript text, with the
    /// character doubled, so it is not mistaken for an encoded surrogate.
    #[tokio::test]
    async fn output_holding_the_escape_character_is_javascript_text() {
        let directory = std::env::temp_dir();
        let name = spocky_store::js_value::js_text("a\u{10FFFF}\u{F0000}b");
        let output = run_git(
            &["check-ref-format", "--branch", &name],
            &GitOptions::read_only(&directory),
        )
        .await
        .expect("a name with U+10FFFF is a valid branch name");
        assert_eq!(output.stdout, format!("{name}\n"));
        assert!(
            spocky_store::js_value::js_text_units(&output.stdout)
                .all(|unit| matches!(unit, spocky_store::js_value::JsTextUnit::Char(_)))
        );
    }

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

    #[test]
    fn strict_throttle_matches_p_throttle() {
        use super::StrictThrottle;
        let mut throttle = StrictThrottle::default();
        // Delays printed by pinned p-throttle 8.1.0 with a stubbed clock:
        // [0, 0, 980, 980, 0] for calls at 0, 10, 20, 30, and 5000 ms.
        assert_eq!(throttle.delay(0, 2), (0, None));
        assert_eq!(throttle.delay(10, 2), (0, None));
        assert_eq!(throttle.delay(20, 2).0, 980);
        assert_eq!(throttle.delay(30, 2).0, 980);
        // After an idle interval the window resets.
        assert_eq!(throttle.delay(5_000, 2), (0, None));
    }

    #[test]
    fn policy_integers_follow_number_semantics() {
        use super::positive_integer;
        assert_eq!(positive_integer("8"), Some(8));
        assert_eq!(positive_integer(" 16 "), Some(16));
        assert_eq!(positive_integer("0x10"), Some(16));
        assert_eq!(positive_integer("1e1"), Some(10));
        assert_eq!(positive_integer("2.5"), None);
        assert_eq!(positive_integer("0"), None);
        assert_eq!(positive_integer(""), None);
        assert_eq!(positive_integer("abc"), None);
        assert_eq!(positive_integer("0b11"), Some(3));
        assert_eq!(positive_integer("0o17"), Some(15));
        assert_eq!(positive_integer("+4"), Some(4));
        assert_eq!(positive_integer("inf"), None);
        assert_eq!(positive_integer("Infinity"), None);
        assert_eq!(positive_integer("1_0"), None);
        // `Number` trims U+FEFF but not U+0085, which Rust's `trim` removes.
        assert_eq!(positive_integer("\u{feff}8\u{feff}"), Some(8));
        assert_eq!(positive_integer("\u{85}8"), None);
        assert_eq!(positive_integer("8\u{85}"), None);
    }

    #[tokio::test]
    async fn streaming_stdout_over_the_cap_is_killed_and_resolves_truncated() {
        /// Removes the repository when the test ends, also on a panic.
        struct RemoveOnDrop(std::path::PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!("spocky-git-stream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create repo dir");
        let _cleanup = RemoveOnDrop(root.clone());
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("big.txt"), "x".repeat(8 * 1024 * 1024)).expect("write blob");
        git(&["add", "big.txt"]);
        git(&[
            "-c",
            "user.name=S",
            "-c",
            "user.email=s@example.invalid",
            "commit",
            "-q",
            "-m",
            "big",
        ]);
        // `git show` streams 8 MiB, far past a 1 KiB cap and the pipe buffer,
        // so the process is still writing when the runner kills it.
        let output = run_git(
            &["show", "HEAD:big.txt"],
            &GitOptions {
                max_stdout_bytes: 1024,
                ..GitOptions::read_only(&root)
            },
        )
        .await
        .expect("truncated output resolves");
        assert!(output.truncated);
        assert_eq!(output.stdout.len(), 1024);
        assert_eq!(output.exit_code, None, "killed by SIGKILL, so no exit code");
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
