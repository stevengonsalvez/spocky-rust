//! PTY process lifecycle with node-pty's observable behavior, as pinned Paseo
//! uses it from `packages/server/src/terminal/terminal.ts` (node-pty
//! `1.2.0-beta.15`, `lib/unixTerminal.js` and `src/unix/pty.cc`).
//!
//! - The pseudo terminal opens with node-pty's termios (`ICRNL IXON IXANY
//!   IMAXBEL BRKINT IUTF8`, `OPOST ONLCR`, `CREAD CS8 HUPCL`, canonical echo
//!   flags, its control characters, 38400 baud) and the requested size.
//! - The child environment is the given one in its given order, with `PWD`
//!   set to the cwd and `TERM` to the terminal name, assigned in place when
//!   present and appended otherwise, like node-pty's object assignment. Rust's
//!   `Command` sorts an environment it builds, so the child starts through
//!   `/usr/bin/env -i --`, which installs the variables in order, and then the
//!   `spocky-pty-helper` binary, which repeats node-pty's spawn helper:
//!   new session, controlling terminal, `chdir`, then `execvp` of the file
//!   with `argv[0]` set to it.
//! - Output is decoded with [`Utf8Decoder`], as `setEncoding("utf8")` does.
//! - Exit is reported after the output stream closes, or 200 ms after the
//!   process exits when the stream stays open (node-pty's
//!   `DESTROY_SOCKET_TIMEOUT_MS`); output after that point is dropped. A
//!   signal death reports exit code 0 and the signal number.
//! - `write` queues without blocking the caller; `kill` defaults to `SIGHUP`
//!   and swallows errors.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::io::FdFlags;
use rustix::process::{Pid, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::{
    ControlModes, InputModes, LocalModes, OptionalActions, OutputModes, SpecialCodeIndex, Termios,
    Winsize,
};

use crate::utf8_decoder::Utf8Decoder;

/// node-pty `DESTROY_SOCKET_TIMEOUT_MS`.
pub const DESTROY_SOCKET_TIMEOUT: Duration = Duration::from_millis(200);

/// The binary that finishes the child setup, built from this crate.
pub const PTY_HELPER_NAME: &str = "spocky-pty-helper";

/// The `env` binary that installs the ordered environment.
const ENV_BINARY: &str = "/usr/bin/env";

const READ_BUFFER_BYTES: usize = 64 * 1024;

/// node-pty's resize error text.
pub const RESIZE_ERROR: &str = "resizing must be done using positive cols and rows";

/// What to spawn, as `pty.spawn(file, args, { name, cols, rows, cwd, env })`.
#[derive(Debug, Clone)]
pub struct PtySpawnOptions {
    pub file: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Environment in insertion order.
    pub env: Vec<(String, String)>,
    /// Terminal name, written to `TERM`.
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    /// Path of [`PTY_HELPER_NAME`]; see [`default_helper_path`].
    pub helper: PathBuf,
}

/// `{ exitCode, signal }` of node-pty's `onExit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyExit {
    pub exit_code: i32,
    pub signal: i32,
}

/// Events in the order node-pty emits them: data, then one exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Data(String),
    Exit(PtyExit),
}

#[derive(Debug)]
pub enum PtyError {
    Io(std::io::Error),
    Resize,
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Resize => f.write_str(RESIZE_ERROR),
        }
    }
}

impl std::error::Error for PtyError {}

impl From<std::io::Error> for PtyError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rustix::io::Errno> for PtyError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

/// The helper next to the running executable, as node-pty resolves its
/// `spawn-helper` next to its native module.
#[must_use]
pub fn default_helper_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(PTY_HELPER_NAME))
}

#[derive(Default)]
struct StreamState {
    closed: bool,
    destroyed: bool,
    exited: bool,
}

type Shared = Arc<(Mutex<StreamState>, Condvar)>;

/// A running PTY child.
pub struct Pty {
    pid: u32,
    master: File,
    input: Sender<Vec<u8>>,
    shared: Shared,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl Pty {
    /// Spawns the child and returns it with its event stream.
    ///
    /// # Errors
    ///
    /// Fails when the pseudo terminal cannot be opened or configured or the
    /// `env` binary cannot start.
    pub fn spawn(options: &PtySpawnOptions) -> Result<(Self, Receiver<PtyEvent>), PtyError> {
        let (master, slave) = open_pty(options.cols, options.rows)?;
        let mut command = Command::new(ENV_BINARY);
        command
            .arg("-i")
            .arg("--")
            .args(child_env(options).iter().map(|(key, value)| {
                let mut pair = OsString::from(key);
                pair.push("=");
                pair.push(value);
                pair
            }))
            .arg(&options.helper)
            .arg(&options.cwd)
            .arg(&options.file)
            .args(&options.args)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        let child = command.spawn()?;
        let pid = child.id();

        let (events, receiver) = mpsc::channel();
        let shared: Shared = Arc::default();
        let reader = master.try_clone()?;
        spawn_reader(reader, events.clone(), Arc::clone(&shared));
        spawn_waiter(child, events, Arc::clone(&shared));
        let input = spawn_writer(master.try_clone()?);
        Ok((
            Self {
                pid,
                master,
                input,
                shared,
            },
            receiver,
        ))
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// `write(data)`: queued, never blocks on a full PTY.
    pub fn write(&self, data: &str) {
        let _ = self.input.send(data.as_bytes().to_vec());
    }

    /// `resize(cols, rows)`.
    ///
    /// # Errors
    ///
    /// [`PtyError::Resize`] for a zero dimension; the ioctl error otherwise.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        if cols == 0 || rows == 0 {
            return Err(PtyError::Resize);
        }
        rustix::termios::tcsetwinsize(&self.master, winsize(cols, rows))?;
        Ok(())
    }

    /// `kill(signal)`: `SIGHUP` by default; errors are swallowed. A child
    /// whose exit was already observed is not signalled, so a recycled pid is
    /// never hit.
    pub fn kill(&self, signal: Option<Signal>) {
        let (lock, _) = &*self.shared;
        let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if state.exited {
            return;
        }
        let Some(pid) = i32::try_from(self.pid).ok().and_then(Pid::from_raw) else {
            return;
        };
        let _ = rustix::process::kill_process(pid, signal.unwrap_or(Signal::HUP));
    }
}

/// The environment node-pty hands the child: `env`, then `PWD` and `TERM`.
fn child_env(options: &PtySpawnOptions) -> Vec<(String, String)> {
    let mut env = options.env.clone();
    for (key, value) in [
        ("PWD", options.cwd.to_string_lossy().into_owned()),
        ("TERM", options.name.clone()),
    ] {
        match env.iter_mut().find(|(existing, _)| existing == key) {
            Some(entry) => entry.1 = value,
            None => env.push((key.to_owned(), value)),
        }
    }
    env
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// `openpty` with node-pty's termios and the initial size.
fn open_pty(cols: u16, rows: u16) -> Result<(File, OwnedFd), PtyError> {
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::io::fcntl_setfd(&master, FdFlags::CLOEXEC)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    // Keep the slave off fd 0..2: a parent started with closed stdio would
    // otherwise hand the child a descriptor its own stdio setup replaces.
    let slave = rustix::io::fcntl_dupfd_cloexec(&slave, 3)?;
    let mut termios = rustix::termios::tcgetattr(&slave)?;
    apply_node_pty_termios(&mut termios)?;
    rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios)?;
    rustix::termios::tcsetwinsize(&master, winsize(cols, rows))?;
    Ok((File::from(master), slave))
}

/// node-pty `pty.cc` termios for a UTF-8 terminal.
fn apply_node_pty_termios(termios: &mut Termios) -> Result<(), PtyError> {
    termios.input_modes = InputModes::ICRNL
        | InputModes::IXON
        | InputModes::IXANY
        | InputModes::IMAXBEL
        | InputModes::BRKINT
        | InputModes::IUTF8;
    termios.output_modes = OutputModes::OPOST | OutputModes::ONLCR;
    termios.control_modes = ControlModes::CREAD | ControlModes::CS8 | ControlModes::HUPCL;
    termios.local_modes = LocalModes::ICANON
        | LocalModes::ISIG
        | LocalModes::IEXTEN
        | LocalModes::ECHO
        | LocalModes::ECHOE
        | LocalModes::ECHOK
        | LocalModes::ECHOKE
        | LocalModes::ECHOCTL;
    let codes = &mut termios.special_codes;
    codes[SpecialCodeIndex::VEOF] = 4;
    codes[SpecialCodeIndex::VEOL] = 0xff;
    codes[SpecialCodeIndex::VEOL2] = 0xff;
    codes[SpecialCodeIndex::VERASE] = 0x7f;
    codes[SpecialCodeIndex::VWERASE] = 23;
    codes[SpecialCodeIndex::VKILL] = 21;
    codes[SpecialCodeIndex::VREPRINT] = 18;
    codes[SpecialCodeIndex::VINTR] = 3;
    codes[SpecialCodeIndex::VQUIT] = 0x1c;
    codes[SpecialCodeIndex::VSUSP] = 26;
    codes[SpecialCodeIndex::VSTART] = 17;
    codes[SpecialCodeIndex::VSTOP] = 19;
    codes[SpecialCodeIndex::VLNEXT] = 22;
    codes[SpecialCodeIndex::VDISCARD] = 15;
    codes[SpecialCodeIndex::VMIN] = 1;
    codes[SpecialCodeIndex::VTIME] = 0;
    #[cfg(target_os = "macos")]
    {
        codes[SpecialCodeIndex::VDSUSP] = 25;
        codes[SpecialCodeIndex::VSTATUS] = 20;
    }
    termios.set_input_speed(38400)?;
    termios.set_output_speed(38400)?;
    Ok(())
}

fn spawn_reader(mut master: File, events: Sender<PtyEvent>, shared: Shared) {
    // ponytail: a background process that keeps the slave open keeps this
    // thread in read() after exit is reported; it ends when that process does.
    thread::spawn(move || {
        let mut decoder = Utf8Decoder::new();
        let mut buffer = vec![0; READ_BUFFER_BYTES];
        let forward = |text: String| {
            let (lock, _) = &*shared;
            let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
            if state.destroyed {
                return false;
            }
            if !text.is_empty() {
                let _ = events.send(PtyEvent::Data(text));
            }
            true
        };
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if !forward(decoder.write(&buffer[..count])) {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if !forward(decoder.end()) {
            return;
        }
        let (lock, closed) = &*shared;
        lock.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
        closed.notify_all();
    });
}

fn spawn_waiter(mut child: Child, events: Sender<PtyEvent>, shared: Shared) {
    thread::spawn(move || {
        let exit = match child.wait() {
            Ok(status) => match (status.code(), status.signal()) {
                (Some(code), _) => PtyExit {
                    exit_code: code,
                    signal: 0,
                },
                (None, Some(signal)) => PtyExit {
                    exit_code: 0,
                    signal,
                },
                (None, None) => PtyExit {
                    exit_code: 0,
                    signal: 0,
                },
            },
            Err(_) => PtyExit {
                exit_code: 0,
                signal: 0,
            },
        };
        let (lock, closed) = &*shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        state.exited = true;
        if !state.closed {
            state = closed
                .wait_timeout_while(state, DESTROY_SOCKET_TIMEOUT, |state| !state.closed)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            state.destroyed = !state.closed;
        }
        let _ = events.send(PtyEvent::Exit(exit));
        drop(state);
    });
}

fn spawn_writer(mut master: File) -> Sender<Vec<u8>> {
    let (input, queue) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        for data in queue {
            if master.write_all(&data).is_err() {
                return;
            }
        }
    });
    input
}

/// The helper's work in the child: `spocky-pty-helper <cwd> <file> [args]`.
/// Returns only when the exec failed.
#[must_use]
pub fn run_helper(args: &[OsString]) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let (Some(cwd), Some(file)) = (args.first(), args.get(1)) else {
        return 1;
    };
    // New session, then opening the terminal makes it the controlling one,
    // as node-pty's helper does after POSIX_SPAWN_SETSID.
    let _ = rustix::process::setsid();
    if let Ok(name) = rustix::termios::ttyname(std::io::stdin(), Vec::new()) {
        let path = Path::new(std::ffi::OsStr::from_bytes(name.as_bytes()));
        let _ = File::options().read(true).write(true).open(path);
    }
    if !cwd.is_empty()
        && let Err(error) = std::env::set_current_dir(cwd)
    {
        helper_failure("chdir(2) failed.", &error);
        return 1;
    }
    let error = Command::new(file).args(&args[2..]).exec();
    helper_failure("execvp(3) failed.", &error);
    1
}

/// node-pty's helper exits silently on macOS; its Linux fork path reports
/// the failure with `perror`, which prints the bare `strerror` text.
fn helper_failure(context: &str, error: &std::io::Error) {
    if cfg!(target_os = "linux") {
        let text = error.to_string();
        let message = text.split(" (os error").next().unwrap_or(&text);
        eprintln!("{context}: {message}");
    }
}
