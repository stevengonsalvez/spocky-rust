//! The terminal registry, following pinned
//! `packages/server/src/terminal/terminal-manager.ts` (`createTerminalManager`).
//!
//! Sessions are indexed by id and by exact cwd, queries for a workspace root
//! aggregate every cwd at or below it, new sessions inherit the environment
//! registered for their closest root, and a session that exits removes itself
//! and announces the change. In the baseline the worker process serializes
//! `createTerminal` requests, so creation here takes one lock; the activity
//! tracker (DTRM-003) is not part of this port.
//!
//! No manager lock is held while a session is called: session calls can wait
//! for the session thread, and that thread calls back into the manager when a
//! session exits or retitles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue};

use crate::capture::{CaptureOptions, CaptureResult, capture_lines};
use crate::path_utils::{assert_absolute_path, is_same_or_descendant_path};
use crate::restore::SnapshotOptions;
use crate::session::{SessionError, SessionOptions, StateSnapshot, TerminalSession};
use crate::terminal_env::resolve_posix;

/// What a manager passes every session besides the per-terminal options.
#[derive(Debug, Clone)]
pub struct SessionDefaults {
    /// The daemon's `process.env`.
    pub process_env: JsObject,
    pub paseo_cli_bin_dir: Option<String>,
    pub paseo_hook_cli_path: Option<String>,
    pub zsh_integration_dir: Option<std::path::PathBuf>,
    pub tmpdir: std::path::PathBuf,
    pub username: String,
    pub pid: u32,
    /// The daemon's working directory.
    pub process_cwd: String,
    pub helper: std::path::PathBuf,
}

/// `getTerminalActivityUrl`.
pub type ActivityUrl = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The options `createTerminal` takes.
#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    pub id: Option<String>,
    pub cwd: String,
    pub workspace_id: String,
    pub name: Option<String>,
    pub title: Option<String>,
    pub env: Option<JsObject>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub activity_token: Option<String>,
    /// `undefined` asks the manager's URL source; `Some(None)` is `null`.
    pub activity_url: Option<Option<String>>,
}

/// A message-carrying error (`error.message` of the baseline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerError(pub String);

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManagerError {}

impl From<SessionError> for ManagerError {
    fn from(error: SessionError) -> Self {
        Self(error.to_string())
    }
}

/// `TerminalListItem`. The activity is not tracked here, so it is always
/// `null` on the wire for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalListItem {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub workspace_id: String,
    pub title: Option<String>,
}

/// `TerminalsChangedEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalsChangedEvent {
    pub cwd: String,
    pub terminals: Vec<TerminalListItem>,
}

/// `validateTerminalActivityToken` outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCheck {
    Valid,
    Unknown,
    Invalid,
}

type ChangedListener = Arc<dyn Fn(&TerminalsChangedEvent) + Send + Sync>;

struct Entry {
    session: TerminalSession,
    exit_listener: u64,
    title_listener: u64,
}

#[derive(Default)]
struct State {
    /// Insertion ordered, like a `Map`.
    by_id: Vec<(String, Entry)>,
    /// Insertion ordered, like a `Map`.
    by_cwd: Vec<(String, Vec<TerminalSession>)>,
    tokens: HashMap<String, String>,
    default_env: Vec<(String, JsObject)>,
    changed_listeners: Vec<(u64, ChangedListener)>,
    next_listener: u64,
}

struct Inner {
    defaults: SessionDefaults,
    activity_url: Option<ActivityUrl>,
    create_lock: Mutex<()>,
    /// Held from `registerSession` to the creation event. The baseline runs
    /// that span in one turn, so a title or exit event of the new session
    /// cannot slip in before the list announces it.
    sync_section: Mutex<()>,
    state: Mutex<State>,
}

/// A handle to the registry; clones share it.
#[derive(Clone)]
pub struct TerminalManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TerminalManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalManager").finish_non_exhaustive()
    }
}

/// `randomBytes(32).toString("base64url")`.
fn activity_token() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = uuid::Uuid::new_v4().as_bytes().to_vec();
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let triple = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * index))
        });
        let symbols = chunk.len() + 1;
        for index in 0..symbols {
            let value = (triple >> (18 - 6 * index)) & 0x3F;
            out.push(char::from(ALPHABET[usize::try_from(value).unwrap_or(0)]));
        }
    }
    out
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TerminalManager {
    #[must_use]
    pub fn new(defaults: SessionDefaults, activity_url: Option<ActivityUrl>) -> Self {
        Self {
            inner: Arc::new(Inner {
                defaults,
                activity_url,
                create_lock: Mutex::new(()),
                sync_section: Mutex::new(()),
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// `getTerminals(cwd, { workspaceId })`: every session at or below `cwd`,
    /// narrowed to one workspace when given.
    ///
    /// # Errors
    ///
    /// The `assertAbsolutePath` error for a relative `cwd`.
    pub fn get_terminals(
        &self,
        cwd: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<TerminalSession>, ManagerError> {
        assert_absolute_path(cwd).map_err(|error| ManagerError(error.to_owned()))?;
        let state = lock(&self.inner.state);
        let mut sessions = Vec::new();
        for (bucket_cwd, bucket) in &state.by_cwd {
            if is_same_or_descendant_path(cwd, bucket_cwd) {
                sessions.extend(bucket.iter().cloned());
            }
        }
        drop(state);
        // A missing owner is not workspace membership; unscoped callers still
        // list those terminals.
        if let Some(workspace_id) = workspace_id {
            sessions.retain(|session| session.workspace_id == workspace_id);
        }
        Ok(sessions)
    }

    /// `registerCwdEnv`.
    ///
    /// # Errors
    ///
    /// The `assertAbsolutePath` error for a relative `cwd`.
    pub fn register_cwd_env(&self, cwd: &str, env: &JsObject) -> Result<(), ManagerError> {
        assert_absolute_path(cwd).map_err(|error| ManagerError(error.to_owned()))?;
        let root = resolve_posix(&self.inner.defaults.process_cwd, cwd);
        let mut state = lock(&self.inner.state);
        let copy = env.clone();
        match state
            .default_env
            .iter_mut()
            .find(|(existing, _)| *existing == root)
        {
            Some(entry) => entry.1 = copy,
            None => state.default_env.push((root, copy)),
        }
        Ok(())
    }

    /// `resolveDefaultEnvForCwd`: the environment of the longest registered
    /// root that contains `cwd`.
    fn default_env_for_cwd(&self, state: &State, cwd: &str) -> Option<JsObject> {
        let normalized = resolve_posix(&self.inner.defaults.process_cwd, cwd);
        let mut best: Option<&(String, JsObject)> = None;
        for entry in &state.default_env {
            let matches = normalized == entry.0 || normalized.starts_with(&format!("{}/", entry.0));
            if matches && best.is_none_or(|current| entry.0.len() > current.0.len()) {
                best = Some(entry);
            }
        }
        best.map(|entry| entry.1.clone())
    }

    /// The default name (`Terminal N` for the cwd's bucket) and
    /// `{ ...inheritedEnv, ...options.env }`.
    fn name_and_env(&self, options: &CreateOptions) -> (String, Option<JsObject>) {
        let state = lock(&self.inner.state);
        let count = state
            .by_cwd
            .iter()
            .find(|(cwd, _)| *cwd == options.cwd)
            .map_or(0, |(_, bucket)| bucket.len());
        let inherited = self.default_env_for_cwd(&state, &options.cwd);
        let merged = match (inherited, options.env.as_ref()) {
            (None, None) => None,
            (inherited, own) => {
                let mut merged = JsObject::new();
                for source in [inherited.as_ref(), own].into_iter().flatten() {
                    for (key, value) in source.iter() {
                        merged.insert(key, value.clone());
                    }
                }
                Some(merged)
            }
        };
        (format!("Terminal {}", count + 1), merged)
    }

    /// `createTerminal(options)`.
    ///
    /// # Errors
    ///
    /// The `assertAbsolutePath` error, or the error that stopped the spawn.
    pub fn create_terminal(
        &self,
        options: &CreateOptions,
    ) -> Result<TerminalSession, ManagerError> {
        assert_absolute_path(&options.cwd).map_err(|error| ManagerError(error.to_owned()))?;
        let _serial = self
            .inner
            .create_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let (default_name, merged_env) = self.name_and_env(options);

        let terminal_id = options
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let token = options
            .activity_token
            .clone()
            .unwrap_or_else(activity_token);
        let activity_url = match &options.activity_url {
            None => self.inner.activity_url.as_ref().and_then(|source| source()),
            Some(url) => url.clone(),
        };
        let mut activity_env = JsObject::new();
        activity_env.insert("PASEO_TERMINAL_ID", JsValue::String(terminal_id.clone()));
        activity_env.insert("PASEO_ACTIVITY_TOKEN", JsValue::String(token.clone()));
        if let Some(url) = activity_url.filter(|url| !url.is_empty()) {
            activity_env.insert("PASEO_TERMINAL_ACTIVITY_URL", JsValue::String(url));
        }
        lock(&self.inner.state)
            .tokens
            .insert(terminal_id.clone(), token);

        let defaults = &self.inner.defaults;
        let created = TerminalSession::create(SessionOptions {
            id: Some(terminal_id.clone()),
            cwd: options.cwd.clone().into(),
            workspace_id: options.workspace_id.clone(),
            shell: None,
            env: merged_env.unwrap_or_default(),
            activity_env,
            rows: options.rows,
            cols: options.cols,
            name: Some(options.name.clone().unwrap_or(default_name)),
            title: options.title.clone().filter(|title| !title.is_empty()),
            command: options
                .command
                .clone()
                .filter(|command| !command.is_empty()),
            args: options.args.clone().unwrap_or_default(),
            process_env: defaults.process_env.clone(),
            paseo_cli_bin_dir: defaults.paseo_cli_bin_dir.clone(),
            paseo_hook_cli_path: defaults.paseo_hook_cli_path.clone(),
            zsh_integration_dir: defaults.zsh_integration_dir.clone(),
            tmpdir: defaults.tmpdir.clone(),
            username: defaults.username.clone(),
            pid: defaults.pid,
            process_cwd: defaults.process_cwd.clone(),
            helper: defaults.helper.clone(),
        });
        let session = match created {
            Ok(session) => session,
            Err(error) => {
                lock(&self.inner.state).tokens.remove(&terminal_id);
                return Err(error.into());
            }
        };

        let sync = self
            .inner
            .sync_section
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.register_session(&session);
        {
            let mut state = lock(&self.inner.state);
            match state.by_cwd.iter_mut().find(|(cwd, _)| *cwd == options.cwd) {
                Some((_, bucket)) => bucket.push(session.clone()),
                None => state
                    .by_cwd
                    .push((options.cwd.clone(), vec![session.clone()])),
            }
        }
        self.emit_terminals_changed(&options.cwd);
        drop(sync);
        Ok(session)
    }

    /// `registerSession`: exit removes the session; a title change announces
    /// the list again.
    fn register_session(&self, session: &TerminalSession) {
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let id = session.id.clone();
        let exit_weak = weak.clone();
        let exit_id = id.clone();
        let exit_listener = session.on_exit(move |_| {
            if let Some(inner) = exit_weak.upgrade() {
                let manager = TerminalManager { inner };
                let _sync = manager
                    .inner
                    .sync_section
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                manager.remove_session(&exit_id, false);
            }
        });
        let cwd = session.cwd.to_string_lossy().into_owned();
        let title_listener = session.on_title_change(move |_| {
            if let Some(inner) = weak.upgrade() {
                let manager = TerminalManager { inner };
                let _sync = manager
                    .inner
                    .sync_section
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                manager.emit_terminals_changed(&cwd);
            }
        });
        let entry = Entry {
            session: session.clone(),
            exit_listener,
            title_listener,
        };
        let mut state = lock(&self.inner.state);
        match state.by_id.iter_mut().find(|(existing, _)| *existing == id) {
            Some(slot) => slot.1 = entry,
            None => state.by_id.push((id, entry)),
        }
    }

    /// `removeSessionById(id, { kill })`.
    fn remove_session(&self, id: &str, kill: bool) {
        let entry = {
            let mut state = lock(&self.inner.state);
            let Some(position) = state.by_id.iter().position(|(existing, _)| existing == id) else {
                return;
            };
            let entry = state.by_id.remove(position).1;
            state.tokens.remove(id);
            let cwd = entry.session.cwd.to_string_lossy().into_owned();
            if let Some(position) = state.by_cwd.iter().position(|(bucket, _)| *bucket == cwd) {
                let bucket = &mut state.by_cwd[position].1;
                if let Some(index) = bucket.iter().position(|session| session.id == id) {
                    bucket.remove(index);
                }
                if bucket.is_empty() {
                    state.by_cwd.remove(position);
                }
            }
            entry
        };
        entry.session.remove_listener(entry.exit_listener);
        entry.session.remove_listener(entry.title_listener);
        if kill {
            entry.session.kill();
        }
        self.emit_terminals_changed(&entry.session.cwd.to_string_lossy());
    }

    fn list_item(session: &TerminalSession) -> TerminalListItem {
        TerminalListItem {
            id: session.id.clone(),
            name: session.name.clone(),
            cwd: session.cwd.to_string_lossy().into_owned(),
            workspace_id: session.workspace_id.clone(),
            title: session.title(),
        }
    }

    /// `emitTerminalsChanged({ cwd })`.
    fn emit_terminals_changed(&self, cwd: &str) {
        let (listeners, terminals) = {
            let state = lock(&self.inner.state);
            if state.changed_listeners.is_empty() {
                return;
            }
            let terminals = state
                .by_cwd
                .iter()
                .find(|(bucket, _)| bucket == cwd)
                .map(|(_, bucket)| bucket.clone())
                .unwrap_or_default();
            let listeners: Vec<ChangedListener> = state
                .changed_listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect();
            (listeners, terminals)
        };
        let event = TerminalsChangedEvent {
            cwd: cwd.to_owned(),
            terminals: terminals.iter().map(Self::list_item).collect(),
        };
        for listener in listeners {
            listener(&event);
        }
    }

    /// `validateTerminalActivityToken`.
    #[must_use]
    pub fn validate_activity_token(&self, terminal_id: &str, token: &str) -> TokenCheck {
        match lock(&self.inner.state).tokens.get(terminal_id) {
            None => TokenCheck::Unknown,
            Some(expected) if expected == token => TokenCheck::Valid,
            Some(_) => TokenCheck::Invalid,
        }
    }

    /// `getTerminal(id)`.
    #[must_use]
    pub fn get_terminal(&self, id: &str) -> Option<TerminalSession> {
        lock(&self.inner.state)
            .by_id
            .iter()
            .find(|(existing, _)| existing == id)
            .map(|(_, entry)| entry.session.clone())
    }

    /// `getTerminalState(id, options)`.
    #[must_use]
    pub fn get_terminal_state(&self, id: &str, options: SnapshotOptions) -> Option<StateSnapshot> {
        self.get_terminal(id)?.state_snapshot(options)
    }

    /// `setTerminalTitle(id, title)`: whether the terminal exists.
    #[must_use]
    pub fn set_terminal_title(&self, id: &str, title: &str) -> bool {
        let Some(session) = self.get_terminal(id) else {
            return false;
        };
        session.set_title(title);
        true
    }

    /// `killTerminal(id)`.
    pub fn kill_terminal(&self, id: &str) {
        self.remove_session(id, true);
    }

    /// `killTerminalAndWait(id, options)`.
    pub fn kill_terminal_and_wait(
        &self,
        id: &str,
        graceful_timeout: Option<Duration>,
        force_timeout: Option<Duration>,
    ) {
        let Some(session) = self.get_terminal(id) else {
            return;
        };
        session.kill_and_wait(
            graceful_timeout.unwrap_or(Duration::from_millis(2000)),
            force_timeout.unwrap_or(Duration::from_millis(1000)),
        );
        self.remove_session(id, false);
    }

    /// `captureTerminal(id, options)`.
    #[must_use]
    pub fn capture_terminal(&self, id: &str, options: &CaptureOptions) -> CaptureResult {
        let empty = CaptureResult {
            lines: Vec::new(),
            total_lines: 0,
        };
        let Some(session) = self.get_terminal(id) else {
            return empty;
        };
        match session.state(SnapshotOptions::default()) {
            Some(state) => capture_lines(&state, options),
            None => empty,
        }
    }

    /// `listDirectories()`.
    #[must_use]
    pub fn list_directories(&self) -> Vec<String> {
        lock(&self.inner.state)
            .by_cwd
            .iter()
            .map(|(cwd, _)| cwd.clone())
            .collect()
    }

    /// `killAll()`.
    pub fn kill_all(&self) {
        let ids: Vec<String> = lock(&self.inner.state)
            .by_id
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove_session(&id, true);
        }
    }

    /// `subscribeTerminalsChanged(listener)`; returns the id for
    /// [`Self::unsubscribe_terminals_changed`].
    pub fn subscribe_terminals_changed(
        &self,
        listener: impl Fn(&TerminalsChangedEvent) + Send + Sync + 'static,
    ) -> u64 {
        let mut state = lock(&self.inner.state);
        state.next_listener += 1;
        let id = state.next_listener;
        state.changed_listeners.push((id, Arc::new(listener)));
        id
    }

    pub fn unsubscribe_terminals_changed(&self, id: u64) {
        lock(&self.inner.state)
            .changed_listeners
            .retain(|(listener, _)| *listener != id);
    }
}
