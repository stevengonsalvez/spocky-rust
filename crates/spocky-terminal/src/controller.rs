//! The terminal message handlers of a client session, following the dispatch
//! half of pinned `packages/server/src/terminal/terminal-session-controller.ts`
//! and `killTerminalsForWorkspace` of `workspace-archive-service.ts`. The
//! output streams themselves are [`TerminalStreams`].
//!
//! The baseline is promise code. This is the same machine with its awaits made
//! explicit, in the style of [`crate::stream`]: everything outside the machine
//! (the terminal manager, the owned-subscription delivery, the workspace
//! registry, microtasks) is the [`ControllerHost`]. A step that waits on the
//! host is a task: the controller hands the host a task id and the host calls
//! [`TerminalSessionController::resume`] when the result is ready.
//!
//! Awaits on already settled manager results (`getTerminals`, `captureTerminal`
//! and the like) run through at the call, except the one in the directory
//! refresh, whose tick is what makes a burst of change events coalesce. The
//! interleaving of such a handler with other microtasks therefore differs from
//! the baseline by those ticks (see `evidence/phase4/terminal-deviations.md`).

use std::collections::{BTreeMap, HashMap};

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_wire::{TerminalOpcode, decode_terminal_resize};

use crate::capture::{CaptureOptions, CaptureResult};
use crate::process_title::js_trim;
use crate::restore::{RestoreMode, RestoreOptions, SnapshotMode};
use crate::session::{ClientMessage, ServerMessage};
use crate::size_ownership::{SizeIntent, SizeRequest};
use crate::stream::{StreamHost, TerminalStreams};

/// Handle of an owned subscription, assigned by the host.
pub type OwnerId = u64;

/// Handle of a client connection, assigned by the host.
pub type SourceId = u64;

/// Handle of a suspended step, assigned by the controller.
pub type TaskId = u64;

/// What `ownership.begin` returned.
#[derive(Debug, Clone)]
pub struct Owner {
    pub id: OwnerId,
    pub response_id: String,
    pub source: SourceId,
}

/// `ownership.begin`: the new owner, and the owners of the same legacy slot
/// that a legacy source's `begin` released first. Their cleanup is
/// [`TerminalSessionController::owner_stopped`], which the controller runs
/// itself before it goes on.
#[derive(Debug, Clone)]
pub struct Begun {
    pub owner: Owner,
    pub released_prior: Vec<OwnerId>,
}

/// What the handlers read of a terminal session: `id`, `name`, `cwd`,
/// `workspaceId` and `getTitle()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub workspace_id: String,
    pub title: Option<String>,
}

/// An entry of `listTerminalWorkspaceRefs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub workspace_id: String,
    pub cwd: String,
}

/// The `createTerminal` options of a `create_terminal_request`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    pub cwd: String,
    pub workspace_id: String,
    pub name: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
}

/// The `killTerminalAndWait` timeouts, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KillTimeouts {
    pub graceful: f64,
    pub force: f64,
}

/// What the host answers a task with.
#[derive(Debug)]
pub enum Resume {
    /// A microtask the controller asked for with [`ControllerHost::defer_task`].
    Tick,
    /// `listTerminalWorkspaceRefs()` settled.
    Refs(Result<Vec<WorkspaceRef>, String>),
    /// `listTerminalWorkspaceRoots()` settled.
    Roots(Result<Vec<String>, String>),
    /// `createTerminal` settled.
    Created(Result<TerminalInfo, String>),
    /// `killTerminalAndWait` settled.
    Killed(Result<(), String>),
}

/// Everything the controller needs from outside, on top of [`StreamHost`].
///
/// The host keeps its own record of which owner a stream slot belongs to
/// ([`Self::stream_bound`]). Owner release works in two halves: the host half
/// (the owner stops delivering, its signal aborts) is the host's, and the
/// owner's cleanup is [`TerminalSessionController::owner_stopped`]. When the
/// controller releases an owner it calls [`Self::release_owner`] and then runs
/// the cleanup itself; when the host releases one (a disconnect, an
/// `unsubscribe` reaching the owner some other way) the host calls
/// `owner_stopped`.
pub trait ControllerHost: StreamHost {
    /// `terminalManager` is defined.
    fn has_manager(&mut self) -> bool;
    /// `this.emit(message)`: a reply to the request being handled.
    fn emit(&mut self, message: JsValue);
    /// `hasBinaryChannel()`.
    fn has_binary_channel(&mut self) -> bool;
    /// `isPathWithinRoot(root, path)`.
    fn is_path_within_root(&mut self, root: &str, path: &str) -> bool;
    /// `ownership.begin(family, undefined, stop, legacySlot)`.
    ///
    /// # Errors
    ///
    /// The message of the error `begin` throws.
    fn begin_owner(&mut self, family: &str, legacy_slot: &str) -> Result<Begun, String>;
    /// `owner.emit(message)`.
    fn owner_emit(&mut self, owner: OwnerId, message: JsValue);
    /// `owner.signal.aborted`.
    fn owner_aborted(&mut self, owner: OwnerId) -> bool;
    /// `owner.release()`, host half.
    fn release_owner(&mut self, owner: OwnerId);
    /// `ownership.releaseLegacySlot(slot)`, host half: the owner it released,
    /// whose cleanup the controller then runs.
    ///
    /// # Errors
    ///
    /// The message of the error it throws for a modern source.
    fn release_legacy_slot(&mut self, slot: &str) -> Result<Option<OwnerId>, String>;
    /// `ownership.isModern(source)`.
    fn is_modern(&mut self, source: SourceId) -> bool;
    /// The stream in `slot` belongs to `owner`.
    fn stream_bound(&mut self, slot: u8, owner: OwnerId);
    /// `terminal.subscribe(listener, { initialSnapshot })` and
    /// `terminal.onExit(...)` for the stream in `slot`. The host feeds the
    /// terminal's messages to [`TerminalSessionController::terminal_message`]
    /// and its exit to [`TerminalSessionController::terminal_exited`].
    fn terminal_subscribe(&mut self, slot: u8, terminal_id: &str, mode: SnapshotMode);
    /// The promise `dispatch` (or a kill) returned settled.
    fn settled(&mut self, token: u64, result: Result<(), String>);
    /// Resume `task` with [`Resume::Tick`] on the next microtask.
    fn defer_task(&mut self, task: TaskId);
    /// Start `listTerminalWorkspaceRefs()`; answer with [`Resume::Refs`].
    fn request_workspace_refs(&mut self, task: TaskId);
    /// Start `listTerminalWorkspaceRoots()`; answer with [`Resume::Roots`].
    fn request_workspace_roots(&mut self, task: TaskId);
    /// `terminalManager.subscribeTerminalsChanged(...)`. The host feeds events
    /// to [`TerminalSessionController::on_terminals_changed`].
    fn terminals_changed_subscribe(&mut self);
    /// The unsubscribe function it returned.
    fn terminals_changed_unsubscribe(&mut self);
    /// `terminalManager.getTerminals(cwd, { workspaceId })`.
    ///
    /// # Errors
    ///
    /// The message of the error it throws.
    fn get_terminals(
        &mut self,
        cwd: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<TerminalInfo>, String>;
    /// `terminalManager.listDirectories()`.
    fn list_directories(&mut self) -> Vec<String>;
    /// `terminalManager.getTerminal(id)`, read now.
    fn terminal_info(&mut self, id: &str) -> Option<TerminalInfo>;
    /// `terminal.send(message)`.
    fn terminal_send(&mut self, id: &str, message: &ClientMessage);
    /// `applyTerminalSize(terminal, source, request)`.
    fn apply_terminal_size(&mut self, id: &str, source: SourceId, request: SizeRequest);
    /// Start `terminalManager.createTerminal(request)`; answer with
    /// [`Resume::Created`].
    fn create_terminal(&mut self, task: TaskId, request: &CreateRequest);
    /// `terminalManager.setTerminalTitle(id, title)`.
    fn set_terminal_title(&mut self, id: &str, title: &str) -> bool;
    /// `terminalManager.killTerminal(id)`.
    fn kill_terminal(&mut self, id: &str);
    /// Start `terminalManager.killTerminalAndWait(id, timeouts)`; answer with
    /// [`Resume::Killed`].
    fn kill_terminal_and_wait(&mut self, task: TaskId, id: &str, timeouts: Option<KillTimeouts>);
    /// `terminalManager.captureTerminal(id, options)`.
    ///
    /// # Errors
    ///
    /// The message of the error it throws.
    fn capture_terminal(
        &mut self,
        id: &str,
        options: &CaptureOptions,
    ) -> Result<CaptureResult, String>;
}

/// A `terminals_changed` event of the manager; only the cwd is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalsChanged {
    pub cwd: String,
}

/// What `killTerminalForClose` returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillForClose {
    pub terminal_id: String,
    pub success: bool,
}

/// `controller.getMetrics()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub directory_subscription_count: usize,
    pub stream_subscription_count: usize,
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

fn message(kind: &str, payload: Vec<(&str, JsValue)>) -> JsValue {
    object(vec![("type", text(kind)), ("payload", object(payload))])
}

#[allow(clippy::cast_precision_loss)]
fn number(value: usize) -> JsValue {
    JsValue::Number(value as f64)
}

fn string_list(items: &[String]) -> JsValue {
    JsValue::Array(items.iter().map(|item| text(item)).collect())
}

/// `value ? value : undefined` for a string field.
fn truthy_text(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Insert a field with the baseline's `...(cond ? { key } : {})` spread.
fn push_if<'a>(entries: &mut Vec<(&'a str, JsValue)>, key: &'a str, value: Option<&str>) {
    if let Some(value) = truthy_text(value) {
        entries.push((key, text(value)));
    }
}

fn request_id(request: &JsValue) -> JsValue {
    request
        .get("requestId")
        .cloned()
        .unwrap_or(JsValue::Undefined)
}

/// `toTerminalInfo`.
fn terminal_info_entries(terminal: &TerminalInfo) -> Vec<(&'static str, JsValue)> {
    let mut entries = vec![
        ("id", text(&terminal.id)),
        ("name", text(&terminal.name)),
        ("workspaceId", text(&terminal.workspace_id)),
    ];
    push_if(&mut entries, "title", terminal.title.as_deref());
    entries.push(("activity", JsValue::Null));
    entries
}

/// `terminalSubscriptionKey`.
fn subscription_key(cwd: &str, workspace_id: Option<&str>) -> String {
    workspace_id.map_or_else(|| cwd.to_owned(), |id| format!("{id}::{cwd}"))
}

/// `string.length`.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// One stream's owner.
#[derive(Debug, Clone, Copy)]
struct StreamOwner {
    owner: OwnerId,
    source: SourceId,
}

/// A directory subscription.
#[derive(Debug)]
struct Directory {
    owner: OwnerId,
    source: SourceId,
    cwd: String,
    workspace_id: Option<String>,
    /// `subscription.refresh`.
    refresh: Option<Refresh>,
    pending: bool,
    /// Gone from `subscribedDirectories`; kept while its refresh is running.
    removed: bool,
}

/// The refresh loop of a directory subscription, with whoever awaits it.
#[derive(Debug)]
struct Refresh {
    request_id: Option<JsValue>,
    stage: RefreshStage,
    terminals: Vec<TerminalInfo>,
    /// `getTerminals` threw.
    error: Option<String>,
    waiters: Vec<Waiter>,
    /// How the loop ended, once it did.
    done: Option<Result<(), String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshStage {
    /// `await getTerminals(...)` resumes on the next microtask.
    Terminals,
    /// `await listTerminalWorkspaceRoots()`.
    Roots,
    /// The `await` of the async function that read the roots.
    Filtered,
    /// The loop returned and its promise settles on the next microtask.
    Finished,
}

/// Whoever awaits a refresh promise.
#[derive(Debug, Clone)]
enum Waiter {
    /// `handleSubscribeTerminalsRequest`.
    Subscribe { token: u64, owner: OwnerId },
    /// `handleTerminalsChanged`, awaiting subscription `index` of `subs`.
    Fanout { subs: Vec<OwnerId>, index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListStage {
    Terminals,
    Roots,
    Filtered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateStage {
    /// `resolveLegacyTerminalWorkspaceId` reads the refs.
    LegacyRefs,
    /// The active-workspace check reads the refs.
    CheckRefs,
    /// `createTerminal` is running.
    Creating,
}

/// A suspended step.
#[derive(Debug)]
enum Task {
    /// The loop of `emitTerminalsSnapshotForSubscription`.
    Refresh(OwnerId),
    /// `handleListTerminalsRequest` with a cwd, in
    /// `getTerminalsForWorkspaceRoot`.
    List {
        token: u64,
        request: JsValue,
        stage: ListStage,
        terminals: Vec<TerminalInfo>,
        error: Option<String>,
    },
    /// `handleCreateTerminalRequest`.
    Create {
        token: u64,
        request: JsValue,
        stage: CreateStage,
        workspace_id: Option<String>,
    },
    /// `handleKillTerminalRequest`.
    Kill {
        token: u64,
        terminal_id: String,
        request_id: JsValue,
    },
    /// One `killTerminalAndWait` of `killTerminalsForWorkspace`.
    ArchiveKill { group: u64 },
}

/// A `killTerminalsForWorkspace` call waiting for its kills.
#[derive(Debug)]
struct ArchiveGroup {
    token: u64,
    remaining: usize,
}

/// A binary frame from a client.
#[derive(Debug, Clone)]
pub struct ClientFrame {
    pub opcode: TerminalOpcode,
    pub slot: u8,
    pub payload: Vec<u8>,
}

/// The terminal message handlers of one client session.
#[derive(Debug, Default)]
pub struct TerminalSessionController {
    streams: TerminalStreams,
    stream_owners: BTreeMap<u8, StreamOwner>,
    owner_slots: HashMap<OwnerId, u8>,
    /// `subscribedDirectories`, in insertion order.
    directories: Vec<Directory>,
    /// `unsubscribeTerminalsChanged` is set.
    changed_subscribed: bool,
    tasks: HashMap<TaskId, Task>,
    groups: HashMap<u64, ArchiveGroup>,
    next_task: TaskId,
    next_group: u64,
}

/// How [`TerminalSessionController::dispatch`] saw a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatched {
    /// Not a terminal message: `dispatch` returned `undefined`.
    NotTerminal,
    /// A terminal message. The host is told when its promise settles, through
    /// [`ControllerHost::settled`] with the token given.
    Handled,
}

impl TerminalSessionController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `getMetrics()`.
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        Metrics {
            directory_subscription_count: self
                .directories
                .iter()
                .filter(|directory| !directory.removed)
                .count(),
            stream_subscription_count: self.streams.len(),
        }
    }

    fn new_task(&mut self, task: Task) -> TaskId {
        self.next_task += 1;
        self.tasks.insert(self.next_task, task);
        self.next_task
    }

    fn directory_mut(&mut self, owner: OwnerId) -> Option<&mut Directory> {
        self.directories
            .iter_mut()
            .find(|directory| directory.owner == owner)
    }

    /// `start()`: subscribe to the manager's changes once.
    fn start(&mut self, host: &mut dyn ControllerHost) {
        if !host.has_manager() || self.changed_subscribed {
            return;
        }
        self.changed_subscribed = true;
        host.terminals_changed_subscribe();
    }

    // ---- workspace roots -------------------------------------------------

    /// `isSamePath`.
    fn is_same_path(host: &mut dyn ControllerHost, first: &str, second: &str) -> bool {
        host.is_path_within_root(first, second) && host.is_path_within_root(second, first)
    }

    /// `resolveTerminalOwnerRoot`: the longest workspace root that contains
    /// the terminal's cwd.
    fn resolve_owner_root(
        host: &mut dyn ControllerHost,
        terminal_cwd: &str,
        roots: &[String],
    ) -> Option<String> {
        let mut owner: Option<&String> = None;
        for root in roots {
            if !host.is_path_within_root(root, terminal_cwd) {
                continue;
            }
            if owner.is_none_or(|current| js_len(root) > js_len(current)) {
                owner = Some(root);
            }
        }
        owner.cloned()
    }

    /// `terminalBelongsToRoot`.
    fn terminal_belongs_to_root(
        host: &mut dyn ControllerHost,
        root_cwd: &str,
        terminal_cwd: &str,
        roots: &[String],
    ) -> bool {
        match Self::resolve_owner_root(host, terminal_cwd, roots) {
            None => host.is_path_within_root(root_cwd, terminal_cwd),
            Some(owner_root) => Self::is_same_path(host, root_cwd, &owner_root),
        }
    }

    /// The filter of `getTerminalsForWorkspaceRoot` once the roots are known.
    fn filter_by_roots(
        host: &mut dyn ControllerHost,
        cwd: &str,
        terminals: Vec<TerminalInfo>,
        roots: &[String],
    ) -> Vec<TerminalInfo> {
        if roots.is_empty() {
            return terminals;
        }
        terminals
            .into_iter()
            .filter(|terminal| Self::terminal_belongs_to_root(host, cwd, &terminal.cwd, roots))
            .collect()
    }

    /// `hasDirectorySubscription(input, source)` over the settled
    /// `listTerminalWorkspaceRoots()`. The baseline reads the roots only when
    /// some subscription is a candidate; [`Self::has_directory_candidates`]
    /// tells.
    pub fn has_directory_subscription(
        &self,
        host: &mut dyn ControllerHost,
        workspace_id: &str,
        cwd: &str,
        source: Option<SourceId>,
        workspace_roots: &[String],
    ) -> bool {
        self.directories
            .iter()
            .filter(|subscription| !subscription.removed)
            .filter(|subscription| source.is_none_or(|source| subscription.source == source))
            .any(|subscription| {
                if subscription
                    .workspace_id
                    .as_deref()
                    .is_some_and(|id| id != workspace_id)
                {
                    return false;
                }
                Self::terminal_belongs_to_root(host, &subscription.cwd, cwd, workspace_roots)
            })
    }

    /// Whether `hasDirectorySubscription` has a subscription to look at, so
    /// that the caller reads the workspace roots.
    #[must_use]
    pub fn has_directory_candidates(&self, source: Option<SourceId>) -> bool {
        self.directories
            .iter()
            .filter(|subscription| !subscription.removed)
            .any(|subscription| source.is_none_or(|source| subscription.source == source))
    }

    // ---- dispatch --------------------------------------------------------

    /// `dispatch(msg, ownership)`. `source` is `ownership.currentSource`.
    pub fn dispatch(
        &mut self,
        host: &mut dyn ControllerHost,
        token: u64,
        request: &JsValue,
        source: Option<SourceId>,
    ) -> Dispatched {
        let Some(kind) = request.get("type").and_then(JsValue::as_str) else {
            return Dispatched::NotTerminal;
        };
        let cwd = request.get("cwd").and_then(JsValue::as_str).unwrap_or("");
        let workspace_id = request.get("workspaceId").and_then(JsValue::as_str);
        let terminal_id = request
            .get("terminalId")
            .and_then(JsValue::as_str)
            .unwrap_or("");
        match kind {
            "subscribe_terminals_request" => self.handle_subscribe_terminals(host, token, request),
            "unsubscribe_terminals_request" => {
                let slot = format!("terminal-directory:{}", subscription_key(cwd, workspace_id));
                self.release_slot(host, token, &slot);
            }
            "list_terminals_request" => self.handle_list(host, token, request),
            "create_terminal_request" => self.handle_create(host, token, request),
            "subscribe_terminal_request" => self.handle_subscribe_terminal(host, token, request),
            "unsubscribe_terminal_request" => {
                let slot = format!("terminal-output:{terminal_id}");
                self.release_slot(host, token, &slot);
            }
            "terminal_input" => match source {
                None => host.settled(token, Err("Terminal input requires a source".to_owned())),
                Some(source) => {
                    Self::handle_input(host, request, source);
                    host.settled(token, Ok(()));
                }
            },
            "kill_terminal_request" => self.handle_kill(host, token, request),
            "capture_terminal_request" => {
                Self::handle_capture(host, request);
                host.settled(token, Ok(()));
            }
            "terminal.rename.request" => {
                Self::handle_rename(host, request);
                host.settled(token, Ok(()));
            }
            _ => return Dispatched::NotTerminal,
        }
        Dispatched::Handled
    }

    /// `ownership.releaseLegacySlot(slot)`.
    fn release_slot(&mut self, host: &mut dyn ControllerHost, token: u64, slot: &str) {
        match host.release_legacy_slot(slot) {
            Err(error) => host.settled(token, Err(error)),
            Ok(owner) => {
                if let Some(owner) = owner {
                    self.owner_stopped(host, owner);
                }
                host.settled(token, Ok(()));
            }
        }
    }

    // ---- directory subscriptions ----------------------------------------

    fn handle_subscribe_terminals(
        &mut self,
        host: &mut dyn ControllerHost,
        token: u64,
        request: &JsValue,
    ) {
        if !host.has_manager() {
            host.settled(token, Err("Terminal manager not available".to_owned()));
            return;
        }
        let cwd = request
            .get("cwd")
            .and_then(JsValue::as_str)
            .unwrap_or("")
            .to_owned();
        let workspace_id = request
            .get("workspaceId")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        let slot = format!(
            "terminal-directory:{}",
            subscription_key(&cwd, workspace_id.as_deref())
        );
        let begun = match host.begin_owner("terminal-directories", &slot) {
            Ok(begun) => begun,
            Err(error) => {
                host.settled(token, Err(error));
                return;
            }
        };
        for prior in begun.released_prior {
            self.owner_stopped(host, prior);
        }
        let owner = begun.owner;
        self.directories.push(Directory {
            owner: owner.id,
            source: owner.source,
            cwd,
            workspace_id,
            refresh: None,
            pending: false,
            removed: false,
        });
        self.start(host);
        let request_id = request
            .get("requestId")
            .filter(|id| id.as_str().is_some_and(|text| !text.is_empty()))
            .cloned();
        self.emit_snapshot_for(
            host,
            owner.id,
            request_id,
            Some(Waiter::Subscribe {
                token,
                owner: owner.id,
            }),
        );
    }

    /// `emitTerminalsSnapshotForSubscription(subscription, requestId)`; the
    /// waiter awaits the promise it returns.
    fn emit_snapshot_for(
        &mut self,
        host: &mut dyn ControllerHost,
        owner: OwnerId,
        request_id: Option<JsValue>,
        waiter: Option<Waiter>,
    ) {
        let Some(directory) = self.directory_mut(owner) else {
            // The subscription is gone and its refresh finished; the baseline
            // would run one more refresh that returns at the abort check.
            self.settle_waiter(host, waiter, Ok(()));
            return;
        };
        if let Some(refresh) = directory.refresh.as_mut() {
            directory.pending = true;
            refresh.waiters.extend(waiter);
            return;
        }
        directory.refresh = Some(Refresh {
            request_id,
            stage: RefreshStage::Terminals,
            terminals: Vec::new(),
            error: None,
            waiters: waiter.into_iter().collect(),
            done: None,
        });
        self.refresh_iteration(host, owner);
    }

    /// One pass of the `do { ... } while (subscription.pending)` loop, up to
    /// its first await.
    fn refresh_iteration(&mut self, host: &mut dyn ControllerHost, owner: OwnerId) {
        let Some(directory) = self.directory_mut(owner) else {
            return;
        };
        directory.pending = false;
        let cwd = directory.cwd.clone();
        let workspace_id = directory.workspace_id.clone();
        let result = host.get_terminals(&cwd, workspace_id.as_deref());
        if let Some(refresh) = self
            .directory_mut(owner)
            .and_then(|directory| directory.refresh.as_mut())
        {
            refresh.stage = RefreshStage::Terminals;
            match result {
                Ok(terminals) => refresh.terminals = terminals,
                Err(error) => refresh.error = Some(error),
            }
        }
        let task = self.new_task(Task::Refresh(owner));
        host.defer_task(task);
    }

    fn resume_refresh(
        &mut self,
        host: &mut dyn ControllerHost,
        task: TaskId,
        owner: OwnerId,
        with: Resume,
    ) {
        let Some(stage) = self
            .directory_mut(owner)
            .and_then(|directory| directory.refresh.as_ref())
            .map(|refresh| refresh.stage)
        else {
            return;
        };
        match (stage, with) {
            (RefreshStage::Terminals, Resume::Tick) => {
                let failed = self
                    .directory_mut(owner)
                    .and_then(|directory| directory.refresh.as_mut())
                    .and_then(|refresh| refresh.error.take());
                if let Some(error) = failed {
                    self.finish_refresh(host, owner, Err(error));
                    return;
                }
                self.set_refresh_stage(owner, RefreshStage::Roots);
                self.tasks.insert(task, Task::Refresh(owner));
                host.request_workspace_roots(task);
            }
            (RefreshStage::Roots, Resume::Roots(roots)) => match roots {
                Err(error) => self.finish_refresh(host, owner, Err(error)),
                Ok(roots) => {
                    let Some((cwd, terminals)) = self.directory_mut(owner).and_then(|directory| {
                        let cwd = directory.cwd.clone();
                        directory
                            .refresh
                            .as_mut()
                            .map(|refresh| (cwd, std::mem::take(&mut refresh.terminals)))
                    }) else {
                        return;
                    };
                    let filtered = Self::filter_by_roots(host, &cwd, terminals, &roots);
                    if let Some(refresh) = self
                        .directory_mut(owner)
                        .and_then(|directory| directory.refresh.as_mut())
                    {
                        refresh.terminals = filtered;
                        refresh.stage = RefreshStage::Filtered;
                    }
                    let task = self.new_task(Task::Refresh(owner));
                    host.defer_task(task);
                }
            },
            (RefreshStage::Filtered, Resume::Tick) => self.emit_refresh(host, owner),
            (RefreshStage::Finished, Resume::Tick) => self.settle_refresh(host, owner),
            _ => {}
        }
    }

    fn set_refresh_stage(&mut self, owner: OwnerId, stage: RefreshStage) {
        if let Some(refresh) = self
            .directory_mut(owner)
            .and_then(|directory| directory.refresh.as_mut())
        {
            refresh.stage = stage;
        }
    }

    /// After the terminals are read: the abort check and the emit.
    fn emit_refresh(&mut self, host: &mut dyn ControllerHost, owner: OwnerId) {
        if host.owner_aborted(owner) {
            self.finish_refresh(host, owner, Ok(()));
            return;
        }
        let Some(directory) = self.directory_mut(owner) else {
            return;
        };
        let cwd = directory.cwd.clone();
        let workspace_id = directory.workspace_id.clone();
        let Some(refresh) = directory.refresh.as_mut() else {
            return;
        };
        let terminals = std::mem::take(&mut refresh.terminals);
        let request_id = refresh.request_id.take();
        let infos: Vec<JsValue> = terminals
            .iter()
            .map(|terminal| object(terminal_info_entries(&Self::fresh(host, terminal))))
            .collect();
        let mut payload = vec![("cwd", text(&cwd))];
        payload.push((
            "workspaceId",
            workspace_id.as_deref().map_or(JsValue::Undefined, text),
        ));
        payload.push(("terminals", JsValue::Array(infos)));
        if let Some(request_id) = request_id {
            payload.push(("requestId", request_id));
        }
        host.owner_emit(owner, message("terminals_changed", payload));
        let pending = self
            .directory_mut(owner)
            .is_some_and(|directory| directory.pending);
        if pending {
            self.refresh_iteration(host, owner);
        } else {
            self.finish_refresh(host, owner, Ok(()));
        }
    }

    /// `toTerminalInfo` reads the live terminal; a terminal that is gone keeps
    /// what was read when the list was taken.
    fn fresh(host: &mut dyn ControllerHost, terminal: &TerminalInfo) -> TerminalInfo {
        host.terminal_info(&terminal.id)
            .unwrap_or_else(|| terminal.clone())
    }

    fn finish_refresh(
        &mut self,
        host: &mut dyn ControllerHost,
        owner: OwnerId,
        result: Result<(), String>,
    ) {
        if let Some(refresh) = self
            .directory_mut(owner)
            .and_then(|directory| directory.refresh.as_mut())
        {
            refresh.stage = RefreshStage::Finished;
            refresh.done = Some(result);
        }
        let task = self.new_task(Task::Refresh(owner));
        host.defer_task(task);
    }

    /// The reactions of the settled refresh promise, in registration order:
    /// `subscription.refresh = null`, then each awaiter.
    fn settle_refresh(&mut self, host: &mut dyn ControllerHost, owner: OwnerId) {
        let Some(directory) = self.directory_mut(owner) else {
            return;
        };
        let Some(refresh) = directory.refresh.take() else {
            return;
        };
        if directory.removed {
            self.directories
                .retain(|directory| !(directory.owner == owner && directory.removed));
        }
        let result = refresh.done.unwrap_or(Ok(()));
        for waiter in refresh.waiters {
            self.settle_waiter(host, Some(waiter), result.clone());
        }
    }

    fn settle_waiter(
        &mut self,
        host: &mut dyn ControllerHost,
        waiter: Option<Waiter>,
        result: Result<(), String>,
    ) {
        match (waiter, result) {
            (Some(Waiter::Subscribe { token, .. }), Ok(())) => host.settled(token, Ok(())),
            (Some(Waiter::Subscribe { token, owner }), Err(error)) => {
                host.release_owner(owner);
                self.owner_stopped(host, owner);
                host.settled(token, Err(error));
            }
            (Some(Waiter::Fanout { subs, index }), Ok(())) => {
                self.fanout_next(host, subs, index + 1);
            }
            // handleTerminalsChanged rejects and nothing awaits it; with no
            // awaiter there is nothing to settle.
            (Some(Waiter::Fanout { .. }) | None, Err(_)) | (None, Ok(())) => {}
        }
    }

    /// The manager announced a change in a directory: `handleTerminalsChanged`.
    pub fn on_terminals_changed(
        &mut self,
        host: &mut dyn ControllerHost,
        event: &TerminalsChanged,
    ) {
        let candidates: Vec<(OwnerId, String)> = self
            .directories
            .iter()
            .filter(|directory| !directory.removed)
            .map(|directory| (directory.owner, directory.cwd.clone()))
            .collect();
        let subs: Vec<OwnerId> = candidates
            .into_iter()
            .filter(|(_, cwd)| host.is_path_within_root(cwd, &event.cwd))
            .map(|(owner, _)| owner)
            .collect();
        self.fanout_next(host, subs, 0);
    }

    fn fanout_next(&mut self, host: &mut dyn ControllerHost, subs: Vec<OwnerId>, index: usize) {
        let Some(owner) = subs.get(index).copied() else {
            return;
        };
        self.emit_snapshot_for(host, owner, None, Some(Waiter::Fanout { subs, index }));
    }

    // ---- owners ----------------------------------------------------------

    /// The cleanup of an owner whose release the host performed (the `stop`
    /// of `ownership.begin`).
    pub fn owner_stopped(&mut self, host: &mut dyn ControllerHost, owner: OwnerId) {
        if let Some(slot) = self.owner_slots.remove(&owner) {
            if self
                .stream_owners
                .get(&slot)
                .is_some_and(|stream| stream.owner == owner)
            {
                self.stream_owners.remove(&slot);
                self.streams.release_registration(host, slot);
            }
            return;
        }
        let Some(position) = self
            .directories
            .iter()
            .position(|directory| directory.owner == owner && !directory.removed)
        else {
            return;
        };
        self.directories[position].removed = true;
        if self.directories[position].refresh.is_none() {
            self.directories.remove(position);
        }
        let empty = !self.directories.iter().any(|directory| !directory.removed);
        if empty && self.changed_subscribed {
            self.changed_subscribed = false;
            host.terminals_changed_unsubscribe();
        }
    }

    // ---- list ------------------------------------------------------------

    /// `getAllTerminalSessions`.
    fn all_terminals(host: &mut dyn ControllerHost) -> Result<Vec<TerminalInfo>, String> {
        let mut all: Vec<TerminalInfo> = Vec::new();
        for cwd in host.list_directories() {
            for terminal in host.get_terminals(&cwd, None)? {
                if !all.iter().any(|known| known.id == terminal.id) {
                    all.push(terminal);
                }
            }
        }
        Ok(all)
    }

    fn list_response(host: &mut dyn ControllerHost, request: &JsValue, terminals: &[TerminalInfo]) {
        let mut payload = Vec::new();
        push_if(
            &mut payload,
            "cwd",
            request.get("cwd").and_then(JsValue::as_str),
        );
        let infos: Vec<JsValue> = terminals
            .iter()
            .map(|terminal| {
                let terminal = Self::fresh(host, terminal);
                let mut entries = terminal_info_entries(&terminal);
                entries.push(("cwd", text(&terminal.cwd)));
                object(entries)
            })
            .collect();
        payload.push(("terminals", JsValue::Array(infos)));
        payload.push(("requestId", request_id(request)));
        host.emit(message("list_terminals_response", payload));
    }

    fn handle_list(&mut self, host: &mut dyn ControllerHost, token: u64, request: &JsValue) {
        if !host.has_manager() {
            Self::list_response(host, request, &[]);
            host.settled(token, Ok(()));
            return;
        }
        let workspace_id = request.get("workspaceId").and_then(JsValue::as_str);
        let cwd = request.get("cwd").and_then(JsValue::as_str);
        if let Some(workspace_id) = workspace_id {
            let found = Self::all_terminals(host).map(|all| {
                all.into_iter()
                    .filter(|terminal| terminal.workspace_id == workspace_id)
                    .collect::<Vec<_>>()
            });
            Self::finish_list(host, token, request, found);
            return;
        }
        let Some(cwd) = cwd else {
            let found = Self::all_terminals(host);
            Self::finish_list(host, token, request, found);
            return;
        };
        let (terminals, error) = match host.get_terminals(cwd, None) {
            Ok(terminals) => (terminals, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let task = self.new_task(Task::List {
            token,
            request: request.clone(),
            stage: ListStage::Terminals,
            terminals,
            error,
        });
        host.defer_task(task);
    }

    fn finish_list(
        host: &mut dyn ControllerHost,
        token: u64,
        request: &JsValue,
        found: Result<Vec<TerminalInfo>, String>,
    ) {
        // The baseline logs the error and answers with an empty list.
        let terminals = found.unwrap_or_default();
        Self::list_response(host, request, &terminals);
        host.settled(token, Ok(()));
    }

    fn resume_list(
        &mut self,
        host: &mut dyn ControllerHost,
        task: TaskId,
        mut list: Task,
        with: Resume,
    ) {
        let Task::List {
            token,
            request,
            stage,
            terminals,
            error,
        } = &mut list
        else {
            return;
        };
        let cwd = request
            .get("cwd")
            .and_then(JsValue::as_str)
            .unwrap_or("")
            .to_owned();
        match (*stage, with) {
            (ListStage::Terminals, Resume::Tick) => {
                if let Some(error) = error.take() {
                    let (token, request) = (*token, request.clone());
                    Self::finish_list(host, token, &request, Err(error));
                    return;
                }
                *stage = ListStage::Roots;
                self.tasks.insert(task, list);
                host.request_workspace_roots(task);
            }
            (ListStage::Roots, Resume::Roots(roots)) => match roots {
                Err(error) => {
                    let (token, request) = (*token, request.clone());
                    Self::finish_list(host, token, &request, Err(error));
                }
                Ok(roots) => {
                    *terminals =
                        Self::filter_by_roots(host, &cwd, std::mem::take(terminals), &roots);
                    *stage = ListStage::Filtered;
                    self.tasks.insert(task, list);
                    host.defer_task(task);
                }
            },
            (ListStage::Filtered, Resume::Tick) => {
                let (token, request, terminals) = (*token, request.clone(), terminals.clone());
                Self::finish_list(host, token, &request, Ok(terminals));
            }
            _ => {}
        }
    }

    // ---- create ----------------------------------------------------------

    fn create_response(
        host: &mut dyn ControllerHost,
        request: &JsValue,
        terminal: Option<&TerminalInfo>,
        error: Option<&str>,
    ) {
        let terminal = terminal.map_or(JsValue::Null, |terminal| {
            let mut entries = vec![
                ("id", text(&terminal.id)),
                ("name", text(&terminal.name)),
                ("cwd", text(&terminal.cwd)),
                ("workspaceId", text(&terminal.workspace_id)),
            ];
            push_if(&mut entries, "title", terminal.title.as_deref());
            entries.push(("activity", JsValue::Null));
            object(entries)
        });
        host.emit(message(
            "create_terminal_response",
            vec![
                ("terminal", terminal),
                ("error", error.map_or(JsValue::Null, text)),
                ("requestId", request_id(request)),
            ],
        ));
    }

    fn handle_create(&mut self, host: &mut dyn ControllerHost, token: u64, request: &JsValue) {
        if !host.has_manager() {
            Self::create_response(host, request, None, Some("Terminal manager not available"));
            host.settled(token, Ok(()));
            return;
        }
        let agent_id = request.get("agentId").and_then(JsValue::as_str);
        if let Some(agent_id) = truthy_text(agent_id) {
            Self::create_response(
                host,
                request,
                None,
                Some(&format!(
                    "Agent-backed terminals are no longer supported for agent {agent_id}"
                )),
            );
            host.settled(token, Ok(()));
            return;
        }
        // `msg.workspaceId ?? await resolveLegacyTerminalWorkspaceId(msg.cwd)`.
        let given = request
            .get("workspaceId")
            .filter(|value| !value.is_null())
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        if let Some(workspace_id) = given {
            self.create_with_workspace(host, token, request.clone(), Some(workspace_id));
        } else {
            let task = self.new_task(Task::Create {
                token,
                request: request.clone(),
                stage: CreateStage::LegacyRefs,
                workspace_id: None,
            });
            host.request_workspace_refs(task);
        }
    }

    /// From `if (!workspaceId)` on.
    fn create_with_workspace(
        &mut self,
        host: &mut dyn ControllerHost,
        token: u64,
        request: JsValue,
        workspace_id: Option<String>,
    ) {
        let Some(workspace_id) = workspace_id.filter(|id| !id.is_empty()) else {
            Self::create_response(host, &request, None, Some("workspaceId is required"));
            host.settled(token, Ok(()));
            return;
        };
        let task = self.new_task(Task::Create {
            token,
            request,
            stage: CreateStage::CheckRefs,
            workspace_id: Some(workspace_id),
        });
        host.request_workspace_refs(task);
    }

    /// `resolveLegacyTerminalWorkspaceId`.
    fn resolve_legacy_workspace_id(
        host: &mut dyn ControllerHost,
        cwd: &str,
        refs: &[WorkspaceRef],
    ) -> Option<String> {
        if refs.is_empty() {
            return None;
        }
        for workspace in refs {
            if Self::is_same_path(host, &workspace.cwd, cwd) {
                return Some(workspace.workspace_id.clone());
            }
        }
        let roots: Vec<String> = refs.iter().map(|workspace| workspace.cwd.clone()).collect();
        let owner_root = Self::resolve_owner_root(host, cwd, &roots)?;
        for workspace in refs {
            if Self::is_same_path(host, &workspace.cwd, &owner_root) {
                return Some(workspace.workspace_id.clone());
            }
        }
        None
    }

    #[allow(clippy::too_many_lines)]
    fn resume_create(
        &mut self,
        host: &mut dyn ControllerHost,
        task: TaskId,
        create: Task,
        with: Resume,
    ) {
        let Task::Create {
            token,
            request,
            stage,
            workspace_id,
        } = create
        else {
            return;
        };
        let cwd = request
            .get("cwd")
            .and_then(JsValue::as_str)
            .unwrap_or("")
            .to_owned();
        match (stage, with) {
            (CreateStage::LegacyRefs, Resume::Refs(refs)) => match refs {
                Err(error) => {
                    Self::create_response(host, &request, None, Some(&error));
                    host.settled(token, Ok(()));
                }
                Ok(refs) => {
                    let resolved = Self::resolve_legacy_workspace_id(host, &cwd, &refs);
                    self.create_with_workspace(host, token, request, resolved);
                }
            },
            (CreateStage::CheckRefs, Resume::Refs(refs)) => {
                let workspace_id = workspace_id.unwrap_or_default();
                let error = match refs {
                    Err(error) => Some(error),
                    Ok(refs) => (!refs
                        .iter()
                        .any(|workspace| workspace.workspace_id == workspace_id))
                    .then(|| format!("Workspace {workspace_id} is not active or does not exist")),
                };
                if let Some(error) = error {
                    Self::create_response(host, &request, None, Some(&error));
                    host.settled(token, Ok(()));
                    return;
                }
                let create_request = CreateRequest {
                    cwd,
                    workspace_id: workspace_id.clone(),
                    name: request
                        .get("name")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                    command: request
                        .get("command")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                    args: request.get("args").and_then(JsValue::as_array).map(|args| {
                        args.iter()
                            .filter_map(|arg| arg.as_str().map(str::to_owned))
                            .collect()
                    }),
                    rows: Self::size_field(&request, "rows"),
                    cols: Self::size_field(&request, "cols"),
                };
                self.tasks.insert(
                    task,
                    Task::Create {
                        token,
                        request,
                        stage: CreateStage::Creating,
                        workspace_id: Some(workspace_id),
                    },
                );
                host.create_terminal(task, &create_request);
            }
            (CreateStage::Creating, Resume::Created(created)) => {
                match created {
                    Ok(terminal) => {
                        Self::create_response(host, &request, Some(&terminal), None);
                    }
                    Err(error) => Self::create_response(host, &request, None, Some(&error)),
                }
                host.settled(token, Ok(()));
            }
            _ => {}
        }
    }

    /// `msg.size?.rows` or `.cols`.
    fn size_field(request: &JsValue, key: &str) -> Option<u16> {
        let value = request.get("size")?.get(key)?.as_f64()?;
        format!("{value}").parse().ok()
    }

    // ---- rename, capture, input, kill ------------------------------------

    fn handle_rename(host: &mut dyn ControllerHost, request: &JsValue) {
        let respond = |host: &mut dyn ControllerHost, success: bool, error: Option<&str>| {
            host.emit(message(
                "terminal.rename.response",
                vec![
                    ("requestId", request_id(request)),
                    ("success", JsValue::Bool(success)),
                    ("error", error.map_or(JsValue::Null, text)),
                ],
            ));
        };
        let title = js_trim(request.get("title").and_then(JsValue::as_str).unwrap_or(""));
        if title.is_empty() {
            respond(host, false, Some("Title is required"));
            return;
        }
        if js_len(title) > 200 {
            respond(host, false, Some("Title is too long"));
            return;
        }
        if !host.has_manager() {
            respond(host, false, Some("Terminal manager not available"));
            return;
        }
        let terminal_id = request
            .get("terminalId")
            .and_then(JsValue::as_str)
            .unwrap_or("");
        let renamed = host.set_terminal_title(terminal_id, title);
        respond(host, renamed, (!renamed).then_some("Terminal not found"));
    }

    fn capture_response(
        host: &mut dyn ControllerHost,
        request: &JsValue,
        capture: Option<CaptureResult>,
    ) {
        let (lines, total) = capture.map_or((Vec::new(), 0), |capture| {
            (capture.lines, capture.total_lines)
        });
        host.emit(message(
            "capture_terminal_response",
            vec![
                (
                    "terminalId",
                    request
                        .get("terminalId")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("lines", string_list(&lines)),
                ("totalLines", number(total)),
                ("requestId", request_id(request)),
            ],
        ));
    }

    fn handle_capture(host: &mut dyn ControllerHost, request: &JsValue) {
        let terminal_id = request
            .get("terminalId")
            .and_then(JsValue::as_str)
            .unwrap_or("");
        if !host.has_manager() || host.terminal_info(terminal_id).is_none() {
            Self::capture_response(host, request, None);
            return;
        }
        let options = CaptureOptions {
            start: request.get("start").and_then(JsValue::as_f64),
            end: request.get("end").and_then(JsValue::as_f64),
            strip_ansi: request.get("stripAnsi").and_then(JsValue::as_bool),
        };
        // An error is logged and answered with an empty capture.
        let capture = host.capture_terminal(terminal_id, &options).ok();
        Self::capture_response(host, request, capture);
    }

    fn handle_input(host: &mut dyn ControllerHost, request: &JsValue, source: SourceId) {
        if !host.has_manager() {
            return;
        }
        let terminal_id = request
            .get("terminalId")
            .and_then(JsValue::as_str)
            .unwrap_or("");
        if host.terminal_info(terminal_id).is_none() {
            return;
        }
        let Some(input) = request.get("message") else {
            return;
        };
        match input.get("type").and_then(JsValue::as_str) {
            Some("resize") => {
                let rows = input.get("rows").and_then(JsValue::as_f64);
                let cols = input.get("cols").and_then(JsValue::as_f64);
                let intent = match input.get("intent").and_then(JsValue::as_str) {
                    Some("claim") => Some(SizeIntent::Claim),
                    Some("update") => Some(SizeIntent::Update),
                    _ => None,
                };
                if let (Some(rows), Some(cols)) = (rows, cols)
                    && let (Ok(rows), Ok(cols)) = (
                        format!("{rows}").parse::<u16>(),
                        format!("{cols}").parse::<u16>(),
                    )
                {
                    host.apply_terminal_size(
                        terminal_id,
                        source,
                        SizeRequest { rows, cols, intent },
                    );
                }
            }
            Some("input") => {
                let data = input
                    .get("data")
                    .and_then(JsValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                host.terminal_send(terminal_id, &ClientMessage::Input(data));
            }
            _ => host.terminal_send(terminal_id, &ClientMessage::Mouse),
        }
    }

    fn handle_kill(&mut self, host: &mut dyn ControllerHost, token: u64, request: &JsValue) {
        let terminal_id = request
            .get("terminalId")
            .and_then(JsValue::as_str)
            .unwrap_or("")
            .to_owned();
        if !host.has_manager() {
            Self::kill_response(host, request, false);
            host.settled(token, Ok(()));
            return;
        }
        self.streams.detach_stream(host, &terminal_id, true);
        let task = self.new_task(Task::Kill {
            token,
            terminal_id: terminal_id.clone(),
            request_id: request_id(request),
        });
        host.kill_terminal_and_wait(task, &terminal_id, None);
    }

    fn kill_response(host: &mut dyn ControllerHost, request: &JsValue, success: bool) {
        host.emit(message(
            "kill_terminal_response",
            vec![
                (
                    "terminalId",
                    request
                        .get("terminalId")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("success", JsValue::Bool(success)),
                ("requestId", request_id(request)),
            ],
        ));
    }

    // ---- terminal streams ------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn handle_subscribe_terminal(
        &mut self,
        host: &mut dyn ControllerHost,
        token: u64,
        request: &JsValue,
    ) {
        let terminal_id = request
            .get("terminalId")
            .and_then(JsValue::as_str)
            .unwrap_or("")
            .to_owned();
        let respond_error = |host: &mut dyn ControllerHost, error: &str| {
            host.emit(message(
                "subscribe_terminal_response",
                vec![
                    ("terminalId", text(&terminal_id)),
                    ("error", text(error)),
                    ("requestId", request_id(request)),
                ],
            ));
        };
        if !host.has_manager() {
            respond_error(host, "Terminal manager not available");
            host.settled(token, Ok(()));
            return;
        }
        if host.terminal_info(&terminal_id).is_none() {
            respond_error(host, "Terminal not found");
            host.settled(token, Ok(()));
            return;
        }
        let begun =
            match host.begin_owner("terminal-output", &format!("terminal-output:{terminal_id}")) {
                Ok(begun) => begun,
                Err(error) => {
                    host.settled(token, Err(error));
                    return;
                }
            };
        for prior in begun.released_prior {
            self.owner_stopped(host, prior);
        }
        let owner = begun.owner;
        let modern = host.is_modern(owner.source);
        let restore = request.get("restore").and_then(Self::restore_options);
        // COMPAT(ownedSubscriptions): a legacy client sent its size with the
        // restore request instead of a resize claim.
        if !modern && let Some((rows, cols)) = restore.and_then(|restore| restore.size) {
            host.apply_terminal_size(
                &terminal_id,
                owner.source,
                SizeRequest {
                    rows,
                    cols,
                    intent: Some(SizeIntent::Claim),
                },
            );
        }
        let bound = if host.has_binary_channel() {
            self.streams.bind(&terminal_id, restore, !modern)
        } else {
            None
        };
        let Some(bound) = bound else {
            host.release_owner(owner.id);
            self.owner_stopped(host, owner.id);
            respond_error(host, "No terminal stream slots available");
            host.settled(token, Ok(()));
            return;
        };
        self.stream_owners.insert(
            bound.slot,
            StreamOwner {
                owner: owner.id,
                source: owner.source,
            },
        );
        self.owner_slots.insert(owner.id, bound.slot);
        host.stream_bound(bound.slot, owner.id);
        host.terminal_subscribe(bound.slot, &terminal_id, bound.snapshot_mode);
        host.emit(message(
            "subscribe_terminal_response",
            vec![
                ("terminalId", text(&terminal_id)),
                ("slot", number(usize::from(bound.slot))),
                ("subscriptionId", text(&owner.response_id)),
                ("error", JsValue::Null),
                ("requestId", request_id(request)),
            ],
        ));
        if self.streams.contains(bound.slot) {
            self.streams.try_send_snapshot(host, bound.slot);
        }
        host.settled(token, Ok(()));
    }

    /// `msg.restore` as the stream reads it.
    fn restore_options(value: &JsValue) -> Option<RestoreOptions> {
        let mode = match value.get("mode").and_then(JsValue::as_str)? {
            "live" => RestoreMode::Live,
            "visible-snapshot" => RestoreMode::VisibleSnapshot,
            _ => RestoreMode::FullSnapshot,
        };
        let integer = |value: &JsValue, key: &str| -> Option<f64> {
            value.get(key).and_then(JsValue::as_f64)
        };
        let size = value.get("size").and_then(|size| {
            let rows = format!("{}", integer(size, "rows")?).parse().ok()?;
            let cols = format!("{}", integer(size, "cols")?).parse().ok()?;
            Some((rows, cols))
        });
        Some(RestoreOptions {
            mode,
            scrollback_lines: integer(value, "scrollbackLines")
                .and_then(|lines| format!("{lines}").parse().ok()),
            size,
        })
    }

    /// A message of the terminal a stream is subscribed to.
    pub fn terminal_message(
        &mut self,
        host: &mut dyn ControllerHost,
        slot: u8,
        message: ServerMessage,
    ) {
        self.streams.terminal_message(host, slot, message);
    }

    /// The terminal exited: `terminal.onExit`.
    pub fn terminal_exited(&mut self, host: &mut dyn ControllerHost, terminal_id: &str) {
        self.streams.detach_stream(host, terminal_id, true);
    }

    /// `detachStream(terminalId, options)`.
    pub fn detach_stream(
        &mut self,
        host: &mut dyn ControllerHost,
        terminal_id: &str,
        emit_exit: bool,
    ) -> bool {
        self.streams.detach_stream(host, terminal_id, emit_exit)
    }

    /// A stream's trailing timer elapsed.
    pub fn fire_timer(&mut self, host: &mut dyn ControllerHost, slot: u8, token: u64) {
        self.streams.fire_timer(host, slot, token);
    }

    /// A stream's snapshot read settled.
    pub fn snapshot_result(
        &mut self,
        host: &mut dyn ControllerHost,
        slot: u8,
        result: Result<Option<crate::session::StateSnapshot>, String>,
    ) {
        self.streams.snapshot_result(host, slot, result);
    }

    /// A stream's deferred step ([`StreamHost::defer`]).
    pub fn resume_stream(&mut self, host: &mut dyn ControllerHost, slot: u8) {
        self.streams.resume(host, slot);
    }

    /// `handleBinaryFrame(frame, source)`.
    pub fn handle_binary_frame(
        &mut self,
        host: &mut dyn ControllerHost,
        frame: &ClientFrame,
        source: SourceId,
    ) {
        let Some(stream) = self.stream_owners.get(&frame.slot).copied() else {
            return;
        };
        if !self.streams.contains(frame.slot) || stream.source != source || !host.has_manager() {
            return;
        }
        let Some(terminal_id) = self.streams.terminal_of(frame.slot).map(str::to_owned) else {
            return;
        };
        if host.terminal_info(&terminal_id).is_none() {
            self.streams.detach_stream(host, &terminal_id, true);
            return;
        }
        match frame.opcode {
            TerminalOpcode::Input => {
                if frame.payload.is_empty() {
                    return;
                }
                let data = String::from_utf8_lossy(&frame.payload).into_owned();
                if data.is_empty() {
                    return;
                }
                host.terminal_send(&terminal_id, &ClientMessage::Input(data));
            }
            TerminalOpcode::Resize => {
                let Some(resize) = decode_terminal_resize(&frame.payload) else {
                    return;
                };
                let (Ok(rows), Ok(cols)) = (
                    format!("{}", resize.rows).parse::<u16>(),
                    format!("{}", resize.cols).parse::<u16>(),
                ) else {
                    return;
                };
                let intent = resize.intent.map(|intent| match intent {
                    spocky_wire::TerminalResizeIntent::Claim => SizeIntent::Claim,
                    spocky_wire::TerminalResizeIntent::Update => SizeIntent::Update,
                });
                host.apply_terminal_size(&terminal_id, source, SizeRequest { rows, cols, intent });
            }
            _ => {}
        }
    }

    /// `killTerminalForClose(terminalId)`.
    pub fn kill_terminal_for_close(
        &mut self,
        host: &mut dyn ControllerHost,
        terminal_id: &str,
    ) -> KillForClose {
        if !host.has_manager() {
            return KillForClose {
                terminal_id: terminal_id.to_owned(),
                success: false,
            };
        }
        self.streams.detach_stream(host, terminal_id, true);
        host.kill_terminal(terminal_id);
        KillForClose {
            terminal_id: terminal_id.to_owned(),
            success: true,
        }
    }

    /// `killTerminalsForWorkspace(workspaceId)`; the host is told when it
    /// settles.
    pub fn kill_terminals_for_workspace(
        &mut self,
        host: &mut dyn ControllerHost,
        token: u64,
        workspace_id: &str,
    ) {
        if !host.has_manager() {
            host.settled(token, Ok(()));
            return;
        }
        let mut ids = Vec::new();
        for cwd in host.list_directories() {
            // A directory that cannot be listed is skipped.
            for terminal in host
                .get_terminals(&cwd, Some(workspace_id))
                .unwrap_or_default()
            {
                if terminal.workspace_id == workspace_id {
                    ids.push(terminal.id);
                }
            }
        }
        if ids.is_empty() {
            host.settled(token, Ok(()));
            return;
        }
        self.next_group += 1;
        let group = self.next_group;
        self.groups.insert(
            group,
            ArchiveGroup {
                token,
                remaining: ids.len(),
            },
        );
        for id in ids {
            self.streams.detach_stream(host, &id, true);
            let task = self.new_task(Task::ArchiveKill { group });
            host.kill_terminal_and_wait(
                task,
                &id,
                Some(KillTimeouts {
                    graceful: 2000.0,
                    force: 1500.0,
                }),
            );
        }
    }

    /// `dispose()`.
    pub fn dispose(&mut self, host: &mut dyn ControllerHost) {
        if self.changed_subscribed {
            self.changed_subscribed = false;
            host.terminals_changed_unsubscribe();
        }
        // `subscribedDirectories.clear()` does not release the owners, and a
        // refresh in flight carries on.
        for directory in &mut self.directories {
            directory.removed = true;
        }
        self.directories
            .retain(|directory| directory.refresh.is_some());
        for slot in self.streams.slots() {
            host.release(slot);
            if let Some(stream) = self.stream_owners.remove(&slot) {
                self.owner_slots.remove(&stream.owner);
            }
            self.streams.release_registration(host, slot);
        }
    }

    // ---- tasks -----------------------------------------------------------

    /// Answer a task the controller started.
    pub fn resume(&mut self, host: &mut dyn ControllerHost, task: TaskId, with: Resume) {
        let Some(suspended) = self.tasks.remove(&task) else {
            return;
        };
        match suspended {
            Task::Refresh(owner) => self.resume_refresh(host, task, owner, with),
            list @ Task::List { .. } => self.resume_list(host, task, list, with),
            create @ Task::Create { .. } => self.resume_create(host, task, create, with),
            Task::Kill {
                token,
                terminal_id,
                request_id,
            } => {
                let success = matches!(with, Resume::Killed(Ok(())));
                host.emit(message(
                    "kill_terminal_response",
                    vec![
                        ("terminalId", text(&terminal_id)),
                        ("success", JsValue::Bool(success)),
                        ("requestId", request_id),
                    ],
                ));
                host.settled(token, Ok(()));
            }
            Task::ArchiveKill { group } => {
                let Some(entry) = self.groups.get_mut(&group) else {
                    return;
                };
                entry.remaining -= 1;
                if entry.remaining == 0 {
                    let token = entry.token;
                    self.groups.remove(&group);
                    host.settled(token, Ok(()));
                }
            }
        }
    }
}
