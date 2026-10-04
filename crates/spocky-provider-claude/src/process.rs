//! The Claude Code child process: `spawnProcess` from `utils/spawn.ts` with
//! the env order `child_process.spawn` gives it, the `exit` event, and
//! `terminateWithTreeKill` from `utils/tree-kill.ts`.
//!
//! `std::process::Command` sorts the environment, but Node passes it in
//! object key order. The child is therefore started through
//! `/usr/bin/env -i KEY=VALUE... command args...`, which builds `environ`
//! in argument order and then executes the command in place (same pid,
//! same argv). A command or key containing `=` cannot pass through
//! `env(1)`, so that rare case spawns directly with a sorted environment.
//!
//! Signals go through `kill(1)` (the crate forbids `unsafe`, so no
//! `libc::kill`) and only to the recorded child pid and the descendants
//! `ps` lists under it, never by name or argv. A signal is sent only while
//! the recorded child has not exited, so a reused pid is not signalled.

use std::cell::RefCell;
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::rc::Rc;
use std::time::Duration;

use spocky_contracts::js_value::JsObject;
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStdin, ChildStdout};

use crate::launch::env_pairs;
use crate::local::Deferred;

/// How the child exited: `(code, signal)` as Node's `exit` event reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildExit {
    pub code: Option<i32>,
    pub signal: Option<String>,
}

/// A spawned Claude Code process.
pub struct ChildProcess {
    pub pid: Option<u32>,
    stdin: RefCell<Option<ChildStdin>>,
    stdout: RefCell<Option<ChildStdout>>,
    exit: Rc<Deferred<ChildExit>>,
    killed: RefCell<bool>,
}

/// The name Node gives a signal number: the host's table.
pub(crate) fn signal_name(number: i32) -> String {
    #[cfg(target_os = "macos")]
    const NAMES: [&str; 32] = [
        "",
        "SIGHUP",
        "SIGINT",
        "SIGQUIT",
        "SIGILL",
        "SIGTRAP",
        "SIGABRT",
        "SIGEMT",
        "SIGFPE",
        "SIGKILL",
        "SIGBUS",
        "SIGSEGV",
        "SIGSYS",
        "SIGPIPE",
        "SIGALRM",
        "SIGTERM",
        "SIGURG",
        "SIGSTOP",
        "SIGTSTP",
        "SIGCONT",
        "SIGCHLD",
        "SIGTTIN",
        "SIGTTOU",
        "SIGIO",
        "SIGXCPU",
        "SIGXFSZ",
        "SIGVTALRM",
        "SIGPROF",
        "SIGWINCH",
        "SIGINFO",
        "SIGUSR1",
        "SIGUSR2",
    ];
    #[cfg(not(target_os = "macos"))]
    const NAMES: [&str; 32] = [
        "",
        "SIGHUP",
        "SIGINT",
        "SIGQUIT",
        "SIGILL",
        "SIGTRAP",
        "SIGABRT",
        "SIGBUS",
        "SIGFPE",
        "SIGKILL",
        "SIGUSR1",
        "SIGSEGV",
        "SIGUSR2",
        "SIGPIPE",
        "SIGALRM",
        "SIGTERM",
        "SIGSTKFLT",
        "SIGCHLD",
        "SIGCONT",
        "SIGSTOP",
        "SIGTSTP",
        "SIGTTIN",
        "SIGTTOU",
        "SIGURG",
        "SIGXCPU",
        "SIGXFSZ",
        "SIGVTALRM",
        "SIGPROF",
        "SIGWINCH",
        "SIGIO",
        "SIGPWR",
        "SIGSYS",
    ];
    usize::try_from(number)
        .ok()
        .and_then(|index| NAMES.get(index))
        .filter(|name| !name.is_empty())
        .map_or_else(|| format!("SIG{number}"), |name| (*name).to_owned())
}

/// The spawn request `spawnClaudeCodeProcess` receives, after Paseo's
/// command and env resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: JsObject,
}

/// How a failed spawn reaches the caller in Node 22.20.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnFailureKind {
    /// EACCES, EAGAIN and ENOENT: the child's `error` event, a tick later,
    /// with `spawn <file> <code>`.
    Event,
    /// EMFILE and ENFILE: the same event, but the child has no stdio streams.
    EventWithoutStdio,
    /// Every other code: `spawn()` throws `spawn <code>` at once.
    Thrown,
}

/// A failed spawn and the way Node reports its errno.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnFailure {
    /// `ENOENT`, `EACCES`, and so on, when known.
    pub code: Option<String>,
    pub message: String,
    pub kind: SpawnFailureKind,
}

impl SpawnFailure {
    /// The failure Node reports for the libuv error `code` of spawning `file`.
    fn for_code(file: &str, code: &str) -> Self {
        let kind = match code {
            "EACCES" | "EAGAIN" | "ENOENT" => SpawnFailureKind::Event,
            "EMFILE" | "ENFILE" => SpawnFailureKind::EventWithoutStdio,
            _ => SpawnFailureKind::Thrown,
        };
        let message = if kind == SpawnFailureKind::Thrown {
            format!("spawn {code}")
        } else {
            format!("spawn {file} {code}")
        };
        Self {
            code: Some(code.to_owned()),
            message,
            kind,
        }
    }
}

/// libuv's name for an errno value (`uv_err_name`): the POSIX codes of its
/// error map, numbered for the host.
#[must_use]
#[allow(clippy::too_many_lines)] // The tables.
pub fn errno_name(number: i32) -> Option<&'static str> {
    // Generated from util.getSystemErrorMap() of the pinned node by
    // scripts/phase4/provider-claude-errno-table.sh; the errno_differential
    // test compares both tables with it.
    #[cfg(target_os = "macos")]
    const TABLE: [(i32, &str); 65] = [
        (1, "EPERM"),
        (2, "ENOENT"),
        (3, "ESRCH"),
        (4, "EINTR"),
        (5, "EIO"),
        (6, "ENXIO"),
        (7, "E2BIG"),
        (8, "ENOEXEC"),
        (9, "EBADF"),
        (12, "ENOMEM"),
        (13, "EACCES"),
        (14, "EFAULT"),
        (16, "EBUSY"),
        (17, "EEXIST"),
        (18, "EXDEV"),
        (19, "ENODEV"),
        (20, "ENOTDIR"),
        (21, "EISDIR"),
        (22, "EINVAL"),
        (23, "ENFILE"),
        (24, "EMFILE"),
        (25, "ENOTTY"),
        (26, "ETXTBSY"),
        (27, "EFBIG"),
        (28, "ENOSPC"),
        (29, "ESPIPE"),
        (30, "EROFS"),
        (31, "EMLINK"),
        (32, "EPIPE"),
        (34, "ERANGE"),
        (35, "EAGAIN"),
        (37, "EALREADY"),
        (38, "ENOTSOCK"),
        (39, "EDESTADDRREQ"),
        (40, "EMSGSIZE"),
        (41, "EPROTOTYPE"),
        (42, "ENOPROTOOPT"),
        (43, "EPROTONOSUPPORT"),
        (44, "ESOCKTNOSUPPORT"),
        (45, "ENOTSUP"),
        (47, "EAFNOSUPPORT"),
        (48, "EADDRINUSE"),
        (49, "EADDRNOTAVAIL"),
        (50, "ENETDOWN"),
        (51, "ENETUNREACH"),
        (53, "ECONNABORTED"),
        (54, "ECONNRESET"),
        (55, "ENOBUFS"),
        (56, "EISCONN"),
        (57, "ENOTCONN"),
        (58, "ESHUTDOWN"),
        (60, "ETIMEDOUT"),
        (61, "ECONNREFUSED"),
        (62, "ELOOP"),
        (63, "ENAMETOOLONG"),
        (64, "EHOSTDOWN"),
        (65, "EHOSTUNREACH"),
        (66, "ENOTEMPTY"),
        (78, "ENOSYS"),
        (79, "EFTYPE"),
        (84, "EOVERFLOW"),
        (89, "ECANCELED"),
        (92, "EILSEQ"),
        (96, "ENODATA"),
        (100, "EPROTO"),
    ];
    #[cfg(not(target_os = "macos"))]
    const TABLE: [(i32, &str); 67] = [
        (1, "EPERM"),
        (2, "ENOENT"),
        (3, "ESRCH"),
        (4, "EINTR"),
        (5, "EIO"),
        (6, "ENXIO"),
        (7, "E2BIG"),
        (8, "ENOEXEC"),
        (9, "EBADF"),
        (11, "EAGAIN"),
        (12, "ENOMEM"),
        (13, "EACCES"),
        (14, "EFAULT"),
        (16, "EBUSY"),
        (17, "EEXIST"),
        (18, "EXDEV"),
        (19, "ENODEV"),
        (20, "ENOTDIR"),
        (21, "EISDIR"),
        (22, "EINVAL"),
        (23, "ENFILE"),
        (24, "EMFILE"),
        (25, "ENOTTY"),
        (26, "ETXTBSY"),
        (27, "EFBIG"),
        (28, "ENOSPC"),
        (29, "ESPIPE"),
        (30, "EROFS"),
        (31, "EMLINK"),
        (32, "EPIPE"),
        (34, "ERANGE"),
        (36, "ENAMETOOLONG"),
        (38, "ENOSYS"),
        (39, "ENOTEMPTY"),
        (40, "ELOOP"),
        (49, "EUNATCH"),
        (61, "ENODATA"),
        (64, "ENONET"),
        (71, "EPROTO"),
        (75, "EOVERFLOW"),
        (84, "EILSEQ"),
        (88, "ENOTSOCK"),
        (89, "EDESTADDRREQ"),
        (90, "EMSGSIZE"),
        (91, "EPROTOTYPE"),
        (92, "ENOPROTOOPT"),
        (93, "EPROTONOSUPPORT"),
        (94, "ESOCKTNOSUPPORT"),
        (95, "ENOTSUP"),
        (97, "EAFNOSUPPORT"),
        (98, "EADDRINUSE"),
        (99, "EADDRNOTAVAIL"),
        (100, "ENETDOWN"),
        (101, "ENETUNREACH"),
        (103, "ECONNABORTED"),
        (104, "ECONNRESET"),
        (105, "ENOBUFS"),
        (106, "EISCONN"),
        (107, "ENOTCONN"),
        (108, "ESHUTDOWN"),
        (110, "ETIMEDOUT"),
        (111, "ECONNREFUSED"),
        (112, "EHOSTDOWN"),
        (113, "EHOSTUNREACH"),
        (114, "EALREADY"),
        (121, "EREMOTEIO"),
        (125, "ECANCELED"),
    ];
    TABLE
        .iter()
        .find(|(value, _)| *value == number)
        .map(|(_, name)| *name)
}

fn is_executable_file(path: &std::path::Path) -> Result<(), &'static str> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err("ENOENT"),
        Err(_) => Err("EACCES"),
        Ok(meta) if meta.is_dir() || meta.permissions().mode() & 0o111 == 0 => Err("EACCES"),
        Ok(_) => Ok(()),
    }
}

/// libuv's executable lookup: a path with `/` as given, else each `PATH`
/// entry of the child's env.
fn preflight(command: &str, env: &JsObject, cwd: Option<&str>) -> Result<(), SpawnFailure> {
    let failure = |code: &str| SpawnFailure::for_code(command, code);
    // The forked child changes directory before it looks for the command.
    if let Some(cwd) = cwd {
        match std::fs::metadata(cwd) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(failure("ENOENT"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(failure("EACCES"));
            }
            Ok(meta) if !meta.is_dir() => return Err(failure("ENOTDIR")),
            _ => {}
        }
    }
    if command.contains('/') {
        return is_executable_file(std::path::Path::new(command)).map_err(failure);
    }
    let path = env
        .get("PATH")
        .and_then(spocky_contracts::js_value::JsValue::as_str)
        .unwrap_or("/usr/bin:/bin");
    let mut last = "ENOENT";
    for directory in path.split(':') {
        let directory = if directory.is_empty() { "." } else { directory };
        match is_executable_file(&std::path::Path::new(directory).join(command)) {
            Ok(()) => return Ok(()),
            Err("EACCES") => last = "EACCES",
            Err(_) => {}
        }
    }
    Err(failure(last))
}

impl ChildProcess {
    /// `spawnProcess(command, args, { cwd, env, stdio: "pipe", shell: false })`.
    ///
    /// # Errors
    ///
    /// The spawn failure Node reports as the child's `error` event.
    pub fn spawn(
        request: &SpawnRequest,
        on_stderr: Option<Rc<dyn Fn(String)>>,
    ) -> Result<Rc<Self>, SpawnFailure> {
        preflight(&request.command, &request.env, request.cwd.as_deref())?;
        let pairs = env_pairs(&request.env);
        let trampoline = !request.command.contains('=')
            && pairs
                .iter()
                .all(|(key, _)| !key.to_string_lossy().contains('='));
        let mut command = if trampoline {
            let mut command = tokio::process::Command::new("/usr/bin/env");
            command.arg("-i");
            for (key, value) in &pairs {
                let mut assignment = key.clone();
                assignment.push("=");
                assignment.push(value);
                command.arg(assignment);
            }
            command.arg(&request.command);
            command.env_clear();
            command
        } else {
            let mut command = tokio::process::Command::new(&request.command);
            command.env_clear().envs(pairs);
            command
        };
        command
            .args(&request.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| {
            let number = error.raw_os_error();
            // libuv's name for a code it does not know.
            match number
                .and_then(errno_name)
                .map(str::to_owned)
                .or_else(|| number.map(|number| format!("Unknown system error -{number}")))
            {
                Some(code) => SpawnFailure::for_code(&request.command, &code),
                None => SpawnFailure {
                    code: None,
                    message: error.to_string(),
                    kind: SpawnFailureKind::Thrown,
                },
            }
        })?;
        let process = Rc::new(Self {
            pid: child.id(),
            stdin: RefCell::new(child.stdin.take()),
            stdout: RefCell::new(child.stdout.take()),
            exit: Deferred::new(),
            killed: RefCell::new(false),
        });
        if let Some(mut stderr) = child.stderr.take() {
            tokio::task::spawn_local(async move {
                let mut buffer = vec![0_u8; 64 * 1024];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if let Some(handler) = &on_stderr {
                                handler(String::from_utf8_lossy(&buffer[..count]).into_owned());
                            }
                        }
                    }
                }
            });
        }
        let exit = Rc::clone(&process.exit);
        tokio::task::spawn_local(async move {
            let status = child.wait().await;
            let exit_value = match status {
                Ok(status) => ChildExit {
                    code: status.code(),
                    signal: status.signal().map(signal_name),
                },
                Err(_) => ChildExit {
                    code: None,
                    signal: None,
                },
            };
            exit.settle(exit_value);
        });
        Ok(process)
    }

    /// Takes the stdin pipe.
    pub fn take_stdin(&self) -> Option<ChildStdin> {
        self.stdin.borrow_mut().take()
    }

    /// Takes the stdout pipe.
    pub fn take_stdout(&self) -> Option<ChildStdout> {
        self.stdout.borrow_mut().take()
    }

    /// The exit, once the child has exited.
    #[must_use]
    pub fn exited(&self) -> Option<ChildExit> {
        self.exit.peek()
    }

    /// Waits for the `exit` event.
    pub async fn wait_exit(&self) -> ChildExit {
        self.exit.wait().await
    }

    /// `child.killed`: a signal was sent.
    #[must_use]
    pub fn killed(&self) -> bool {
        *self.killed.borrow()
    }

    /// `child.kill(signal)` while the child is running.
    pub fn kill(&self, signal: &str) {
        if self.exited().is_some() {
            return;
        }
        if let Some(pid) = self.pid {
            send_signal(pid, signal);
            *self.killed.borrow_mut() = true;
        }
    }
}

/// Called with the pid and name of a signal.
pub type SignalObserver = Rc<dyn Fn(u32, &str)>;

thread_local! {
    /// Called with every signal this thread sends, for tests.
    static SIGNAL_OBSERVER: RefCell<Option<SignalObserver>> = const { RefCell::new(None) };
}

/// Reports each signal sent from this thread to `observer`, or stops
/// reporting with `None`.
pub fn observe_signals(observer: Option<SignalObserver>) {
    SIGNAL_OBSERVER.with(|slot| *slot.borrow_mut() = observer);
}

pub(crate) fn send_signal(pid: u32, signal: &str) {
    if let Some(observer) = SIGNAL_OBSERVER.with(|slot| slot.borrow().clone()) {
        observer(pid, signal);
    }
    let name = signal.trim_start_matches("SIG");
    let _ = std::process::Command::new("/bin/kill")
        .args(["-s", name, &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The pids `tree-kill` signals, in its order: it walks the tree from `root`
/// breadth first, then for each pid found, signals the pid's children and
/// then the pid itself, skipping any already signalled. A grandchild
/// therefore comes after the root.
fn kill_order(root: u32) -> Vec<u32> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-A", "-o", "ppid=", "-o", "pid="])
        .output()
    else {
        return vec![root];
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    let pairs: Vec<(u32, u32)> = listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect();
    let mut keys = vec![root];
    let mut children: Vec<Vec<u32>> = Vec::new();
    let mut at = 0;
    while at < keys.len() {
        let parent = keys[at];
        let found: Vec<u32> = pairs
            .iter()
            .filter(|(ppid, _)| *ppid == parent)
            .map(|(_, pid)| *pid)
            .collect();
        for pid in &found {
            if !keys.contains(pid) {
                keys.push(*pid);
            }
        }
        children.push(found);
        at += 1;
    }
    let mut order: Vec<u32> = Vec::new();
    for (key, kids) in keys.iter().zip(&children) {
        for kid in kids {
            if !order.contains(kid) {
                order.push(*kid);
            }
        }
        if !order.contains(key) {
            order.push(*key);
        }
    }
    order
}

/// `signalProcessTree(child, signal)`: `tree-kill` signals the pids with
/// `process.kill`, which leaves `child.killed` alone; only a child without a
/// pid is signalled through `child.kill`.
pub fn signal_process_tree(child: &ChildProcess, signal: &str) {
    if child.exited().is_some() {
        return;
    }
    let Some(pid) = child.pid else {
        child.kill(signal);
        return;
    };
    for target in kill_order(pid) {
        send_signal(target, signal);
    }
}

/// `TerminateWithTreeKillResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateResult {
    AlreadyExited,
    Terminated,
    Killed,
    KillTimeout,
}

/// `terminateWithTreeKill(child, { gracefulTimeoutMs, forceTimeoutMs })`.
pub async fn terminate_with_tree_kill(
    child: &ChildProcess,
    graceful: Duration,
    force: Duration,
) -> TerminateResult {
    if child.exited().is_some() {
        return TerminateResult::AlreadyExited;
    }
    signal_process_tree(child, "SIGTERM");
    if tokio::time::timeout(graceful, child.wait_exit())
        .await
        .is_ok()
    {
        return TerminateResult::Terminated;
    }
    signal_process_tree(child, "SIGKILL");
    if tokio::time::timeout(force, child.wait_exit()).await.is_ok() {
        TerminateResult::Killed
    } else {
        TerminateResult::KillTimeout
    }
}

/// Waits `delay`, then signals the child unless it already exited (the
/// SDK's `close()` timers).
pub async fn kill_after(child: Rc<ChildProcess>, delay: Duration, signal: &'static str) {
    tokio::time::sleep(delay).await;
    if child.exited().is_none() {
        child.kill(signal);
    }
}
