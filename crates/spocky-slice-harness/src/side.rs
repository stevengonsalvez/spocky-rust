//! Runs one side of a slice gate: disposable root, Responses stub, daemon in a
//! named tmux session, the pinned CLI command script, bounded stop with
//! survivor verification, and capture of every compared output.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::stub::{FORBIDDEN_PORTS, RecordedRequest, Script};

/// Prefix of every disposable root and tmux session the harness owns.
pub const OWNED_PREFIX: &str = "spocky-p3-";
const ROOT_PARENT: &str = "/private/tmp";
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const READY_INTERVAL: Duration = Duration::from_millis(500);
const STEP_TIMEOUT: Duration = Duration::from_secs(240);
const STOP_GRACE: Duration = Duration::from_secs(30);
const KILL_GRACE: Duration = Duration::from_secs(5);
const FIXTURE_DATE: &str = "2020-01-02T03:04:05Z";

/// macOS seatbelt wrapper used for every daemon and CLI process of a side.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Seatbelt profile: everything the pinned programs need is allowed except
/// outbound IP connections to anything but loopback. Inherited by every
/// descendant (the codex app-server, git, shells). A denied connect fails
/// with `EPERM` and the kernel logs `Sandbox: <name>(<pid>) deny(1)
/// network-outbound`, which [`egress_violations`] turns into a gate failure.
pub const EGRESS_PROFILE: &str = "(version 1)(allow default)\
(deny network-outbound (remote ip \"*:*\"))\
(allow network-outbound (remote ip \"localhost:*\"))";

/// Which daemon a side runs. The client is always the pinned Paseo CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonKind {
    Original,
    Spocky,
}

impl DaemonKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Spocky => "spocky",
        }
    }

    /// Parses `original` or `spocky`.
    ///
    /// # Errors
    ///
    /// Returns a message for any other value.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "original" => Ok(Self::Original),
            "spocky" => Ok(Self::Spocky),
            other => Err(format!(
                "unknown daemon {other:?}; expected original or spocky"
            )),
        }
    }
}

/// Verified tool locations shared by both sides.
#[derive(Debug, Clone)]
pub struct Tools {
    /// Built pinned Paseo archive root (contains `packages/cli`).
    pub paseo_root: PathBuf,
    /// Directory holding Node 22.20.0 `node`.
    pub node_bin: PathBuf,
    /// Pinned `codex` binary.
    pub codex: PathBuf,
    /// `spocky-responses-stub` binary.
    pub stub: PathBuf,
    /// `spocky-daemon` binary, required only for Spocky sides.
    pub spocky_daemon: Option<PathBuf>,
}

/// One CLI argument template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Lit(&'static str),
    /// Expands to `--host 127.0.0.1:<daemon port>`.
    Host,
    /// The absolute project directory.
    Project,
    /// A value captured from an earlier step on the same side.
    Captured(&'static str),
}

/// One pinned CLI invocation.
#[derive(Debug, Clone)]
pub struct StepSpec {
    pub name: &'static str,
    pub args: Vec<Arg>,
    /// Captures a JSON string from stdout as (key, JSON pointer).
    pub capture: Option<(&'static str, &'static str)>,
}

/// A positive per-side assertion. Equality between sides is not enough: both
/// sides failing the same way must still fail the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Every step and the readiness probe exited 0.
    AllExitZero,
    /// A step's stdout parses as JSON with this string at the pointer.
    JsonString {
        step: &'static str,
        pointer: &'static str,
        expected: &'static str,
    },
    /// A step's stdout contains this text.
    StdoutContains {
        step: &'static str,
        needle: &'static str,
    },
    /// A step's stdout has a line exactly equal to this text.
    StdoutLine {
        step: &'static str,
        line: &'static str,
    },
    /// Every scripted reply was consumed and no unscripted request arrived.
    StubExactlyConsumed,
    /// The daemon exited with this code after SIGTERM.
    DaemonExit(i32),
}

/// Named digest preimages, as (name, exact preimage).
pub type Preimages = Vec<(&'static str, String)>;

/// Builds a side's preimages from its captured values.
pub type PreimageBuilder = fn(&BTreeMap<&'static str, String>) -> Preimages;

/// A complete gate definition.
#[derive(Debug, Clone)]
pub struct GateSpec {
    pub id: &'static str,
    pub script: Script,
    pub steps: Vec<StepSpec>,
    pub checks: Vec<Check>,
    /// Exact preimages the gate knows the daemon hashes (creation request
    /// fingerprints), built from this side's `project` path and captures.
    pub preimages: PreimageBuilder,
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Exit {
    Code(i32),
    /// Exited with this code but a leftover process held its output open.
    PipesHeld(i32),
    Signal(i32),
    TimedOut,
    NotRun(String),
}

impl Exit {
    fn from_status(status: ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Signal(signal);
            }
        }
        status
            .code()
            .map_or(Self::NotRun("no exit code".into()), Self::Code)
    }

    /// Stable text form used as a compared artifact.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Code(code) => format!("exit {code}"),
            Self::PipesHeld(code) => format!("exit {code}, output pipes held open"),
            Self::Signal(signal) => format!("signal {signal}"),
            Self::TimedOut => "timed out".into(),
            Self::NotRun(reason) => format!("not run: {reason}"),
        }
    }
}

/// One executed CLI step.
#[derive(Debug, Clone, Serialize)]
pub struct StepRun {
    pub name: String,
    pub argv: Vec<String>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: Exit,
    /// Stub requests recorded when the step ended.
    pub stub_requests: usize,
}

/// A captured file, relative to the disposable root.
#[derive(Debug, Clone, Serialize)]
pub struct CapturedFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// Everything one side produced.
#[derive(Debug, Clone, Serialize)]
pub struct SideRun {
    pub kind: DaemonKind,
    pub root: String,
    pub session: String,
    pub daemon_port: u16,
    pub stub_port: u16,
    pub daemon_pid: Option<u32>,
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    pub readiness_attempts: u32,
    pub readiness: StepRun,
    pub steps: Vec<StepRun>,
    pub daemon_exit: Exit,
    pub stub_records: Vec<String>,
    pub stub_scripted: usize,
    pub stub_unscripted: usize,
    pub script_len: usize,
    /// Compared files: the Paseo home, the user home, and the codex config.
    pub state: Vec<CapturedFile>,
    /// Retained but not compared: daemon logs and the codex home listing.
    pub uncompared: Vec<CapturedFile>,
    pub extracted: Vec<(String, String)>,
    pub preimages: Vec<(&'static str, String)>,
    /// Processes that needed SIGKILL after the grace period.
    pub force_killed: Vec<u32>,
    /// Processes still alive after SIGKILL. Any entry fails the gate.
    pub survivors: Vec<u32>,
    /// Harness failures that prevented a complete capture.
    pub harness_errors: Vec<String>,
    /// Every PID observed on this side (daemon tree, CLI steps, codex
    /// invocations, root-path scans); used for the egress check.
    pub observed_pids: Vec<u32>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Quotes a value for `/bin/sh` single quotes.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Finds a free loopback port that is never forbidden.
///
/// # Errors
///
/// Returns an I/O error when binding fails.
pub fn free_port() -> io::Result<u16> {
    let listener = crate::stub::bind_loopback()?;
    listener.local_addr().map(|address| address.port())
}

/// How long output pipes may stay open after the process itself ended.
const PIPE_GRACE: Duration = Duration::from_secs(5);

/// Runs `command` in its own process group with piped output and a hard
/// timeout. On expiry the whole group is killed. Output collection is bounded:
/// if a leftover group member keeps a pipe open past [`PIPE_GRACE`], the group
/// is killed and the result is [`Exit::PipesHeld`], which fails every check.
fn run_bounded(command: &mut Command, timeout: Duration) -> (Vec<u8>, Vec<u8>, Exit) {
    run_tracked(command, timeout, &mut Vec::new())
}

/// [`run_bounded`], also recording the spawned PID in `pids`.
fn run_tracked(
    command: &mut Command,
    timeout: Duration,
    pids: &mut Vec<u32>,
) -> (Vec<u8>, Vec<u8>, Exit) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return (Vec::new(), Vec::new(), Exit::NotRun(error.to_string())),
    };
    let group = child.id();
    pids.push(group);
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let mut exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Exit::from_status(status),
            Ok(None) if started.elapsed() >= timeout => {
                kill_group(group);
                let _ = child.kill();
                let _ = child.wait();
                break Exit::TimedOut;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => break Exit::NotRun(error.to_string()),
        }
    };
    let mut collect = |receiver: &std::sync::mpsc::Receiver<Vec<u8>>| {
        if let Ok(bytes) = receiver.recv_timeout(PIPE_GRACE) {
            return bytes;
        }
        kill_group(group);
        if let Exit::Code(code) = exit {
            exit = Exit::PipesHeld(code);
        }
        receiver.recv_timeout(PIPE_GRACE).unwrap_or_default()
    };
    let stdout = collect(&stdout);
    let stderr = collect(&stderr);
    (stdout, stderr, exit)
}

fn kill_group(group: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    receiver
}

/// Directory layout of one disposable root.
struct Layout {
    root: PathBuf,
}

impl Layout {
    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
    fn text(&self, relative: &str) -> String {
        self.path(relative).display().to_string()
    }
}

/// The exact environment for every CLI and daemon process on a side.
fn side_environment(layout: &Layout, tools: &Tools) -> BTreeMap<String, String> {
    let path = format!(
        "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
        layout.text("bin"),
        tools.node_bin.display()
    );
    BTreeMap::from([
        ("CODEX_HOME".to_owned(), layout.text("codex-home")),
        ("HOME".to_owned(), layout.text("home")),
        ("PASEO_HOME".to_owned(), layout.text("paseo-home")),
        ("PATH".to_owned(), path),
        ("TMPDIR".to_owned(), layout.text("tmp")),
        ("TZ".to_owned(), "UTC".to_owned()),
        ("USERPROFILE".to_owned(), layout.text("home")),
    ])
}

fn codex_config(stub_port: u16) -> String {
    format!(
        "model_provider = \"spocky-stub\"\n\n\
         [model_providers.spocky-stub]\n\
         name = \"spocky-stub\"\n\
         base_url = \"http://127.0.0.1:{stub_port}/v1\"\n\
         env_key = \"OPENAI_API_KEY\"\n\
         wire_api = \"responses\"\n\
         supports_websockets = false\n\
         request_max_retries = 0\n\
         stream_max_retries = 0\n\n\
         [analytics]\n\
         enabled = false\n\n\
         [features]\n\
         plugins = false\n"
    )
}

fn paseo_config(layout: &Layout, daemon_port: u16, stub_port: u16) -> Value {
    serde_json::json!({
        "daemon": {
            "listen": format!("127.0.0.1:{daemon_port}"),
            "relay": { "enabled": false }
        },
        "features": {
            "dictation": { "enabled": false },
            "voiceMode": { "enabled": false }
        },
        "agents": {
            "providers": {
                "codex": {
                    "env": {
                        "CODEX_HOME": layout.text("codex-home"),
                        "OPENAI_BASE_URL": format!("http://127.0.0.1:{stub_port}/v1"),
                        "OPENAI_API_KEY": "test-key"
                    }
                }
            }
        }
    })
}

fn create_layout(gate: &str, tools: &Tools) -> io::Result<Layout> {
    // Equal-length root names on both sides keep length-derived values (for
    // example HTTP content-length) comparable; hex time is never a wall-clock literal.
    let root = loop {
        let candidate =
            Path::new(ROOT_PARENT).join(format!("{OWNED_PREFIX}{gate}-{:011x}", now_ms()));
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    };
    let layout = Layout { root };
    for directory in [
        "home",
        "paseo-home",
        "codex-home",
        "project",
        "bin",
        "tmp",
        "stub",
        "codex-io",
    ] {
        fs::create_dir(layout.path(directory))?;
    }
    write_codex_wrapper(&layout, &tools.codex)?;
    Ok(layout)
}

/// Most codex invocations one side may record; the shim exits 95 beyond it.
pub const MAX_CODEX_INVOCATIONS: u32 = 1000;

/// The `codex` the daemon finds on `PATH`. It records each invocation's argv
/// and the exact bytes the daemon writes to codex stdin (the app-server
/// JSON-RPC input) under `codex-io/<n>/`, then execs the pinned binary as the
/// same PID with stdin fed through a FIFO by `tee`.
#[must_use]
pub fn codex_wrapper_script(io_dir: &str, codex: &str) -> String {
    format!(
        "#!/bin/sh\n\
         io={io}\n\
         [ -d \"$io\" ] || exit 96\n\
         n=1\n\
         while ! /bin/mkdir \"$io/$n\" 2>/dev/null; do\n\
         n=$((n + 1))\n\
         [ \"$n\" -le {max} ] || exit 95\n\
         done\n\
         printf '%s\\n' \"$$\" >\"$io/$n/pid\"\n\
         for arg in \"$@\"; do printf '%s\\n' \"$arg\"; done >\"$io/$n/argv\"\n\
         /usr/bin/mkfifo \"$io/$n/fifo\" || exit 98\n\
         exec 9<&0\n\
         /usr/bin/tee \"$io/$n/stdin\" <&9 >\"$io/$n/fifo\" &\n\
         exec {codex} \"$@\" <\"$io/$n/fifo\" 9<&-\n",
        max = MAX_CODEX_INVOCATIONS,
        io = shell_quote(io_dir),
        codex = shell_quote(codex),
    )
}

fn write_codex_wrapper(layout: &Layout, codex: &Path) -> io::Result<()> {
    let path = layout.path("bin/codex");
    fs::write(
        &path,
        codex_wrapper_script(&layout.text("codex-io"), &codex.display().to_string()),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn git_command(layout: &Layout, args: &[&str]) -> Command {
    let mut command = Command::new("/usr/bin/git");
    command
        .args(args)
        .current_dir(layout.path("project"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", layout.text("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Spocky Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@spocky.invalid")
        .env("GIT_AUTHOR_DATE", FIXTURE_DATE)
        .env("GIT_COMMITTER_NAME", "Spocky Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@spocky.invalid")
        .env("GIT_COMMITTER_DATE", FIXTURE_DATE);
    command
}

fn init_project(layout: &Layout) -> Result<(), String> {
    let project = layout.path("project");
    fs::write(project.join("README.md"), "fixture\n").map_err(|error| error.to_string())?;
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["add", "README.md"],
        &["commit", "-q", "-m", "fixture"],
    ] {
        let mut command = git_command(layout, args);
        let (_, stderr, exit) = run_bounded(&mut command, Duration::from_secs(30));
        if exit != Exit::Code(0) {
            return Err(format!(
                "git {args:?} failed: {} {}",
                exit.render(),
                String::from_utf8_lossy(&stderr)
            ));
        }
    }
    Ok(())
}

/// The running stub; killed and reaped on drop so no error path leaks it.
struct Stub {
    child: Child,
    port: u16,
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_stub(layout: &Layout, tools: &Tools, script: &Script) -> Result<Stub, String> {
    let script_path = layout.path("stub/script.json");
    let bytes = serde_json::to_vec_pretty(script).map_err(|error| error.to_string())?;
    fs::write(&script_path, bytes).map_err(|error| error.to_string())?;
    let port_path = layout.path("stub/port");
    let log = fs::File::create(layout.path("stub/stub.log")).map_err(|error| error.to_string())?;
    let log_err = log.try_clone().map_err(|error| error.to_string())?;
    let mut child = Command::new(&tools.stub)
        .arg(&script_path)
        .arg(layout.path("stub/record.jsonl"))
        .arg(&port_path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .map_err(|error| format!("start stub: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = fs::read_to_string(&port_path) {
            let port = text
                .trim()
                .parse::<u16>()
                .map_err(|error| format!("stub port file: {error}"))?;
            return Ok(Stub { child, port });
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("stub exited early: {status}"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("stub did not publish its port within 10 s".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn stub_records(layout: &Layout) -> Vec<String> {
    fs::read_to_string(layout.path("stub/record.jsonl"))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn launch_script(
    layout: &Layout,
    environment: &BTreeMap<String, String>,
    program: &[String],
) -> String {
    let mut env_words = String::new();
    for (key, value) in environment {
        let _ = write!(env_words, " {}", shell_quote(&format!("{key}={value}")));
    }
    let program_words: Vec<String> = program.iter().map(|word| shell_quote(word)).collect();
    format!(
        "cd {project} || exit 97\n\
         /usr/bin/env -i{env_words} {program} >{out} 2>&1 &\n\
         child=$!\n\
         printf '%s\\n' \"$child\" >{pid}\n\
         wait \"$child\"\n\
         printf '%s\\n' \"$?\" >{exit}\n",
        project = shell_quote(&layout.text("project")),
        program = program_words.join(" "),
        out = shell_quote(&layout.text("daemon.out")),
        pid = shell_quote(&layout.text("daemon.pid")),
        exit = shell_quote(&layout.text("daemon.exit")),
    )
}

/// Dedicated tmux socket for every gate session. The gate never talks to the
/// default tmux server, so it can never address or stop anyone else's session.
pub const TMUX_SOCKET: &str = "spocky-p3-gate";

fn tmux(args: &[&str]) -> (Vec<u8>, Vec<u8>, Exit) {
    run_bounded(
        Command::new("tmux").args(["-L", TMUX_SOCKET]).args(args),
        Duration::from_secs(15),
    )
}

/// The pane PID of an exact session on the dedicated socket.
fn pane_pid(session: &str) -> Option<u32> {
    let (stdout, _, exit) = tmux(&[
        "display-message",
        "-p",
        "-t",
        &format!("={session}"),
        "#{pane_pid}",
    ]);
    (exit == Exit::Code(0))
        .then(|| String::from_utf8_lossy(&stdout).trim().parse().ok())
        .flatten()
}

fn session_exists(session: &str) -> bool {
    tmux(&["has-session", "-t", &format!("={session}")]).2 == Exit::Code(0)
}

fn signal(pid: u32, name: &str) {
    let _ = run_bounded(
        Command::new("/bin/kill").args([&format!("-{name}"), &pid.to_string()]),
        Duration::from_secs(5),
    );
}

/// One process from a `ps` snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// `ps lstart`: unchanged by `exec`, so it identifies one process across
    /// the env, sandbox-exec, and wrapper hand-offs, and changes on PID reuse.
    pub start: String,
    /// `ps stat`; a leading `Z` marks an already-exited zombie.
    pub stat: String,
    pub comm: String,
}

/// Whether a process is any tmux client or server. The gate never signals one.
#[must_use]
pub fn is_tmux(process: &Proc) -> bool {
    Path::new(&process.comm)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("tmux"))
}

/// All processes, from one `ps -A -o pid=,ppid=,stat=,lstart=,comm=` snapshot.
fn snapshot() -> Vec<Proc> {
    let (stdout, _, exit) = run_bounded(
        Command::new("/bin/ps").args(["-A", "-o", "pid=,ppid=,stat=,lstart=,comm="]),
        Duration::from_secs(10),
    );
    if exit != Exit::Code(0) {
        return Vec::new();
    }
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let parent = words.next()?.parse().ok()?;
            let state = words.next()?.to_owned();
            // lstart is five words, for example `Thu Oct  1 17:22:45 2026`.
            let start: Vec<&str> = words.by_ref().take(5).collect();
            if start.len() != 5 {
                return None;
            }
            let comm = words.collect::<Vec<_>>().join(" ");
            Some(Proc {
                pid,
                ppid: parent,
                start: start.join(" "),
                stat: state,
                comm,
            })
        })
        .collect()
}

/// Processes this gate run started: roots it spawned or launched, plus every
/// descendant seen while its parent was owned. A tmux process is never owned,
/// even as a descendant, and neither is this harness process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Owned {
    /// (pid, start time when first seen); the start time guards against PID reuse.
    pub processes: Vec<(u32, String)>,
}

impl Owned {
    /// Adds a process this run started directly.
    pub fn add_root(&mut self, pid: u32, snapshot: &[Proc]) {
        if let Some(process) = snapshot.iter().find(|process| process.pid == pid) {
            self.insert(process);
        }
    }

    fn insert(&mut self, process: &Proc) {
        if is_tmux(process)
            || process.pid == std::process::id()
            || self.processes.iter().any(|(pid, _)| *pid == process.pid)
        {
            return;
        }
        self.processes.push((process.pid, process.start.clone()));
    }

    /// Adds every not-yet-owned child of an owned process, to a fixed point.
    pub fn extend(&mut self, snapshot: &[Proc]) {
        loop {
            let before = self.processes.len();
            for process in snapshot {
                if self.owns_live(process.ppid, snapshot) {
                    self.insert(process);
                }
            }
            if self.processes.len() == before {
                return;
            }
        }
    }

    /// Whether `pid` is owned and still the same process (same start time).
    fn owns_live(&self, pid: u32, snapshot: &[Proc]) -> bool {
        self.processes.iter().any(|(owned, start)| {
            *owned == pid
                && snapshot
                    .iter()
                    .any(|process| process.pid == pid && process.start == *start)
        })
    }

    #[must_use]
    pub fn pids(&self) -> Vec<u32> {
        self.processes.iter().map(|(pid, _)| *pid).collect()
    }
}

/// The only processes the stop sweep may signal: owned, alive (not a zombie)
/// with the same start time, not tmux, not in `keep`, and not this harness.
#[must_use]
pub fn sweep_targets(owned: &Owned, snapshot: &[Proc], keep: &[u32]) -> Vec<u32> {
    snapshot
        .iter()
        .filter(|process| {
            !is_tmux(process)
                && !process.stat.starts_with('Z')
                && !keep.contains(&process.pid)
                && process.pid != std::process::id()
                && owned.owns_live(process.pid, snapshot)
        })
        .map(|process| process.pid)
        .collect()
}

/// The PID to send SIGTERM: the recorded daemon PID only if it is owned,
/// alive with the same start time, not tmux, and not in `keep`.
#[must_use]
pub fn term_target(
    daemon_pid: Option<u32>,
    owned: &Owned,
    snapshot: &[Proc],
    keep: &[u32],
) -> Option<u32> {
    daemon_pid.filter(|pid| sweep_targets(owned, snapshot, keep).contains(pid))
}

/// SIGKILLs every sweep target, waits, and returns (killed, survivors).
fn kill_owned(sampler: &Sampler, keep: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let killed = sweep_targets(&sampler.owned(), &snapshot(), keep);
    for pid in &killed {
        signal(*pid, "KILL");
    }
    wait_until(KILL_GRACE, || {
        sweep_targets(&sampler.owned(), &snapshot(), keep).is_empty()
    });
    let survivors = sweep_targets(&sampler.owned(), &snapshot(), keep);
    (killed, survivors)
}

/// Samples the process table every 100 ms while a side runs, growing the
/// owned set so short-lived descendants are tracked for the stop sweep and
/// the egress check.
struct Sampler {
    owned: std::sync::Arc<std::sync::Mutex<Owned>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Sampler {
    fn start() -> Self {
        let owned = std::sync::Arc::new(std::sync::Mutex::new(Owned::default()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = {
            let owned = std::sync::Arc::clone(&owned);
            let stop = std::sync::Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let table = snapshot();
                    if let Ok(mut owned) = owned.lock() {
                        owned.extend(&table);
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            })
        };
        Self {
            owned,
            stop,
            handle: Some(handle),
        }
    }

    fn add_root(&self, pid: u32) {
        let table = snapshot();
        if let Ok(mut owned) = self.owned.lock() {
            owned.add_root(pid, &table);
            owned.extend(&table);
        }
    }

    fn owned(&self) -> Owned {
        self.owned
            .lock()
            .map(|owned| owned.clone())
            .unwrap_or_default()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// PIDs every codex invocation recorded through the wrapper (`exec` keeps it).
fn codex_pids(layout: &Layout) -> Vec<u32> {
    let Ok(entries) = fs::read_dir(layout.path("codex-io")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| fs::read_to_string(entry.ok()?.path().join("pid")).ok())
        .filter_map(|text| text.trim().parse::<u32>().ok())
        .collect()
}

/// Kernel sandbox denials of outbound connections by any of `pids` within
/// the last `window`. Each returned line fails the gate.
#[must_use]
pub fn egress_violations(window: Duration, pids: &[u32]) -> Vec<String> {
    let (stdout, stderr, exit) = run_bounded(
        Command::new("/usr/bin/log").args([
            "show",
            "--last",
            &format!("{}s", window.as_secs().max(1)),
            "--style",
            "compact",
            "--predicate",
            "eventMessage CONTAINS \"deny\" AND eventMessage CONTAINS \"network-outbound\"",
        ]),
        Duration::from_secs(120),
    );
    if exit != Exit::Code(0) {
        return vec![format!(
            "egress check could not read the kernel log: {} {}",
            exit.render(),
            String::from_utf8_lossy(&stderr)
        )];
    }
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter(|line| {
            line.split("Sandbox: ").nth(1).is_some_and(|rest| {
                rest.split_once(") deny")
                    .and_then(|(head, _)| head.rsplit_once('('))
                    .and_then(|(_, pid)| pid.parse::<u32>().ok())
                    .is_some_and(|pid| pids.contains(&pid))
            })
        })
        .map(str::to_owned)
        .collect()
}

/// PIDs of this user's processes whose arguments or environment contain
/// `needle` (macOS `ps -E`), excluding this harness process. Used only to
/// report unowned processes; the gate never signals a PID from this scan.
fn processes_mentioning(needle: &str) -> Vec<u32> {
    let (stdout, _, exit) = run_bounded(
        Command::new("/bin/ps").args(["-A", "-E", "-ww", "-o", "pid=,command="]),
        Duration::from_secs(10),
    );
    if exit != Exit::Code(0) {
        return Vec::new();
    }
    let own = std::process::id();
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .filter(|pid| *pid != own)
        .collect()
}

fn wait_until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if done() {
            return true;
        }
        if started.elapsed() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Collects files under `directory` (relative to the root), sorted by path.
fn collect_files(layout: &Layout, directory: &str, out: &mut Vec<CapturedFile>) -> io::Result<()> {
    collect_files_except(layout, directory, &[], out)
}

/// Like [`collect_files`], skipping directories whose root-relative path is in `skip`.
fn collect_files_except(
    layout: &Layout,
    directory: &str,
    skip: &[&str],
    out: &mut Vec<CapturedFile>,
) -> io::Result<()> {
    let base = layout.path(directory);
    if !base.exists() {
        return Ok(());
    }
    let mut pending = vec![base];
    let mut found = Vec::new();
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                let relative = path.strip_prefix(&layout.root).map_err(io::Error::other)?;
                if !skip.iter().any(|skipped| relative == Path::new(skipped)) {
                    pending.push(path);
                }
            } else {
                found.push((path, kind));
            }
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, kind) in found {
        let relative = path
            .strip_prefix(&layout.root)
            .map_err(io::Error::other)?
            .display()
            .to_string();
        // Never open FIFOs, sockets, or devices: reading one can block forever.
        let bytes = if kind.is_symlink() {
            format!("symlink -> {}", fs::read_link(&path)?.display()).into_bytes()
        } else if kind.is_file() {
            fs::read(&path)?
        } else {
            b"special file".to_vec()
        };
        out.push(CapturedFile {
            path: relative,
            bytes,
        });
    }
    Ok(())
}

/// One compared record per codex invocation: its argv and the exact bytes the
/// daemon wrote to its stdin. The wrapper's arrival number is dropped, because
/// concurrent `--version` probes and app-server starts race for it; canonical
/// state ordering then orders invocations by content.
fn codex_invocations(layout: &Layout) -> io::Result<Vec<CapturedFile>> {
    let mut invocations = Vec::new();
    for entry in fs::read_dir(layout.path("codex-io"))? {
        let directory = entry?.path();
        let argv = fs::read(directory.join("argv"))?;
        let stdin = fs::read(directory.join("stdin"))?;
        let mut bytes = b"argv:\n".to_vec();
        bytes.extend_from_slice(&argv);
        bytes.extend_from_slice(b"stdin:\n");
        bytes.extend_from_slice(&stdin);
        invocations.push(CapturedFile {
            path: "codex-io/invocation".into(),
            bytes,
        });
    }
    Ok(invocations)
}

fn is_daemon_log(path: &str) -> bool {
    path.strip_prefix("paseo-home/")
        .is_some_and(|name| name.starts_with("daemon.log"))
}

fn capture_files(
    layout: &Layout,
    errors: &mut Vec<String>,
) -> (Vec<CapturedFile>, Vec<CapturedFile>) {
    let mut all = Vec::new();
    for directory in ["paseo-home", "home"] {
        if let Err(error) = collect_files(layout, directory, &mut all) {
            errors.push(format!("capture {directory}: {error}"));
        }
    }
    if let Err(error) = collect_files_except(layout, "project", &["project/.git"], &mut all) {
        errors.push(format!("capture project: {error}"));
    }
    match codex_invocations(layout) {
        Ok(invocations) => all.extend(invocations),
        Err(error) => errors.push(format!("capture codex-io: {error}")),
    }
    for (name, args) in [
        ("project.git-status", &["status", "--porcelain"][..]),
        ("project.git-head", &["rev-parse", "HEAD"]),
    ] {
        let (stdout, stderr, exit) =
            run_bounded(&mut git_command(layout, args), Duration::from_secs(30));
        let mut bytes = format!("{}\n", exit.render()).into_bytes();
        bytes.extend_from_slice(&stdout);
        bytes.extend_from_slice(&stderr);
        all.push(CapturedFile {
            path: name.into(),
            bytes,
        });
    }
    let mut tmp = Vec::new();
    match collect_files(layout, "tmp", &mut tmp) {
        Ok(()) => all.push(CapturedFile {
            path: "tmp.listing".into(),
            bytes: tmp
                .iter()
                .fold(String::new(), |mut listing, file| {
                    listing.push_str(&file.path);
                    listing.push('\n');
                    listing
                })
                .into_bytes(),
        }),
        Err(error) => errors.push(format!("capture tmp listing: {error}")),
    }
    let (uncompared_logs, mut state): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|file| is_daemon_log(&file.path));
    match fs::read(layout.path("codex-home/config.toml")) {
        Ok(bytes) => state.push(CapturedFile {
            path: "codex-home/config.toml".into(),
            bytes,
        }),
        Err(error) => errors.push(format!("capture codex config: {error}")),
    }
    let mut uncompared = uncompared_logs;
    for name in ["daemon.out", "stub/stub.log"] {
        if let Ok(bytes) = fs::read(layout.path(name)) {
            uncompared.push(CapturedFile {
                path: name.into(),
                bytes,
            });
        }
    }
    let mut codex = Vec::new();
    if collect_files(layout, "codex-home", &mut codex).is_ok() {
        let listing = codex.iter().fold(String::new(), |mut listing, file| {
            listing.push_str(&file.path);
            listing.push('\n');
            listing
        });
        uncompared.push(CapturedFile {
            path: "codex-home.listing".into(),
            bytes: listing.into_bytes(),
        });
    }
    (state, uncompared)
}

fn extract_secrets(state: &[CapturedFile]) -> Vec<(String, String)> {
    let mut extracted = Vec::new();
    for file in state {
        match file.path.as_str() {
            "paseo-home/daemon-keypair.json" => {
                if let Ok(value) = serde_json::from_slice::<Value>(&file.bytes) {
                    for (key, class) in [
                        ("publicKeyB64", "daemon-public-key"),
                        ("secretKeyB64", "daemon-secret-key"),
                    ] {
                        if let Some(Value::String(secret)) = value.get(key) {
                            extracted.push((class.to_owned(), secret.clone()));
                        }
                    }
                }
            }
            "paseo-home/local-credential" => {
                let text = String::from_utf8_lossy(&file.bytes).trim().to_owned();
                if !text.is_empty() {
                    extracted.push(("local-credential".to_owned(), text));
                }
            }
            _ => {}
        }
    }
    extracted
}

fn expand_args(
    step: &StepSpec,
    layout: &Layout,
    host: &str,
    captured: &BTreeMap<&'static str, String>,
) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    for arg in &step.args {
        match arg {
            Arg::Lit(value) => argv.push((*value).to_owned()),
            Arg::Host => {
                argv.push("--host".into());
                argv.push(host.to_owned());
            }
            Arg::Project => argv.push(layout.text("project")),
            Arg::Captured(key) => match captured.get(key) {
                Some(value) => argv.push(value.clone()),
                None => return Err(format!("missing captured value {key}")),
            },
        }
    }
    Ok(argv)
}

fn cli_command(
    tools: &Tools,
    layout: &Layout,
    environment: &BTreeMap<String, String>,
    argv: &[String],
) -> Command {
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .args(["-p", EGRESS_PROFILE])
        .arg(tools.paseo_root.join("packages/cli/bin/paseo"))
        .args(argv)
        .current_dir(layout.path("project"))
        .env_clear()
        .envs(environment);
    command
}

fn not_run(name: &str, argv: Vec<String>, reason: String) -> StepRun {
    StepRun {
        name: name.into(),
        argv,
        stdout: Vec::new(),
        stderr: Vec::new(),
        exit: Exit::NotRun(reason),
        stub_requests: 0,
    }
}

/// Runs one complete side and deletes its disposable root afterwards.
///
/// `evidence` is this side's own directory. It receives `launch.json` before
/// the daemon starts, `pid.json` right after, and the raw capture after stop.
///
/// # Errors
///
/// Returns an error only when the disposable root cannot be prepared; every
/// later failure is recorded in the returned capture and fails the gate.
pub fn run_side(
    gate: &GateSpec,
    kind: DaemonKind,
    tools: &Tools,
    evidence: &Path,
) -> Result<SideRun, String> {
    let window_start_ms = now_ms();
    let layout = create_layout(gate.id, tools).map_err(|error| format!("create root: {error}"))?;
    let result = run_in_layout(gate, kind, tools, evidence, &layout, window_start_ms);
    let root = layout.root.display().to_string();
    let owned = Path::new(ROOT_PARENT).join(format!("{OWNED_PREFIX}{}-", gate.id));
    if root.starts_with(&owned.display().to_string())
        && layout.root.parent() == Some(Path::new(ROOT_PARENT))
        && let Err(error) = fs::remove_dir_all(&layout.root)
    {
        return result.map(|mut side| {
            side.harness_errors.push(format!("remove root: {error}"));
            side
        });
    }
    result
}

#[allow(clippy::too_many_lines)]
fn run_in_layout(
    gate: &GateSpec,
    kind: DaemonKind,
    tools: &Tools,
    evidence: &Path,
    layout: &Layout,
    window_start_ms: u64,
) -> Result<SideRun, String> {
    let mut errors = Vec::new();
    init_project(layout)?;
    let stub = start_stub(layout, tools, &gate.script)?;
    let daemon_port = loop {
        let port = free_port().map_err(|error| error.to_string())?;
        if port != stub.port && !FORBIDDEN_PORTS.contains(&port) {
            break port;
        }
    };
    let host = format!("127.0.0.1:{daemon_port}");
    fs::write(
        layout.path("codex-home/config.toml"),
        codex_config(stub.port),
    )
    .map_err(|error| error.to_string())?;
    let config = serde_json::to_vec(&paseo_config(layout, daemon_port, stub.port))
        .map_err(|error| error.to_string())?;
    fs::write(layout.path("paseo-home/config.json"), config).map_err(|error| error.to_string())?;

    let environment = side_environment(layout, tools);
    let unsandboxed: Vec<String> = match kind {
        DaemonKind::Original => vec![
            tools.node_bin.join("node").display().to_string(),
            tools
                .paseo_root
                .join("packages/cli/dist/index.js")
                .display()
                .to_string(),
            "daemon".into(),
            "run".into(),
        ],
        DaemonKind::Spocky => match &tools.spocky_daemon {
            Some(binary) => vec![binary.display().to_string()],
            None => return Err("spocky side requested without --spocky-daemon".into()),
        },
    };
    let program: Vec<String> = [SANDBOX_EXEC, "-p", EGRESS_PROFILE]
        .into_iter()
        .map(str::to_owned)
        .chain(unsandboxed)
        .collect();
    let session = format!("{OWNED_PREFIX}{}-{}-{}", gate.id, kind.label(), now_ms());
    fs::write(
        layout.path("launch.sh"),
        launch_script(layout, &environment, &program),
    )
    .map_err(|error| error.to_string())?;
    let side_evidence = evidence.to_path_buf();
    fs::create_dir_all(&side_evidence).map_err(|error| error.to_string())?;
    let launch = serde_json::json!({
        "gate": gate.id,
        "daemon": kind.label(),
        "session": session,
        "root": layout.text(""),
        "paseoHome": layout.text("paseo-home"),
        "daemonPort": daemon_port,
        "stubPort": stub.port,
        "logPath": layout.text("daemon.out"),
        "program": program,
        "environment": environment,
    });
    fs::write(
        side_evidence.join("launch.json"),
        serde_json::to_vec_pretty(&launch).unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;

    let launched = tmux(&[
        "new-session",
        "-d",
        "-s",
        &session,
        &format!("/bin/sh {}", shell_quote(&layout.text("launch.sh"))),
    ]);
    if launched.2 != Exit::Code(0) {
        errors.push(format!(
            "tmux new-session failed: {} {}",
            launched.2.render(),
            String::from_utf8_lossy(&launched.1)
        ));
    }
    let daemon_pid = if wait_until(Duration::from_secs(10), || {
        layout.path("daemon.pid").exists()
    }) {
        fs::read_to_string(layout.path("daemon.pid"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
    } else {
        None
    };
    let sampler = Sampler::start();
    let pane = pane_pid(&session);
    // Only the pane is a root. The daemon (read from a file) becomes owned
    // only by being the pane's descendant, never because the file names it.
    if let Some(pid) = pane {
        sampler.add_root(pid);
    }
    let _ = fs::write(
        side_evidence.join("pid.json"),
        serde_json::to_vec(&serde_json::json!({
            "session": session,
            "tmuxSocket": TMUX_SOCKET,
            "panePid": pane,
            "daemonPid": daemon_pid
        }))
        .unwrap_or_default(),
    );

    let mut pids: Vec<u32> = Vec::new();
    let mut readiness_attempts = 0;
    let ready_started = Instant::now();
    let readiness = loop {
        readiness_attempts += 1;
        let argv: Vec<String> = vec!["ls".into(), "--host".into(), host.clone(), "--json".into()];
        let left = READY_TIMEOUT
            .saturating_sub(ready_started.elapsed())
            .clamp(Duration::from_secs(1), Duration::from_secs(30));
        let (stdout, stderr, exit) = run_tracked(
            &mut cli_command(tools, layout, &environment, &argv),
            left,
            &mut pids,
        );
        let attempt = StepRun {
            name: "ready".into(),
            argv,
            stdout,
            stderr,
            exit,
            stub_requests: stub_records(layout).len(),
        };
        if attempt.exit == Exit::Code(0)
            || layout.path("daemon.exit").exists()
            || ready_started.elapsed() >= READY_TIMEOUT
        {
            break attempt;
        }
        thread::sleep(READY_INTERVAL);
    };

    let mut captured: BTreeMap<&'static str, String> = BTreeMap::new();
    captured.insert("project", layout.text("project"));
    let mut steps = Vec::new();
    for step in &gate.steps {
        if readiness.exit != Exit::Code(0) {
            steps.push(not_run(step.name, Vec::new(), "daemon not ready".into()));
            continue;
        }
        let argv = match expand_args(step, layout, &host, &captured) {
            Ok(argv) => argv,
            Err(reason) => {
                steps.push(not_run(step.name, Vec::new(), reason));
                continue;
            }
        };
        let (stdout, stderr, exit) = run_tracked(
            &mut cli_command(tools, layout, &environment, &argv),
            STEP_TIMEOUT,
            &mut pids,
        );
        if let Some((key, pointer)) = step.capture
            && let Some(Value::String(value)) = serde_json::from_slice::<Value>(&stdout)
                .ok()
                .and_then(|json| json.pointer(pointer).cloned())
        {
            captured.insert(key, value);
        }
        steps.push(StepRun {
            name: step.name.into(),
            argv,
            stdout,
            stderr,
            exit,
            stub_requests: stub_records(layout).len(),
        });
    }

    let (force_killed, survivors) = stop_daemon(
        layout,
        &session,
        daemon_pid,
        &sampler,
        &[stub.child.id()],
        &mut errors,
    );
    let owned_pids = sampler.owned().pids();
    drop(sampler);
    for pid in owned_pids {
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    let daemon_exit = fs::read_to_string(layout.path("daemon.exit"))
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .map_or(
            Exit::NotRun("daemon exit status missing".into()),
            Exit::Code,
        );
    let stub_port = stub.port;
    drop(stub);
    for pid in codex_pids(layout) {
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    // Let the kernel log flush, then fail on any denied outbound connect.
    thread::sleep(Duration::from_secs(2));
    let window =
        Duration::from_millis(now_ms().saturating_sub(window_start_ms)) + Duration::from_secs(30);
    for violation in egress_violations(window, &pids) {
        errors.push(format!("non-loopback egress attempt: {violation}"));
    }

    let records = stub_records(layout);
    let mut stub_scripted = 0;
    let mut stub_unscripted = 0;
    for line in &records {
        match serde_json::from_str::<RecordedRequest>(line) {
            Ok(entry) if entry.scripted.is_some() => stub_scripted += 1,
            Ok(_) => stub_unscripted += 1,
            Err(error) => errors.push(format!("stub record: {error}")),
        }
    }
    let (state, uncompared) = capture_files(layout, &mut errors);
    let extracted = extract_secrets(&state);
    let side = SideRun {
        kind,
        root: layout.text(""),
        session,
        daemon_port,
        stub_port,
        daemon_pid,
        window_start_ms,
        window_end_ms: now_ms(),
        readiness_attempts,
        readiness,
        steps,
        daemon_exit,
        stub_records: records,
        stub_scripted,
        stub_unscripted,
        script_len: gate.script.responses.len(),
        state,
        uncompared,
        extracted,
        preimages: (gate.preimages)(&captured),
        force_killed,
        survivors,
        harness_errors: errors,
        observed_pids: pids,
    };
    write_raw(&side, &side_evidence);
    Ok(side)
}

/// Stops the daemon and verifies nothing of the side survives.
///
/// Signals only processes this run owns (see [`Owned`] and
/// [`sweep_targets`]): never a tmux process, never `keep` (the stub), never a
/// process merely mentioning the root. Owned-looking strays that are not owned
/// are reported as harness errors and left alone. The tmux session is stopped
/// by exact name on the dedicated socket only.
fn stop_daemon(
    layout: &Layout,
    session: &str,
    daemon_pid: Option<u32>,
    sampler: &Sampler,
    keep: &[u32],
    errors: &mut Vec<String>,
) -> (Vec<u32>, Vec<u32>) {
    if layout.path("daemon.exit").exists() {
        // Already exited: its PID may belong to someone else now.
    } else {
        match term_target(daemon_pid, &sampler.owned(), &snapshot(), keep) {
            Some(pid) => signal(pid, "TERM"),
            None => errors.push(format!(
                "daemon pid {daemon_pid:?} is not an owned live process; not signalled"
            )),
        }
    }
    let exited = wait_until(STOP_GRACE, || {
        layout.path("daemon.exit").exists()
            && sweep_targets(&sampler.owned(), &snapshot(), keep).is_empty()
    });
    if !exited {
        errors.push(format!(
            "daemon tree did not exit within {} s of SIGTERM",
            STOP_GRACE.as_secs()
        ));
    }
    if session_exists(session) {
        let killed = tmux(&["kill-session", "-t", &format!("={session}")]);
        if killed.2 != Exit::Code(0) && session_exists(session) {
            errors.push(format!("tmux kill-session {session} failed"));
        }
    }
    let (force_killed, survivors) = kill_owned(sampler, keep);
    let owned = sampler.owned();
    let table = snapshot();
    for pid in processes_mentioning(&layout.text("")) {
        let unowned = !owned.pids().contains(&pid) && !keep.contains(&pid);
        let tmux_process = table
            .iter()
            .any(|process| process.pid == pid && is_tmux(process));
        if unowned && !tmux_process {
            errors.push(format!(
                "unowned process {pid} mentions the disposable root; not signalled"
            ));
        }
    }
    if session_exists(session) {
        errors.push(format!("tmux session {session} survived"));
    }
    (force_killed, survivors)
}

fn write_raw(side: &SideRun, directory: &Path) {
    let _ = fs::create_dir_all(directory);
    let _ = fs::write(
        directory.join("side.json"),
        serde_json::to_vec_pretty(side).unwrap_or_default(),
    );
    let files = directory.join("files");
    let mut written: Vec<PathBuf> = Vec::new();
    for file in side.state.iter().chain(&side.uncompared) {
        let mut target = files.join(&file.path);
        let mut copy = 1;
        while written.contains(&target) {
            copy += 1;
            target = files.join(format!("{}.{copy}", file.path));
        }
        written.push(target.clone());
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(target, &file.bytes);
    }
}

/// Evaluates a gate's positive checks on one side; returns failures.
#[must_use]
pub fn failed_checks(gate: &GateSpec, side: &SideRun) -> Vec<String> {
    let mut failures = Vec::new();
    for check in &gate.checks {
        let failure = match check {
            Check::AllExitZero => std::iter::once(&side.readiness)
                .chain(&side.steps)
                .find(|step| step.exit != Exit::Code(0))
                .map(|step| format!("{}: {}", step.name, step.exit.render())),
            Check::JsonString {
                step,
                pointer,
                expected,
            } => {
                let found = side
                    .steps
                    .iter()
                    .find(|run| run.name == *step)
                    .and_then(|run| serde_json::from_slice::<Value>(&run.stdout).ok())
                    .and_then(|json| json.pointer(pointer).cloned());
                (found != Some(Value::String((*expected).to_owned())))
                    .then(|| format!("{step}{pointer} is {found:?}, expected {expected:?}"))
            }
            Check::StdoutLine { step, line } => {
                let found = side
                    .steps
                    .iter()
                    .find(|run| run.name == *step)
                    .is_some_and(|run| {
                        String::from_utf8_lossy(&run.stdout)
                            .lines()
                            .any(|text| text == *line)
                    });
                (!found).then(|| format!("{step} stdout has no line {line:?}"))
            }
            Check::StdoutContains { step, needle } => {
                let contains = side
                    .steps
                    .iter()
                    .find(|run| run.name == *step)
                    .is_some_and(|run| String::from_utf8_lossy(&run.stdout).contains(needle));
                (!contains).then(|| format!("{step} stdout lacks {needle:?}"))
            }
            Check::StubExactlyConsumed => {
                (side.stub_scripted != side.script_len || side.stub_unscripted != 0).then(|| {
                    format!(
                        "stub consumed {} of {} scripted replies with {} unscripted requests",
                        side.stub_scripted, side.script_len, side.stub_unscripted
                    )
                })
            }
            Check::DaemonExit(code) => (side.daemon_exit != Exit::Code(*code))
                .then(|| format!("daemon {}", side.daemon_exit.render())),
        };
        if let Some(failure) = failure {
            failures.push(format!("{}: {failure}", side.kind.label()));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: u32) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("spocky-side-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn codex_wrapper_records_argv_and_exact_stdin_and_passes_through() {
        let directory = scratch(line!());
        let io = directory.join("io");
        fs::create_dir(&io).unwrap();
        // Stand-in for codex: echoes stdin and ignores its arguments.
        let fake = directory.join("fake-codex");
        fs::write(&fake, "#!/bin/sh\nexec /bin/cat\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let wrapper = directory.join("codex");
        fs::write(
            &wrapper,
            codex_wrapper_script(&io.display().to_string(), &fake.display().to_string()),
        )
        .unwrap();
        let input = b"{\"id\":1,\"method\":\"initialize\"}\n{\"b\":2,\"a\":1}\n";
        let mut child = Command::new("/bin/sh")
            .arg(&wrapper)
            .args(["app-server", "two words"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.stdout, input);
        assert!(wait_until(Duration::from_secs(5), || {
            fs::read(io.join("1/stdin")).is_ok_and(|bytes| bytes == input)
        }));
        assert_eq!(
            fs::read_to_string(io.join("1/argv")).unwrap(),
            "app-server\ntwo words\n"
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn codex_wrapper_refuses_missing_io_and_unbounded_numbering() {
        let directory = scratch(line!());
        let marker = directory.join("ran");
        let fake = directory.join("fake-codex");
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\n: >{}\n",
                shell_quote(&marker.display().to_string())
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let run_wrapper = |io: &Path| {
            let wrapper = directory.join("codex");
            fs::write(
                &wrapper,
                codex_wrapper_script(&io.display().to_string(), &fake.display().to_string()),
            )
            .unwrap();
            // Children forked concurrently by other tests can inherit this
            // pipe on macOS; only the shim's exit code matters here.
            match run_bounded(
                Command::new("/bin/sh").arg(&wrapper),
                Duration::from_secs(20),
            )
            .2
            {
                Exit::PipesHeld(code) => Exit::Code(code),
                other => other,
            }
        };
        assert_eq!(run_wrapper(&directory.join("missing")), Exit::Code(96));
        let full = directory.join("full");
        for number in 1..=MAX_CODEX_INVOCATIONS {
            fs::create_dir_all(full.join(number.to_string())).unwrap();
        }
        assert_eq!(run_wrapper(&full), Exit::Code(95));
        assert!(!marker.exists());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_loopback_connect_is_blocked_and_detected_but_loopback_is_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        let mut pids = Vec::new();
        let (_, _, loopback) = run_tracked(
            Command::new(SANDBOX_EXEC).args([
                "-p",
                EGRESS_PROFILE,
                "/usr/bin/nc",
                "-z",
                "-w",
                "3",
                "127.0.0.1",
                &port,
            ]),
            Duration::from_secs(20),
            &mut pids,
        );
        assert_eq!(loopback, Exit::Code(0));
        let loopback_pid = pids[0];
        let (_, _, outbound) = run_tracked(
            Command::new(SANDBOX_EXEC).args([
                "-p",
                EGRESS_PROFILE,
                "/usr/bin/nc",
                "-z",
                "-w",
                "3",
                "192.0.2.1",
                "443",
            ]),
            Duration::from_secs(20),
            &mut pids,
        );
        assert_ne!(outbound, Exit::Code(0));
        let outbound_pid = pids[1];
        assert!(wait_until(Duration::from_secs(30), || {
            !egress_violations(Duration::from_secs(120), &[outbound_pid]).is_empty()
        }));
        assert!(egress_violations(Duration::from_secs(120), &[loopback_pid]).is_empty());
    }

    #[test]
    fn special_files_are_never_opened() {
        let directory = scratch(line!());
        let layout = Layout {
            root: directory.clone(),
        };
        fs::create_dir(directory.join("io")).unwrap();
        fs::write(directory.join("io/plain"), b"x").unwrap();
        let fifo = directory.join("io/fifo");
        let made = Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        let mut out = Vec::new();
        collect_files(&layout, "io", &mut out).unwrap();
        let summary: Vec<(String, Vec<u8>)> = out
            .into_iter()
            .map(|file| (file.path, file.bytes))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("io/fifo".to_owned(), b"special file".to_vec()),
                ("io/plain".to_owned(), b"x".to_vec()),
            ]
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn held_pipes_fail_closed_within_the_grace() {
        let started = Instant::now();
        let (_, _, exit) = run_bounded(
            Command::new("/bin/sh").args(["-c", "/bin/sleep 30 & exit 0"]),
            Duration::from_secs(10),
        );
        assert_eq!(exit, Exit::PipesHeld(0));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b c"), r"'a'\''b c'");
    }

    #[test]
    fn bounded_run_reports_exit_signal_and_timeout() {
        let (stdout, stderr, exit) = run_bounded(
            Command::new("/bin/sh").args(["-c", "printf out; printf err >&2; exit 3"]),
            Duration::from_secs(10),
        );
        assert_eq!(
            (stdout.as_slice(), stderr.as_slice(), exit),
            (&b"out"[..], &b"err"[..], Exit::Code(3))
        );
        let (_, _, exit) = run_bounded(
            Command::new("/bin/sh").args(["-c", "kill -TERM $$"]),
            Duration::from_secs(10),
        );
        assert_eq!(exit, Exit::Signal(15));
        let (_, _, exit) = run_bounded(
            Command::new("/bin/sleep").arg("5"),
            Duration::from_millis(200),
        );
        assert_eq!(exit, Exit::TimedOut);
    }

    #[test]
    fn launch_script_records_pid_and_exit_and_clears_environment() {
        let layout = Layout {
            root: PathBuf::from("/private/tmp/spocky-p3-x"),
        };
        let script = launch_script(
            &layout,
            &BTreeMap::from([("HOME".to_owned(), "/h'x".to_owned())]),
            &["/bin/prog".to_owned(), "a b".to_owned()],
        );
        assert_eq!(
            script,
            "cd '/private/tmp/spocky-p3-x/project' || exit 97\n\
             /usr/bin/env -i 'HOME=/h'\\''x' '/bin/prog' 'a b' >'/private/tmp/spocky-p3-x/daemon.out' 2>&1 &\n\
             child=$!\n\
             printf '%s\\n' \"$child\" >'/private/tmp/spocky-p3-x/daemon.pid'\n\
             wait \"$child\"\n\
             printf '%s\\n' \"$?\" >'/private/tmp/spocky-p3-x/daemon.exit'\n"
        );
    }

    fn proc(pid: u32, parent: u32, comm: &str) -> Proc {
        Proc {
            pid,
            ppid: parent,
            start: "Thu Oct  1 17:00:00 2026".into(),
            stat: "S".into(),
            comm: comm.into(),
        }
    }

    #[test]
    fn ownership_follows_descendants_but_never_tmux_or_unrelated() {
        let table = vec![
            proc(100, 1, "/usr/local/bin/tmux"),
            proc(101, 100, "/bin/sh"),
            proc(102, 101, "node"),
            proc(103, 102, "/usr/local/Caskroom/codex/0.159.0/bin/codex"),
            proc(104, 102, "tmux"),
            proc(105, 104, "/bin/sh"),
            proc(200, 1, "/bin/sleep"),
        ];
        let mut owned = Owned::default();
        owned.add_root(101, &table);
        owned.add_root(100, &table);
        owned.extend(&table);
        assert_eq!(owned.pids(), vec![101, 102, 103]);
        let targets = sweep_targets(&owned, &table, &[103]);
        assert_eq!(targets, vec![101, 102]);
        for forbidden in [100, 104, 105, 200] {
            assert!(!targets.contains(&forbidden), "{forbidden}");
        }
    }

    #[test]
    fn zombies_are_never_targets() {
        let mut owned = Owned::default();
        let mut zombie = proc(400, 1, "node");
        owned.add_root(400, std::slice::from_ref(&zombie));
        zombie.stat = "Z".into();
        assert!(sweep_targets(&owned, &[zombie], &[]).is_empty());
    }

    #[test]
    fn reused_pid_is_not_signalled_but_an_exec_is() {
        let before = vec![proc(300, 1, "/usr/bin/env")];
        let mut owned = Owned::default();
        owned.add_root(300, &before);
        // Same process after exec: same start time, new command name.
        let exec = vec![proc(300, 1, "node")];
        assert_eq!(sweep_targets(&owned, &exec, &[]), vec![300]);
        // PID reused by another process: different start time.
        let mut reused = proc(300, 1, "node");
        reused.start = "Thu Oct  1 17:00:09 2026".into();
        assert!(sweep_targets(&owned, &[reused], &[]).is_empty());
    }

    fn alive(pid: u32) -> bool {
        snapshot()
            .iter()
            .any(|process| process.pid == pid && !process.stat.starts_with('Z'))
    }

    /// A live process whose environment mentions `needle` but that the gate
    /// did not start.
    fn decoy(needle: &str) -> std::process::Child {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .env("SPOCKY_TEST_ROOT", needle)
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(wait_until(Duration::from_secs(5), || processes_mentioning(
            needle
        )
        .contains(&pid)));
        child
    }

    #[test]
    fn sweep_kills_owned_processes_and_spares_live_decoys_and_tmux() {
        let directory = scratch(line!());
        let needle = directory.display().to_string();
        let mut unrelated = decoy(&needle);
        let unrelated_pid = unrelated.id();
        // A program named tmux inside the owned tree (a copy of sleep).
        let fake_tmux = directory.join("tmux");
        fs::copy("/bin/sleep", &fake_tmux).unwrap();
        let script = format!(
            "{} 30 & echo $! > {}; exec /bin/sleep 30",
            shell_quote(&fake_tmux.display().to_string()),
            shell_quote(&directory.join("tmux.pid").display().to_string())
        );
        let mut ours = Command::new("/bin/sh")
            .args(["-c", &script])
            .env("SPOCKY_TEST_ROOT", &needle)
            .spawn()
            .unwrap();
        let ours_pid = ours.id();
        assert!(wait_until(Duration::from_secs(5), || directory
            .join("tmux.pid")
            .exists()));
        let tmux_pid: u32 = fs::read_to_string(directory.join("tmux.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let sampler = Sampler::start();
        sampler.add_root(ours_pid);
        // Real tmux processes handed in as roots are refused too.
        for process in snapshot().iter().filter(|process| is_tmux(process)) {
            sampler.add_root(process.pid);
        }
        assert!(wait_until(Duration::from_secs(5), || alive(tmux_pid)));
        assert_eq!(sampler.owned().pids(), vec![ours_pid]);
        let (killed, survivors) = kill_owned(&sampler, &[]);
        let _ = ours.wait();
        assert_eq!(killed, vec![ours_pid]);
        assert!(survivors.is_empty());
        assert!(!alive(ours_pid));
        assert!(
            alive(unrelated_pid),
            "decoy that mentions the root must survive"
        );
        assert!(
            alive(tmux_pid),
            "a tmux process in the owned tree must survive"
        );
        signal(tmux_pid, "KILL");
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn term_refuses_a_decoy_pid_written_to_the_daemon_pid_file() {
        let directory = scratch(line!());
        let needle = directory.display().to_string();
        let mut unrelated = decoy(&needle);
        let decoy_pid = unrelated.id();
        let pid_file = directory.join("daemon.pid");
        fs::write(&pid_file, format!("{decoy_pid}\n")).unwrap();
        let recorded = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .ok();
        let mut ours = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let table = snapshot();
        let mut owned = Owned::default();
        owned.add_root(ours.id(), &table);
        owned.extend(&table);
        assert_eq!(term_target(recorded, &owned, &table, &[]), None);
        assert_eq!(
            term_target(Some(ours.id()), &owned, &table, &[]),
            Some(ours.id())
        );
        assert_eq!(
            term_target(Some(ours.id()), &owned, &table, &[ours.id()]),
            None
        );
        assert!(alive(decoy_pid));
        let _ = ours.kill();
        let _ = ours.wait();
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn sampler_tracks_short_lived_descendants() {
        let sampler = Sampler::start();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 3; /bin/sleep 3"])
            .spawn()
            .unwrap();
        sampler.add_root(child.id());
        // Root plus both sleeps (or the root exec'd into the second one).
        let tracked = wait_until(Duration::from_secs(10), || {
            sampler.owned().processes.len() >= 3
                || (sampler.owned().processes.len() >= 2
                    && child.try_wait().ok().flatten().is_some())
        });
        let _ = child.kill();
        let _ = child.wait();
        assert!(tracked, "{:?}", sampler.owned());
    }

    #[test]
    fn daemon_logs_are_split_from_compared_state() {
        assert!(is_daemon_log("paseo-home/daemon.log"));
        assert!(is_daemon_log("paseo-home/daemon.log.1"));
        assert!(!is_daemon_log("paseo-home/agents/daemon.log"));
        assert!(!is_daemon_log("home/daemon.log"));
    }

    #[test]
    fn extracts_keypair_and_credential_secrets() {
        let state = vec![
            CapturedFile {
                path: "paseo-home/daemon-keypair.json".into(),
                bytes: br#"{"v":2,"publicKeyB64":"pub=","secretKeyB64":"sec="}"#.to_vec(),
            },
            CapturedFile {
                path: "paseo-home/local-credential".into(),
                bytes: b"token\n".to_vec(),
            },
        ];
        assert_eq!(
            extract_secrets(&state),
            vec![
                ("daemon-public-key".to_owned(), "pub=".to_owned()),
                ("daemon-secret-key".to_owned(), "sec=".to_owned()),
                ("local-credential".to_owned(), "token".to_owned()),
            ]
        );
    }

    fn side_with(steps: Vec<StepRun>) -> SideRun {
        SideRun {
            kind: DaemonKind::Original,
            root: String::new(),
            session: String::new(),
            daemon_port: 1,
            stub_port: 2,
            daemon_pid: None,
            window_start_ms: 0,
            window_end_ms: 0,
            readiness_attempts: 1,
            readiness: StepRun {
                name: "ready".into(),
                argv: Vec::new(),
                stdout: b"[]\n".to_vec(),
                stderr: Vec::new(),
                exit: Exit::Code(0),
                stub_requests: 0,
            },
            steps,
            daemon_exit: Exit::Code(0),
            stub_records: Vec::new(),
            stub_scripted: 1,
            stub_unscripted: 0,
            script_len: 1,
            state: Vec::new(),
            uncompared: Vec::new(),
            extracted: Vec::new(),
            preimages: Vec::new(),
            force_killed: Vec::new(),
            survivors: Vec::new(),
            harness_errors: Vec::new(),
            observed_pids: Vec::new(),
        }
    }

    #[test]
    fn checks_fail_on_wrong_status_missing_text_and_stub_drift() {
        let gate = GateSpec {
            id: "t",
            script: Script {
                responses: Vec::new(),
            },
            steps: Vec::new(),
            preimages: |_| Vec::new(),
            checks: vec![
                Check::AllExitZero,
                Check::JsonString {
                    step: "run",
                    pointer: "/status",
                    expected: "completed",
                },
                Check::StdoutContains {
                    step: "logs",
                    needle: "READY",
                },
                Check::StdoutLine {
                    step: "logs",
                    line: "READY",
                },
                Check::StubExactlyConsumed,
                Check::DaemonExit(0),
            ],
        };
        let run = |name: &str, stdout: &[u8], exit| StepRun {
            name: name.into(),
            argv: Vec::new(),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            exit,
            stub_requests: 0,
        };
        let good = side_with(vec![
            run("run", br#"{"status":"completed"}"#, Exit::Code(0)),
            run("logs", b"READY\n", Exit::Code(0)),
        ]);
        assert!(failed_checks(&gate, &good).is_empty());

        let mut bad = side_with(vec![
            run("run", br#"{"status":"error"}"#, Exit::Code(1)),
            run("logs", b"nothing\n", Exit::Code(0)),
        ]);
        bad.stub_unscripted = 1;
        bad.daemon_exit = Exit::Code(143);
        assert_eq!(failed_checks(&gate, &bad).len(), 6);

        // The prompt echo alone contains READY but has no exact READY line.
        let echo_only = side_with(vec![
            run("run", br#"{"status":"completed"}"#, Exit::Code(0)),
            run(
                "logs",
                b"[User] Reply with the single word READY.\n",
                Exit::Code(0),
            ),
        ]);
        assert_eq!(
            failed_checks(&gate, &echo_only),
            vec!["original: logs stdout has no line \"READY\"".to_owned()]
        );
    }
}
