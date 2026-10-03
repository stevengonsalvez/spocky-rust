//! One terminal session: a PTY child, the headless emulator that mirrors it,
//! input and output, resize, titles, snapshots, and exit, following pinned
//! `createTerminal` in `packages/server/src/terminal/terminal.ts`.
//!
//! The emulator is not `Send`, so each session runs an actor thread that owns
//! it, and [`TerminalSession`] is a cheap handle that talks to the actor.
//! The actor reproduces the event-loop ordering the baseline depends on: PTY
//! data, commands and timers are handled first, then the "setImmediate"
//! input flush, then xterm's deferred parse, whose write callbacks bump the
//! state revision and notify subscribers. That keeps the replay protocol
//! intact: a subscriber that attaches while parses are pending still gets
//! those outputs, queued behind its snapshot with revisions at or below the
//! snapshot's.
//!
//! Left out on purpose: the activity tracker (DTRM-003). One documented
//! difference: the baseline can process the PTY exit before the parse of the
//! output that preceded it (a race between two Node phases); the port always
//! parses first, so exit diagnostics include the final output.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_wire::TerminalState;
use spocky_xterm::Terminal;

use crate::exit_lines::{EXIT_OUTPUT_LINE_LIMIT, RecentOutput, last_output_lines_from_text};
use crate::handlers::{ParserEvents, register_handlers};
use crate::input_mode::InputModeTracker;
use crate::process_title::{initial_title, js_trim};
use crate::pty::{Pty, PtyError, PtyEvent, PtySpawnOptions};
use crate::restore::{SnapshotMode, SnapshotOptions};
use crate::snapshot::{extract_state, last_output_lines};
use crate::terminal_env::{
    TerminalEnvironmentInput, build_terminal_environment, prepare_zsh_runtime_dir,
};

/// `TERMINAL_TITLE_DEBOUNCE_MS`.
const TITLE_DEBOUNCE: Duration = Duration::from_millis(150);
/// The wait `kill()` gives the process before it disposes the session.
const KILL_DISPOSE_TIMEOUT: Duration = Duration::from_millis(1000);
/// The pause `createTerminal` takes so the shell can initialize.
const SHELL_INIT_DELAY: Duration = Duration::from_millis(50);
/// Bound on every round trip to the actor.
const ACTOR_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// `CreateTerminalOptions` plus what the baseline reads from its process.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub id: Option<String>,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub shell: Option<String>,
    pub env: JsObject,
    pub activity_env: JsObject,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// The daemon's `process.env`.
    pub process_env: JsObject,
    /// `resolvePaseoCliBinDir()` and `resolvePaseoCliExecutablePath()`.
    pub paseo_cli_bin_dir: Option<String>,
    pub paseo_hook_cli_path: Option<String>,
    /// `resolveZshShellIntegrationDir()`; only read for a zsh shell.
    pub zsh_integration_dir: Option<PathBuf>,
    /// `tmpdir()`, the user name, and the daemon pid that name the zsh
    /// runtime directory.
    pub tmpdir: PathBuf,
    pub username: String,
    pub pid: u32,
    /// The daemon's working directory, for `path.resolve`.
    pub process_cwd: String,
    /// The [`crate::pty::PTY_HELPER_NAME`] binary.
    pub helper: PathBuf,
}

/// `ClientMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Input(String),
    Resize { rows: u16, cols: u16 },
    Mouse,
}

/// `ServerMessage`.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Output {
        data: String,
        revision: u64,
    },
    Snapshot {
        state: Box<TerminalState>,
        revision: u64,
    },
    SnapshotReady {
        revision: u64,
        replay_preamble: String,
    },
    TitleChange {
        title: Option<String>,
    },
}

/// `TerminalExitInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub last_output_lines: Vec<String>,
}

/// `TerminalStateSnapshot`.
#[derive(Debug, Clone, PartialEq)]
pub struct StateSnapshot {
    pub state: TerminalState,
    pub revision: u64,
}

/// Why a session could not start.
#[derive(Debug)]
pub enum SessionError {
    Pty(PtyError),
    Io(std::io::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pty(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SessionError {}

type MessageListener = Box<dyn FnMut(ServerMessage) + Send>;
type ExitListener = Box<dyn FnOnce(ExitInfo) + Send>;
type CommandFinishedListener = Box<dyn FnMut(Option<f64>) + Send>;
type TitleListener = Box<dyn FnMut(Option<&str>) + Send>;

enum Command {
    Send(ClientMessage),
    Subscribe {
        id: u64,
        listener: MessageListener,
        mode: SnapshotMode,
    },
    Unsubscribe(u64),
    OnExit(ExitListener),
    OnCommandFinished(u64, CommandFinishedListener),
    OnTitleChange(u64, TitleListener),
    RemoveListener(u64),
    SetTitle(String),
    GetState(SnapshotOptions, Sender<StateSnapshot>),
    GetReplayPreamble(Sender<String>),
    Kill,
    KillPty(Option<rustix::process::Signal>),
}

enum Event {
    Command(Command),
    Pty(PtyEvent),
}

#[derive(Default)]
struct Mirror {
    title: Option<String>,
    exit_info: Option<ExitInfo>,
    rows: u16,
    cols: u16,
    process_exited: bool,
}

type SharedMirror = Arc<(Mutex<Mirror>, Condvar)>;

/// A handle to a running terminal session; clones share the session.
#[derive(Clone)]
pub struct TerminalSession {
    pub id: String,
    pub name: String,
    pub cwd: PathBuf,
    pub workspace_id: String,
    tx: Sender<Event>,
    mirror: SharedMirror,
    next_listener: Arc<AtomicU64>,
}

impl std::fmt::Debug for TerminalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn lock(mirror: &SharedMirror) -> std::sync::MutexGuard<'_, Mirror> {
    mirror.0.lock().unwrap_or_else(PoisonError::into_inner)
}

fn env_pairs(env: &JsObject) -> Vec<(String, String)> {
    env.iter()
        .filter_map(|(key, value)| Some((key.to_owned(), value.as_str()?.to_owned())))
        .collect()
}

impl TerminalSession {
    /// `createTerminal(options)`.
    ///
    /// # Errors
    ///
    /// The PTY or zsh integration error that stopped the spawn.
    pub fn create(options: SessionOptions) -> Result<Self, SessionError> {
        let rows = options.rows.unwrap_or(24);
        let cols = options.cols.unwrap_or(80);
        let name = options
            .name
            .clone()
            .unwrap_or_else(|| "Terminal".to_owned());
        let id = options
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let shell = options.shell.clone().unwrap_or_else(|| {
            options
                .process_env
                .get("SHELL")
                .and_then(JsValue::as_str)
                .filter(|shell| !shell.is_empty())
                .unwrap_or("/bin/sh")
                .to_owned()
        });
        let (file, args) = match &options.command {
            Some(command) if !command.is_empty() => (command.clone(), options.args.clone()),
            _ => (shell, Vec::new()),
        };

        let mut env = options.env.clone();
        for (key, value) in options.activity_env.iter() {
            env.insert(key, value.clone());
        }
        env.insert(
            "PASEO_WORKSPACE_ID",
            JsValue::String(options.workspace_id.clone()),
        );
        let built = build_terminal_environment(
            &TerminalEnvironmentInput {
                shell: &file,
                process_env: &options.process_env,
                env: &env,
                paseo_cli_bin_dir: options.paseo_cli_bin_dir.as_deref(),
                paseo_hook_cli_path: options.paseo_hook_cli_path.as_deref(),
                cwd: &options.process_cwd,
            },
            || {
                let source = options.zsh_integration_dir.clone().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "zsh shell integration directory is not configured",
                    )
                })?;
                prepare_zsh_runtime_dir(&source, &options.tmpdir, &options.username, options.pid)
            },
        )
        .map_err(SessionError::Io)?;

        let (pty, pty_events) = Pty::spawn(&PtySpawnOptions {
            file: file.clone(),
            args: args.clone(),
            cwd: options.cwd.clone(),
            env: env_pairs(&built),
            name: "xterm-256color".to_owned(),
            cols,
            rows,
            helper: options.helper.clone(),
        })
        .map_err(SessionError::Pty)?;

        let preset = options.title.as_deref();
        let title = initial_title(preset, options.command.as_deref(), &options.args);
        let manual = preset.is_some_and(|title| !js_trim(title).is_empty());
        let mirror: SharedMirror = Arc::new((
            Mutex::new(Mirror {
                title: title.clone(),
                rows,
                cols,
                ..Mirror::default()
            }),
            Condvar::new(),
        ));
        let (tx, rx) = mpsc::channel();
        let bridge = tx.clone();
        thread::spawn(move || {
            for event in pty_events {
                if bridge.send(Event::Pty(event)).is_err() {
                    return;
                }
            }
        });
        let actor_mirror = Arc::clone(&mirror);
        thread::spawn(move || {
            Actor::new(pty, rows, cols, title, manual, actor_mirror).run(&rx);
        });
        // Let the shell initialize, as createTerminal does.
        thread::sleep(SHELL_INIT_DELAY);
        Ok(Self {
            id,
            name,
            cwd: options.cwd,
            workspace_id: options.workspace_id,
            tx,
            mirror,
            next_listener: Arc::new(AtomicU64::new(1)),
        })
    }

    fn command(&self, command: Command) {
        let _ = self.tx.send(Event::Command(command));
    }

    fn ask<R>(&self, build: impl FnOnce(Sender<R>) -> Command) -> Option<R> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Event::Command(build(reply))).ok()?;
        answer.recv_timeout(ACTOR_REPLY_TIMEOUT).ok()
    }

    fn listener_id(&self) -> u64 {
        self.next_listener.fetch_add(1, Ordering::Relaxed)
    }

    /// `send(msg)`.
    pub fn send(&self, message: ClientMessage) {
        self.command(Command::Send(message));
    }

    /// `subscribe(listener, { initialSnapshot })`; returns the id for
    /// [`Self::unsubscribe`].
    pub fn subscribe(
        &self,
        listener: impl FnMut(ServerMessage) + Send + 'static,
        mode: SnapshotMode,
    ) -> u64 {
        let id = self.listener_id();
        self.command(Command::Subscribe {
            id,
            listener: Box::new(listener),
            mode,
        });
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.command(Command::Unsubscribe(id));
    }

    /// `onExit(listener)`: called once with the exit info, also when the
    /// session already exited.
    pub fn on_exit(&self, listener: impl FnOnce(ExitInfo) + Send + 'static) {
        self.command(Command::OnExit(Box::new(listener)));
    }

    /// `onCommandFinished(listener)`; returns the id for
    /// [`Self::remove_listener`].
    pub fn on_command_finished(&self, listener: impl FnMut(Option<f64>) + Send + 'static) -> u64 {
        let id = self.listener_id();
        self.command(Command::OnCommandFinished(id, Box::new(listener)));
        id
    }

    /// `onTitleChange(listener)`: also called once with the current title
    /// when there is one.
    pub fn on_title_change(&self, listener: impl FnMut(Option<&str>) + Send + 'static) -> u64 {
        let id = self.listener_id();
        self.command(Command::OnTitleChange(id, Box::new(listener)));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        self.command(Command::RemoveListener(id));
    }

    /// `getSize()`.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let mirror = lock(&self.mirror);
        (mirror.rows, mirror.cols)
    }

    /// `getTitle()`.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        lock(&self.mirror).title.clone()
    }

    /// `getExitInfo()`.
    #[must_use]
    pub fn exit_info(&self) -> Option<ExitInfo> {
        lock(&self.mirror).exit_info.clone()
    }

    /// `getStateSnapshot(options)`; `None` once the actor is gone.
    #[must_use]
    pub fn state_snapshot(&self, options: SnapshotOptions) -> Option<StateSnapshot> {
        self.ask(|reply| Command::GetState(options, reply))
    }

    /// `getState(options)`.
    #[must_use]
    pub fn state(&self, options: SnapshotOptions) -> Option<TerminalState> {
        self.state_snapshot(options).map(|snapshot| snapshot.state)
    }

    /// `getReplayPreamble()`.
    #[must_use]
    pub fn replay_preamble(&self) -> String {
        self.ask(Command::GetReplayPreamble).unwrap_or_default()
    }

    /// `setTitle(title)`.
    pub fn set_title(&self, title: &str) {
        self.command(Command::SetTitle(title.to_owned()));
    }

    /// `kill()`: signals the process and emits exit now; the session is
    /// disposed when the process exits or after one second.
    pub fn kill(&self) {
        self.command(Command::Kill);
    }

    /// `killAndWait(options)`: `SIGHUP`, then `SIGKILL` after
    /// `graceful_timeout`, each wait bounded.
    pub fn kill_and_wait(&self, graceful_timeout: Duration, force_timeout: Duration) {
        if lock(&self.mirror).process_exited {
            self.kill();
            return;
        }
        self.command(Command::KillPty(None));
        if !self.wait_for_process_exit(graceful_timeout) {
            self.command(Command::KillPty(Some(rustix::process::Signal::KILL)));
            self.wait_for_process_exit(force_timeout);
        }
        self.kill();
    }

    fn wait_for_process_exit(&self, timeout: Duration) -> bool {
        let guard = lock(&self.mirror);
        let (guard, _) = self
            .mirror
            .1
            .wait_timeout_while(guard, timeout, |mirror| !mirror.process_exited)
            .unwrap_or_else(PoisonError::into_inner);
        guard.process_exited
    }
}

struct Subscription {
    id: u64,
    listener: MessageListener,
    mode: SnapshotMode,
    snapshot_delivered: bool,
    queued: Vec<ServerMessage>,
}

enum WriteItem {
    Output(String),
    /// `terminal.write("", cb)` for the subscription with this id.
    Barrier(u64),
}

enum Microtask {
    TitleReplay(u64),
    ExitReplay(ExitListener),
}

/// One flag per baseline `createTerminal` state variable.
#[allow(clippy::struct_excessive_bools)]
struct Actor {
    terminal: Terminal,
    events: ParserEvents,
    pty: Pty,
    mirror: SharedMirror,
    title: Option<String>,
    title_auto: Rc<Cell<bool>>,
    parsed_titles: Rc<RefCell<Vec<String>>>,
    /// The OSC title waiting out the debounce; `None` with a deadline set
    /// is a pending `undefined`.
    pending_title: Option<String>,
    title_deadline: Option<Instant>,
    killed: bool,
    disposed: bool,
    exit_emitted: bool,
    process_exited: bool,
    exit_info: Option<ExitInfo>,
    kill_deadline: Option<Instant>,
    recent: RecentOutput,
    pending_input: String,
    input_flush: bool,
    revision: u64,
    input_mode: InputModeTracker,
    subs: Vec<Subscription>,
    exit_listeners: Vec<ExitListener>,
    command_listeners: Vec<(u64, CommandFinishedListener)>,
    title_listeners: Vec<(u64, TitleListener)>,
    write_queue: VecDeque<WriteItem>,
    wedged: bool,
    microtasks: Vec<Microtask>,
}

impl Actor {
    fn new(
        pty: Pty,
        rows: u16,
        cols: u16,
        title: Option<String>,
        manual_title: bool,
        mirror: SharedMirror,
    ) -> Self {
        let mut terminal = Terminal::new(u32::from(cols), u32::from(rows));
        let events = register_handlers(&mut terminal);
        let title_auto = Rc::new(Cell::new(!manual_title));
        let parsed_titles = Rc::new(RefCell::new(Vec::new()));
        let (active, sink) = (Rc::clone(&title_auto), Rc::clone(&parsed_titles));
        terminal.on_title_change(move |title| {
            if active.get() {
                sink.borrow_mut().push(title.to_owned());
            }
        });
        let actor = Self {
            terminal,
            events,
            pty,
            mirror,
            title,
            title_auto,
            parsed_titles,
            pending_title: None,
            title_deadline: None,
            killed: false,
            disposed: false,
            exit_emitted: false,
            process_exited: false,
            exit_info: None,
            kill_deadline: None,
            recent: RecentOutput::default(),
            pending_input: String::new(),
            input_flush: false,
            revision: 0,
            input_mode: InputModeTracker::new(),
            subs: Vec::new(),
            exit_listeners: Vec::new(),
            command_listeners: Vec::new(),
            title_listeners: Vec::new(),
            write_queue: VecDeque::new(),
            wedged: false,
            microtasks: Vec::new(),
        };
        actor.sync_size();
        actor
    }

    fn run(mut self, rx: &Receiver<Event>) {
        loop {
            let first = match self.next_deadline() {
                Some(deadline) => {
                    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok(event) => Some(event),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match rx.recv() {
                    Ok(event) => Some(event),
                    Err(_) => return,
                },
            };
            if let Some(event) = first {
                self.handle(event);
                while let Ok(event) = rx.try_recv() {
                    self.handle(event);
                }
            }
            self.run_immediate();
            self.run_write_queue();
            self.fire_timers();
            self.run_microtasks();
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        [self.title_deadline, self.kill_deadline]
            .into_iter()
            .flatten()
            .min()
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Pty(PtyEvent::Data(data)) => self.on_data(data),
            Event::Pty(PtyEvent::Exit(exit)) => {
                // ponytail: the baseline can take the exit before the parse of
                // the output before it; parse first so exit lines are whole.
                self.run_immediate();
                self.run_write_queue();
                self.on_pty_exit(exit.exit_code, exit.signal);
            }
            Event::Command(command) => self.on_command(command),
        }
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Send(message) => self.on_send(message),
            Command::Subscribe { id, listener, mode } => {
                self.subs.push(Subscription {
                    id,
                    listener,
                    mode,
                    snapshot_delivered: false,
                    queued: Vec::new(),
                });
                self.write_queue.push_back(WriteItem::Barrier(id));
            }
            Command::Unsubscribe(id) => self.subs.retain(|sub| sub.id != id),
            Command::OnExit(listener) => {
                if self.killed {
                    self.microtasks.push(Microtask::ExitReplay(listener));
                } else {
                    self.exit_listeners.push(listener);
                }
            }
            Command::OnCommandFinished(id, listener) => {
                self.command_listeners.push((id, listener));
            }
            Command::OnTitleChange(id, listener) => {
                self.title_listeners.push((id, listener));
                if self.title.is_some() {
                    self.microtasks.push(Microtask::TitleReplay(id));
                }
            }
            Command::RemoveListener(id) => {
                self.command_listeners
                    .retain(|(listener, _)| *listener != id);
                self.title_listeners.retain(|(listener, _)| *listener != id);
            }
            Command::SetTitle(title) => self.set_title(&title),
            Command::GetState(options, reply) => {
                let _ = reply.send(StateSnapshot {
                    state: self.state(&options),
                    revision: self.revision,
                });
            }
            Command::GetReplayPreamble(reply) => {
                let _ = reply.send(self.input_mode.preamble());
            }
            Command::Kill => self.kill(),
            Command::KillPty(signal) => self.pty.kill(signal),
        }
    }

    fn state(&self, options: &SnapshotOptions) -> TerminalState {
        extract_state(&self.terminal, options, self.title.as_deref())
    }

    fn on_data(&mut self, data: String) {
        if self.killed {
            return;
        }
        for response in self.input_mode.feed(&data).responses {
            self.pty.write(&response);
        }
        self.recent.push(&data);
        self.write_queue.push_back(WriteItem::Output(data));
    }

    fn on_send(&mut self, message: ClientMessage) {
        if self.killed {
            return;
        }
        match message {
            ClientMessage::Input(data) => {
                self.pending_input.push_str(&data);
                self.input_flush = true;
            }
            ClientMessage::Resize { rows, cols } => {
                self.flush_pending_input();
                // Both calls can throw in the baseline; send() has no result
                // to carry that, so a failed resize is dropped.
                let _ = self.terminal.resize(u32::from(cols), u32::from(rows));
                let _ = self.pty.resize(cols, rows);
                self.revision += 1;
                self.sync_size();
            }
            ClientMessage::Mouse => {}
        }
    }

    /// `getSize()` mirrors the emulator's size, which has minimums.
    fn sync_size(&self) {
        let mut mirror = lock(&self.mirror);
        mirror.rows = u16::try_from(self.terminal.rows()).unwrap_or(u16::MAX);
        mirror.cols = u16::try_from(self.terminal.cols()).unwrap_or(u16::MAX);
    }

    fn flush_pending_input(&mut self) {
        self.input_flush = false;
        let data = std::mem::take(&mut self.pending_input);
        if data.is_empty() || self.killed || self.disposed {
            return;
        }
        self.pty.write(&data);
    }

    /// `setImmediate`: the input flush that follows the poll phase.
    fn run_immediate(&mut self) {
        if self.input_flush {
            self.flush_pending_input();
        }
    }

    /// xterm's deferred parse and its write callbacks.
    fn run_write_queue(&mut self) {
        while !self.wedged {
            let Some(item) = self.write_queue.pop_front() else {
                return;
            };
            match item {
                WriteItem::Output(data) => self.write_output(&data),
                WriteItem::Barrier(id) => self.finish_barrier(id),
            }
        }
    }

    fn write_output(&mut self, data: &str) {
        if self.terminal.write(data).is_err() {
            // A write that throws leaves xterm's queue stuck for good.
            self.wedged = true;
            return;
        }
        self.drain_parser_events();
        if self.disposed || self.killed {
            return;
        }
        self.revision += 1;
        let revision = self.revision;
        for sub in &mut self.subs {
            deliver(
                sub,
                ServerMessage::Output {
                    data: data.to_owned(),
                    revision,
                },
            );
        }
    }

    /// Everything the parse produced: PTY replies, command completion, and
    /// OSC title changes (debounced unless the title is manual).
    fn drain_parser_events(&mut self) {
        let queued = self.events.take();
        for reply in queued.replies {
            self.pty.write(&reply);
        }
        for exit_code in queued.command_finished {
            for (_, listener) in &mut self.command_listeners {
                listener(exit_code);
            }
        }
        let titles = std::mem::take(&mut *self.parsed_titles.borrow_mut());
        for title in titles {
            if self.disposed || self.killed {
                continue;
            }
            self.pending_title = (!js_trim(&title).is_empty()).then_some(title);
            self.title_deadline = Some(Instant::now() + TITLE_DEBOUNCE);
        }
    }

    fn finish_barrier(&mut self, id: u64) {
        if self.terminal.write("").is_err() {
            self.wedged = true;
            return;
        }
        if self.disposed {
            return;
        }
        let revision = self.revision;
        let Some(index) = self.subs.iter().position(|sub| sub.id == id) else {
            return;
        };
        let mode = self.subs[index].mode;
        let first = match mode {
            SnapshotMode::Ready => ServerMessage::SnapshotReady {
                revision,
                // Carry the input-mode preamble so the snapshot-less restore
                // path can replay it without a separate state fetch.
                replay_preamble: self.input_mode.preamble(),
            },
            SnapshotMode::State => ServerMessage::Snapshot {
                state: Box::new(self.state(&SnapshotOptions::default())),
                revision,
            },
        };
        let sub = &mut self.subs[index];
        sub.snapshot_delivered = true;
        (sub.listener)(first);
        for message in std::mem::take(&mut sub.queued) {
            (sub.listener)(message);
        }
    }

    fn set_title(&mut self, next: &str) {
        let manual = js_trim(next);
        if manual.is_empty() {
            return;
        }
        // The title is now manual: stop following OSC titles.
        self.title_auto.set(false);
        self.pending_title = None;
        self.title_deadline = None;
        self.emit_title_change(Some(manual.to_owned()));
    }

    fn emit_title_change(&mut self, next: Option<String>) {
        if self.title == next {
            return;
        }
        self.title = next;
        let title = self.title.clone();
        lock(&self.mirror).title.clone_from(&title);
        for (_, listener) in &mut self.title_listeners {
            listener(title.as_deref());
        }
        for sub in &mut self.subs {
            deliver(
                sub,
                ServerMessage::TitleChange {
                    title: title.clone(),
                },
            );
        }
    }

    fn build_exit_info(&self, exit: Option<(i32, i32)>) -> ExitInfo {
        let lines = last_output_lines(&self.terminal, EXIT_OUTPUT_LINE_LIMIT);
        ExitInfo {
            exit_code: exit.map(|(code, _)| code),
            signal: exit.map(|(_, signal)| signal).filter(|signal| *signal > 0),
            last_output_lines: if lines.is_empty() {
                last_output_lines_from_text(&self.recent.tail(), EXIT_OUTPUT_LINE_LIMIT)
            } else {
                lines
            },
        }
    }

    fn emit_exit(&mut self, info: &ExitInfo) {
        if self.exit_emitted {
            return;
        }
        self.exit_emitted = true;
        lock(&self.mirror).exit_info = Some(info.clone());
        self.exit_info = Some(info.clone());
        for listener in std::mem::take(&mut self.exit_listeners) {
            listener(info.clone());
        }
    }

    fn on_pty_exit(&mut self, exit_code: i32, signal: i32) {
        self.killed = true;
        self.process_exited = true;
        {
            let mut mirror = lock(&self.mirror);
            mirror.process_exited = true;
        }
        self.mirror.1.notify_all();
        let info = self.build_exit_info(Some((exit_code, signal)));
        self.emit_exit(&info);
        self.dispose();
    }

    fn kill(&mut self) {
        if !self.killed {
            self.killed = true;
            if !self.process_exited {
                self.pty.kill(None);
            }
            let info = self.build_exit_info(None);
            self.emit_exit(&info);
        }
        if self.process_exited {
            self.dispose();
            return;
        }
        self.kill_deadline = Some(Instant::now() + KILL_DISPOSE_TIMEOUT);
    }

    fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.kill_deadline = None;
        self.pending_input.clear();
        self.input_flush = false;
        self.recent.clear();
        self.input_mode.reset();
        self.pending_title = None;
        self.title_deadline = None;
        self.title_auto.set(false);
        self.subs.clear();
        self.exit_listeners.clear();
        self.command_listeners.clear();
        self.title_listeners.clear();
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();
        if self.title_deadline.is_some_and(|deadline| deadline <= now) {
            self.title_deadline = None;
            let pending = self.pending_title.take();
            self.emit_title_change(pending);
        }
        if self.kill_deadline.is_some_and(|deadline| deadline <= now) {
            self.kill_deadline = None;
            self.dispose();
        }
    }

    fn run_microtasks(&mut self) {
        for task in std::mem::take(&mut self.microtasks) {
            match task {
                Microtask::TitleReplay(id) => {
                    if self.disposed {
                        continue;
                    }
                    let title = self.title.clone();
                    if let Some((_, listener)) = self
                        .title_listeners
                        .iter_mut()
                        .find(|(listener, _)| *listener == id)
                    {
                        listener(title.as_deref());
                    }
                }
                Microtask::ExitReplay(listener) => {
                    let info = self
                        .exit_info
                        .clone()
                        .unwrap_or_else(|| self.build_exit_info(None));
                    listener(info);
                }
            }
        }
    }
}

/// A subscription listener call: held back until its snapshot went out.
fn deliver(sub: &mut Subscription, message: ServerMessage) {
    if sub.snapshot_delivered {
        (sub.listener)(message);
    } else {
        sub.queued.push(message);
    }
}
