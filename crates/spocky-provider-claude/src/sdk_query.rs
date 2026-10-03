//! The Claude Agent SDK 0.3.246 `Query` and `ProcessTransport` paths Paseo
//! uses: the stream-json stdio protocol with Claude Code, control requests
//! (`initialize`, `interrupt`, `set_permission_mode`, `set_model`,
//! `apply_flag_settings`, `rewind_files`, `cancel_async_message`), and the
//! control requests Claude Code sends back (`can_use_tool`,
//! `hook_callback`, and the declines for handlers Paseo does not pass).
//!
//! [`ClaudeQuery`] is the seam `ClaudeQueryFactory` provides in the
//! baseline: tests replace the process-backed query with scripted ones.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_contracts::text::js_trim;
use spocky_session::agent_sdk::{AbortController, AbortReason, AbortSignal, AgentError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::local::{AsyncQueue, Deferred, LocalBoxFuture, run_inline};
use crate::process::{ChildExit, ChildProcess, SpawnFailure, SpawnRequest, kill_after};
use crate::sdk_options::{SdkCallbacks, prepare_launch};

/// `options` passed to `canUseTool`.
#[derive(Clone)]
pub struct CanUseToolOptions {
    pub signal: AbortSignal,
    pub suggestions: Option<JsValue>,
    pub tool_use_id: Option<String>,
}

/// `canUseTool(toolName, input, options)`, resolving a `PermissionResult`.
pub type CanUseTool = Rc<
    dyn Fn(
        String,
        JsValue,
        CanUseToolOptions,
    ) -> LocalBoxFuture<'static, Result<JsValue, AgentError>>,
>;
/// A hook callback: `(input, toolUseId)`, resolving the hook output.
pub type HookCallback =
    Rc<dyn Fn(JsValue, Option<String>) -> LocalBoxFuture<'static, Result<JsValue, AgentError>>>;
/// `spawnClaudeCodeProcess(spawnOptions)`.
pub type SpawnClaudeCodeProcess =
    Rc<dyn Fn(SpawnRequest) -> Result<Rc<ChildProcess>, SpawnFailure>>;

/// The SDK `Options`: data members in the baseline's key order, with
/// function members as `undefined` slots, plus the functions themselves.
#[derive(Clone)]
pub struct ClaudeOptions {
    pub data: JsObject,
    pub can_use_tool: Option<CanUseTool>,
    /// Hook callbacks per event, in the `hooks` option's order.
    pub hooks: Vec<(String, Vec<HookCallback>)>,
    pub stderr: Option<Rc<dyn Fn(String)>>,
    pub spawn: Option<SpawnClaudeCodeProcess>,
}

/// The prompt stream (`createAsyncMessageInput`): user messages in, ended
/// by `end()`.
#[derive(Default)]
pub struct PromptInput {
    queue: AsyncQueue<JsValue, ()>,
}

impl PromptInput {
    /// `push(item)`: ignored once ended.
    pub fn push(&self, item: JsValue) {
        if !self.queue.is_closed() {
            self.queue.enqueue(item);
        }
    }

    /// `end()`.
    pub fn end(&self) {
        self.queue.done();
    }

    /// The next message, `None` once ended.
    pub async fn next(&self) -> Option<JsValue> {
        self.queue.next().await.and_then(Result::ok)
    }
}

/// `ClaudeQueryInput`.
pub struct QueryInput {
    pub prompt: Rc<PromptInput>,
    pub options: ClaudeOptions,
}

/// The SDK `Query` members Paseo calls.
pub trait ClaudeQuery {
    /// `next()` of the message iterator: a message, `None` when done, or
    /// the error it rejects with.
    fn next(&self) -> LocalBoxFuture<'static, Option<Result<JsValue, AgentError>>>;
    /// `interrupt()`.
    fn interrupt(&self) -> LocalBoxFuture<'static, Result<(), AgentError>>;
    /// `close()`.
    fn close(&self);
    /// `return()`.
    fn return_(&self) -> LocalBoxFuture<'static, ()>;
    /// `setPermissionMode(mode)`.
    fn set_permission_mode(&self, mode: &str) -> LocalBoxFuture<'static, Result<(), AgentError>>;
    /// `setModel(model)`.
    fn set_model(&self, model: Option<&str>) -> LocalBoxFuture<'static, Result<(), AgentError>>;
    /// `applyFlagSettings(settings)`.
    fn apply_flag_settings(
        &self,
        settings: JsValue,
    ) -> LocalBoxFuture<'static, Result<(), AgentError>>;
    /// `supportedCommands()`.
    fn supported_commands(&self) -> LocalBoxFuture<'static, Result<JsValue, AgentError>>;
    /// `rewindFiles(userMessageId, { dryRun })`.
    fn rewind_files(
        &self,
        user_message_id: &str,
        dry_run: bool,
    ) -> LocalBoxFuture<'static, Result<JsValue, AgentError>>;
    /// `cancelAsyncMessage(uuid)`, when the query has it.
    fn cancel_async_message(
        &self,
        uuid: &str,
    ) -> Option<LocalBoxFuture<'static, Result<JsValue, AgentError>>>;
}

/// `ClaudeQueryFactory`.
pub type QueryFactory = Rc<dyn Fn(QueryInput) -> Result<Rc<dyn ClaudeQuery>, AgentError>>;

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `Math.random().toString(36).substring(2, 15)`.
fn request_id() -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    bytes
        .iter()
        .take(11)
        .map(|byte| char::from(ALPHABET[usize::from(*byte) % 36]))
        .collect()
}

type ControlResult = Result<JsValue, AgentError>;

#[allow(clippy::struct_excessive_bools)] // The baseline's flags.
struct Inner {
    child: Option<Rc<ChildProcess>>,
    writer: Option<mpsc::UnboundedSender<Option<String>>>,
    ready: bool,
    exit_error: Option<AgentError>,
    exit_event_delivered: bool,
    aborted: Option<AgentError>,
    pending: HashMap<String, Box<dyn FnOnce(ControlResult)>>,
    unmatched: VecDeque<(String, JsValue)>,
    cancel_controllers: HashMap<String, AbortController>,
    hook_callbacks: HashMap<String, HookCallback>,
    latest_commands: Option<JsValue>,
    first_result_received: bool,
    last_error_result_text: Option<String>,
    cleanup_started: bool,
    initialize_request_id: Option<String>,
}

/// The process-backed `Query`.
pub struct ProcessQuery {
    inner: Rc<RefCell<Inner>>,
    messages: Rc<AsyncQueue<JsValue, AgentError>>,
    initialization: Rc<Deferred<ControlResult>>,
    first_result: Rc<Deferred<()>>,
    cleaned_up: Rc<Deferred<()>>,
    can_use_tool: Option<CanUseTool>,
    executable_path: String,
}

const UNMATCHED_CONTROL_RESPONSES_MAX: usize = 1024;
const CLOSE_GRACE: Duration = Duration::from_millis(2000);
const CLOSE_FORCE: Duration = Duration::from_millis(5000);

impl ProcessQuery {
    /// `query({ prompt, options })` with a stream prompt.
    ///
    /// # Errors
    ///
    /// The option validation errors `query()` throws synchronously.
    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)] // One baseline function; the input is consumed by the options it moves.
    pub fn start(input: QueryInput, process_env: &JsObject) -> Result<Rc<Self>, AgentError> {
        let options = &input.options;
        let callbacks = SdkCallbacks {
            can_use_tool: options.can_use_tool.is_some(),
            stderr: options.stderr.is_some(),
            spawn_override: options.spawn.is_some(),
        };
        let mut data = options.data.clone();
        if !options.hooks.is_empty() {
            data.insert("hooks", hooks_shape(&options.hooks));
        }
        let launch = prepare_launch(&data, callbacks, process_env)?;
        let query = Rc::new(Self {
            inner: Rc::new(RefCell::new(Inner {
                child: None,
                writer: None,
                ready: false,
                exit_error: None,
                exit_event_delivered: false,
                aborted: None,
                pending: HashMap::new(),
                unmatched: VecDeque::new(),
                cancel_controllers: HashMap::new(),
                hook_callbacks: HashMap::new(),
                latest_commands: None,
                first_result_received: false,
                last_error_result_text: None,
                cleanup_started: false,
                initialize_request_id: None,
            })),
            messages: Rc::new(AsyncQueue::default()),
            initialization: Deferred::new(),
            first_result: Deferred::new(),
            cleaned_up: Deferred::new(),
            can_use_tool: options.can_use_tool.clone(),
            executable_path: data
                .get("pathToClaudeCodeExecutable")
                .and_then(JsValue::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
        let request = SpawnRequest {
            command: launch.command.clone(),
            args: launch.args.clone(),
            cwd: launch.cwd.clone(),
            env: launch.env.clone(),
        };
        let spawned = match &options.spawn {
            Some(spawn) => spawn(request),
            None => ChildProcess::spawn(&request, options.stderr.clone()),
        };
        query.attach(spawned);
        let mut hooks_payload: Option<JsObject> = (!options.hooks.is_empty()).then(JsObject::new);
        let mut next_callback_id = 0;
        let mut callbacks_by_matcher = options
            .hooks
            .iter()
            .flat_map(|(_, callbacks)| callbacks.iter().cloned());
        for (event, matcher, timeout, count) in &launch.hook_matchers {
            let mut ids = Vec::new();
            for _ in 0..*count {
                let id = format!("hook_{next_callback_id}");
                next_callback_id += 1;
                if let Some(callback) = callbacks_by_matcher.next() {
                    query
                        .inner
                        .borrow_mut()
                        .hook_callbacks
                        .insert(id.clone(), callback);
                }
                ids.push(text(&id));
            }
            if let Some(payload) = hooks_payload.as_mut() {
                let mut entry = JsObject::new();
                entry.insert("matcher", matcher.clone());
                entry.insert("hookCallbackIds", JsValue::Array(ids));
                entry.insert("timeout", timeout.clone());
                let mut list = payload
                    .get(event)
                    .and_then(JsValue::as_array)
                    .map(<[JsValue]>::to_vec)
                    .unwrap_or_default();
                list.push(JsValue::Object(entry));
                payload.insert(event.clone(), JsValue::Array(list));
            }
        }
        let mut init = launch.init.clone();
        init.insert(
            "hooks",
            hooks_payload.map_or(JsValue::Undefined, JsValue::Object),
        );
        let initialization = Rc::clone(&query.initialization);
        let pending = query.request(init);
        tokio::task::spawn_local(async move {
            initialization.settle(pending.await);
        });
        let streamer = Rc::clone(&query);
        let prompt = Rc::clone(&input.prompt);
        let bidirectional = options.can_use_tool.is_some() || !options.hooks.is_empty();
        tokio::task::spawn_local(async move {
            streamer.stream_input(prompt, bidirectional).await;
        });
        Ok(query)
    }

    fn attach(self: &Rc<Self>, spawned: Result<Rc<ChildProcess>, SpawnFailure>) {
        let child = match spawned {
            Ok(child) => child,
            Err(failure) => {
                let error = self.spawn_error(&failure);
                // Node reports a failed spawn as the child's `error` event, a
                // tick later: writes made right after construction (initialize,
                // applyFlagSettings) are accepted and then rejected with the
                // spawn error when the stream fails.
                let (sender, _receiver) = mpsc::unbounded_channel::<Option<String>>();
                {
                    let mut inner = self.inner.borrow_mut();
                    inner.writer = Some(sender);
                    inner.ready = true;
                }
                let query = Rc::clone(self);
                tokio::task::spawn_local(async move {
                    {
                        let mut inner = query.inner.borrow_mut();
                        inner.exit_error = Some(error.clone());
                        inner.ready = false;
                    }
                    query.fail_stream(error).await;
                });
                return;
            }
        };
        let stdin = child.take_stdin();
        let stdout = child.take_stdout();
        let (sender, mut receiver) = mpsc::unbounded_channel::<Option<String>>();
        if let Some(mut stdin) = stdin {
            let inner = Rc::clone(&self.inner);
            tokio::task::spawn_local(async move {
                while let Some(Some(line)) = receiver.recv().await {
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        inner.borrow_mut().ready = false;
                        break;
                    }
                }
                let _ = stdin.shutdown().await;
            });
        }
        {
            let mut inner = self.inner.borrow_mut();
            inner.child = Some(Rc::clone(&child));
            inner.writer = Some(sender);
            inner.ready = true;
        }
        let watcher = Rc::clone(self);
        let exit_child = Rc::clone(&child);
        tokio::task::spawn_local(async move {
            let exit = exit_child.wait_exit().await;
            let mut inner = watcher.inner.borrow_mut();
            inner.exit_event_delivered = true;
            inner.ready = false;
            if let Some(reason) = inner.aborted.clone() {
                inner.exit_error = Some(reason);
            } else if let Some(error) = process_exit_error(&exit) {
                inner.exit_error = Some(error);
            }
        });
        let reader = Rc::clone(self);
        tokio::task::spawn_local(async move {
            reader.read_messages(stdout).await;
        });
    }

    /// The `error` event mapping for a failed spawn.
    fn spawn_error(&self, failure: &SpawnFailure) -> AgentError {
        const CODES: [&str; 7] = [
            "ENOENT",
            "EACCES",
            "EPERM",
            "ENOTDIR",
            "ELOOP",
            "ENAMETOOLONG",
            "EROFS",
        ];
        if failure
            .code
            .as_deref()
            .is_some_and(|code| CODES.contains(&code))
        {
            let path = &self.executable_path;
            let native = ![".js", ".mjs", ".tsx", ".ts", ".jsx"]
                .iter()
                .any(|extension| path.ends_with(extension));
            let exists = std::path::Path::new(path).exists();
            let message = match (exists, native) {
                (true, true) => format!(
                    "Claude Code native binary at {path} exists but failed to launch. This usually means the binary does not match this system's libc — e.g. spawning a musl-linked binary on a glibc Linux host fails because the musl dynamic loader (/lib/ld-musl-*) is missing. Specify a matching binary with options.pathToClaudeCodeExecutable."
                ),
                (true, false) => {
                    format!("Claude Code executable at {path} exists but failed to launch.")
                }
                (false, true) => format!(
                    "Claude Code native binary not found at {path}. Please ensure Claude Code is installed via native installer or specify a valid path with options.pathToClaudeCodeExecutable."
                ),
                (false, false) => format!(
                    "Claude Code executable not found at {path}. Is options.pathToClaudeCodeExecutable set?"
                ),
            };
            return AgentError {
                name: "ReferenceError".to_owned(),
                message,
            };
        }
        AgentError::new(format!(
            "Failed to spawn Claude Code process: {}",
            failure.message
        ))
    }

    async fn fail_stream(self: Rc<Self>, error: AgentError) {
        self.messages.error(error.clone());
        self.cleanup(Some(error)).await;
    }

    /// `transport.write(line)`.
    fn write(&self, line: String) -> Result<(), AgentError> {
        let inner = self.inner.borrow();
        if inner.aborted.is_some() {
            return Err(AgentError {
                name: "AbortError".to_owned(),
                message: "Operation aborted".to_owned(),
            });
        }
        let Some(writer) = inner.writer.as_ref().filter(|_| inner.ready) else {
            return Err(AgentError::new("ProcessTransport is not ready for writing"));
        };
        if inner
            .child
            .as_ref()
            .is_some_and(|child| child.killed() || child.exited().is_some())
        {
            return Err(AgentError::new("Cannot write to terminated process"));
        }
        if let Some(error) = &inner.exit_error {
            return Err(AgentError::new(format!(
                "Cannot write to process that exited with error: {}",
                error.message
            )));
        }
        let _ = writer.send(Some(line));
        Ok(())
    }

    fn end_input(&self) {
        if let Some(writer) = self.inner.borrow_mut().writer.take() {
            let _ = writer.send(None);
        }
    }

    /// `request(body)`: sends a control request and resolves its response.
    fn request(&self, body: JsObject) -> LocalBoxFuture<'static, ControlResult> {
        let id = request_id();
        let subtype = body
            .get("subtype")
            .and_then(JsValue::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut envelope = JsObject::new();
        envelope.insert("request_id", text(&id));
        envelope.insert("type", text("control_request"));
        envelope.insert("request", JsValue::Object(body));
        let result = Deferred::<ControlResult>::new();
        let settle = Rc::clone(&result);
        let resolver: Box<dyn FnOnce(ControlResult)> =
            Box::new(move |response| settle.settle(response));
        if subtype == "initialize" {
            self.inner.borrow_mut().initialize_request_id = Some(id.clone());
        }
        self.inner.borrow_mut().pending.insert(id.clone(), resolver);
        if let Err(error) = self.write(stringify(&JsValue::Object(envelope)) + "\n") {
            self.inner.borrow_mut().pending.remove(&id);
            return Box::pin(async move { Err(error) });
        }
        Box::pin(async move { result.wait().await })
    }

    async fn stream_input(self: Rc<Self>, prompt: Rc<PromptInput>, bidirectional: bool) {
        let mut count = 0_usize;
        while let Some(message) = prompt.next().await {
            count += 1;
            if self.inner.borrow().aborted.is_some() {
                break;
            }
            if let Err(error) = self.write(stringify(&message) + "\n") {
                // `streamInput(...).catch((error) => abortController.abort(error))`.
                if error.name != "AbortError" {
                    self.abort(error);
                }
                return;
            }
        }
        if count > 0 && bidirectional {
            self.wait_for_first_result().await;
        }
        self.end_input();
    }

    async fn wait_for_first_result(&self) {
        if self.inner.borrow().first_result_received {
            return;
        }
        if self.inner.borrow().cleanup_started || self.inner.borrow().aborted.is_some() {
            return;
        }
        tokio::select! {
            () = self.first_result.wait() => {}
            () = self.cleaned_up.wait() => {}
        }
    }

    #[allow(clippy::needless_pass_by_value)] // Mirrors `abortController.abort(reason)`.
    fn abort(self: &Rc<Self>, reason: AgentError) {
        if self.inner.borrow().aborted.is_some() {
            return;
        }
        self.inner.borrow_mut().aborted = Some(AgentError {
            name: "AbortError".to_owned(),
            message: "Claude Code process aborted by user".to_owned(),
        });
        let _ = reason;
        self.close_transport();
    }

    /// `ProcessTransport.close()`.
    fn close_transport(&self) {
        self.end_input();
        let child = self.inner.borrow().child.clone();
        if let Some(child) = child
            && child.exited().is_none()
            && !child.killed()
        {
            let grace = Rc::clone(&child);
            tokio::task::spawn_local(async move {
                tokio::time::sleep(CLOSE_GRACE).await;
                if grace.exited().is_none() {
                    grace.kill("SIGTERM");
                    tokio::task::spawn_local(kill_after(grace, CLOSE_FORCE, "SIGKILL"));
                }
            });
        }
        self.inner.borrow_mut().ready = false;
    }

    async fn read_messages(self: Rc<Self>, stdout: Option<tokio::process::ChildStdout>) {
        let outcome = self.read_lines(stdout).await;
        match outcome {
            Ok(()) => {
                self.first_result.settle(());
                self.messages.done();
                self.cleanup(None).await;
            }
            Err(error) => {
                self.first_result.settle(());
                let last = self.inner.borrow().last_error_result_text.clone();
                let error = match last {
                    Some(text) if error.name != "AbortError" => {
                        AgentError::new(format!("Claude Code returned an error result: {text}"))
                    }
                    _ => error,
                };
                self.messages.error(error.clone());
                self.cleanup(Some(error)).await;
            }
        }
    }

    async fn read_lines(
        self: &Rc<Self>,
        stdout: Option<tokio::process::ChildStdout>,
    ) -> Result<(), AgentError> {
        let Some(mut stdout) = stdout else {
            return Err(AgentError::new(
                "ProcessTransport output stream not available",
            ));
        };
        if let Some(error) = self.inner.borrow().exit_error.clone() {
            return Err(error);
        }
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = vec![0_u8; 64 * 1024];
        let mut skip_newline = false;
        loop {
            let count = stdout.read(&mut chunk).await.unwrap_or(0);
            if count == 0 {
                break;
            }
            for byte in &chunk[..count] {
                if skip_newline && *byte == b'\n' {
                    skip_newline = false;
                    continue;
                }
                skip_newline = false;
                if *byte == b'\n' || *byte == b'\r' {
                    skip_newline = *byte == b'\r';
                    let line = String::from_utf8_lossy(&buffer).into_owned();
                    buffer.clear();
                    self.handle_line(&line);
                } else {
                    buffer.push(*byte);
                }
            }
        }
        if !buffer.is_empty() {
            let line = String::from_utf8_lossy(&buffer).into_owned();
            self.handle_line(&line);
        }
        if let Some(error) = self.inner.borrow().exit_error.clone() {
            return Err(error);
        }
        self.wait_for_exit().await
    }

    /// `transport.waitForExit()`.
    async fn wait_for_exit(&self) -> Result<(), AgentError> {
        let child = {
            let inner = self.inner.borrow();
            if let Some(error) = &inner.exit_error {
                return Err(error.clone());
            }
            inner.child.clone()
        };
        let Some(child) = child else {
            return Ok(());
        };
        if child.exited().is_some_and(|exit| exit.code == Some(0))
            || (child.killed() && self.inner.borrow().exit_event_delivered)
        {
            return Ok(());
        }
        let exit = child.wait_exit().await;
        if self.inner.borrow().aborted.is_some() {
            return Err(AgentError {
                name: "AbortError".to_owned(),
                message: "Operation aborted".to_owned(),
            });
        }
        process_exit_error(&exit).map_or(Ok(()), Err)
    }

    #[allow(clippy::too_many_lines)] // One baseline function.
    fn handle_line(self: &Rc<Self>, line: &str) {
        if js_trim(line).is_empty() {
            return;
        }
        let Ok(message) = parse(line) else {
            return;
        };
        let kind = message
            .get("type")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        match kind.as_deref() {
            Some("control_response") => {
                let response = message.get("response").cloned().unwrap_or(JsValue::Null);
                let id = response
                    .get("request_id")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let handler = self.inner.borrow_mut().pending.remove(&id);
                if let Some(handler) = handler {
                    let outcome = control_outcome(&response);
                    let is_initialize =
                        self.inner.borrow().initialize_request_id.as_deref() == Some(id.as_str());
                    if outcome.is_ok() && is_initialize {
                        // Prompts the CLI parked before this client connected.
                        for pending in response
                            .get("pending_permission_requests")
                            .and_then(JsValue::as_array)
                            .unwrap_or_default()
                        {
                            if pending
                                .get("request")
                                .and_then(|request| request.get("subtype"))
                                .and_then(JsValue::as_str)
                                == Some("can_use_tool")
                            {
                                let query = Rc::clone(self);
                                let pending = pending.clone();
                                run_inline(async move {
                                    query.handle_control_request(pending).await;
                                });
                            }
                        }
                    }
                    handler(outcome);
                } else {
                    let mut inner = self.inner.borrow_mut();
                    if inner.unmatched.len() >= UNMATCHED_CONTROL_RESPONSES_MAX {
                        inner.unmatched.pop_front();
                    }
                    inner.unmatched.push_back((id, response));
                }
                return;
            }
            Some("control_request") => {
                let query = Rc::clone(self);
                run_inline(async move {
                    query.handle_control_request(message).await;
                });
                return;
            }
            Some("control_cancel_request") => {
                let id = message
                    .get("request_id")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Some(controller) = self.inner.borrow_mut().cancel_controllers.remove(&id) {
                    controller.abort(AbortReason::Value(JsValue::Undefined));
                }
                return;
            }
            Some("keep_alive" | "transcript_mirror") => return,
            _ => {}
        }
        let subtype = message.get("subtype").and_then(JsValue::as_str);
        if kind.as_deref() == Some("system")
            && subtype == Some("commands_changed")
            && message
                .get("commands")
                .and_then(JsValue::as_array)
                .is_some()
        {
            self.inner.borrow_mut().latest_commands = message.get("commands").cloned();
        }
        if (kind.as_deref() == Some("system")
            && matches!(subtype, Some("post_turn_summary" | "task_summary")))
            || matches!(kind.as_deref(), Some("active_goal" | "autocompact_state"))
        {
            self.messages.enqueue(message);
            return;
        }
        if kind.as_deref() == Some("result") {
            let text = if spocky_contracts::js::truthy(message.get("is_error")) {
                if subtype == Some("success") {
                    message
                        .get("result")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned)
                } else {
                    Some(
                        message
                            .get("errors")
                            .and_then(JsValue::as_array)
                            .unwrap_or_default()
                            .iter()
                            .map(|entry| js_trim(entry.as_str().unwrap_or_default()).to_owned())
                            .filter(|entry| !entry.is_empty())
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                }
            } else {
                None
            };
            {
                let mut inner = self.inner.borrow_mut();
                inner.last_error_result_text = text.filter(|text| !text.is_empty());
                inner.first_result_received = true;
            }
            self.first_result.settle(());
        } else if !(kind.as_deref() == Some("system") && subtype == Some("session_state_changed")) {
            self.inner.borrow_mut().last_error_result_text = None;
        }
        self.messages.enqueue(message);
    }

    async fn handle_control_request(self: Rc<Self>, message: JsValue) {
        let id = message
            .get("request_id")
            .and_then(JsValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if self.inner.borrow().cancel_controllers.contains_key(&id) {
            return;
        }
        let controller = AbortController::default();
        self.inner
            .borrow_mut()
            .cancel_controllers
            .insert(id.clone(), controller.clone());
        let request = message.get("request").cloned().unwrap_or(JsValue::Null);
        let outcome = self
            .process_control_request(&request, controller.signal())
            .await;
        let cleaned = self.inner.borrow().cleanup_started;
        if !cleaned {
            let mut response = JsObject::new();
            match outcome {
                Ok(None) => {
                    self.inner.borrow_mut().cancel_controllers.remove(&id);
                    return;
                }
                Ok(Some(value)) => {
                    response.insert("subtype", text("success"));
                    response.insert("request_id", text(&id));
                    response.insert("response", value);
                }
                Err(error) => {
                    response.insert("subtype", text("error"));
                    response.insert("request_id", text(&id));
                    response.insert("error", text(&error.message));
                }
            }
            let mut envelope = JsObject::new();
            envelope.insert("type", text("control_response"));
            envelope.insert("response", JsValue::Object(response));
            let _ = self.write(stringify(&JsValue::Object(envelope)) + "\n");
        }
        self.inner.borrow_mut().cancel_controllers.remove(&id);
    }

    /// `processControlRequest(request, signal)`: `Ok(None)` suppresses the
    /// response.
    async fn process_control_request(
        &self,
        request: &JsValue,
        signal: AbortSignal,
    ) -> Result<Option<JsValue>, AgentError> {
        let subtype = request
            .get("subtype")
            .and_then(JsValue::as_str)
            .unwrap_or_default();
        match subtype {
            "can_use_tool" => {
                let Some(can_use_tool) = self.can_use_tool.clone() else {
                    return Err(AgentError::new("canUseTool callback is not provided."));
                };
                let tool_use_id = request
                    .get("tool_use_id")
                    .and_then(JsValue::as_str)
                    .map(str::to_owned);
                let result = can_use_tool(
                    request.get("tool_name").map_or_else(
                        || "undefined".to_owned(),
                        |name| spocky_contracts::js::js_string(Some(name)),
                    ),
                    request.get("input").cloned().unwrap_or(JsValue::Undefined),
                    CanUseToolOptions {
                        signal,
                        suggestions: request
                            .get("permission_suggestions")
                            .filter(|value| !matches!(value, JsValue::Undefined))
                            .cloned(),
                        tool_use_id: tool_use_id.clone(),
                    },
                )
                .await?;
                if result.is_null() {
                    return Ok(None);
                }
                let mut response = spocky_contracts::js::spread(Some(&result));
                response.insert(
                    "toolUseID",
                    tool_use_id.map_or(JsValue::Undefined, JsValue::String),
                );
                Ok(Some(JsValue::Object(response)))
            }
            "hook_callback" => {
                let callback_id = request
                    .get("callback_id")
                    .map(|id| spocky_contracts::js::js_string(Some(id)))
                    .unwrap_or_default();
                let callback = self
                    .inner
                    .borrow()
                    .hook_callbacks
                    .get(&callback_id)
                    .cloned();
                let Some(callback) = callback else {
                    return Err(AgentError::new(format!(
                        "No hook callback found for ID: {callback_id}"
                    )));
                };
                let output = callback(
                    request.get("input").cloned().unwrap_or(JsValue::Undefined),
                    request
                        .get("tool_use_id")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                )
                .await?;
                Ok(Some(output))
            }
            "mcp_message" => Err(AgentError::new(format!(
                "SDK MCP server not found: {}",
                spocky_contracts::js::js_string(request.get("server_name"))
            ))),
            "elicitation" => {
                let mut decline = JsObject::new();
                decline.insert("action", text("decline"));
                Ok(Some(JsValue::Object(decline)))
            }
            "request_user_dialog" => Ok(None),
            "oauth_token_refresh" => {
                Err(AgentError::new("getOAuthToken callback is not provided."))
            }
            "host_auth_token_refresh" => Err(AgentError::new(
                "getHostAuthToken callback is not provided.",
            )),
            other => Err(AgentError::new(format!(
                "Unsupported control request subtype: {other}"
            ))),
        }
    }

    /// `cleanup(error)`.
    async fn cleanup(&self, error: Option<AgentError>) {
        let started = std::mem::replace(&mut self.inner.borrow_mut().cleanup_started, true);
        if started {
            self.cleaned_up.wait().await;
            return;
        }
        let (controllers, pending) = {
            let mut inner = self.inner.borrow_mut();
            (
                std::mem::take(&mut inner.cancel_controllers),
                std::mem::take(&mut inner.pending),
            )
        };
        for controller in controllers.values() {
            controller.abort(AbortReason::Value(JsValue::Undefined));
        }
        self.close_transport();
        let failure = error
            .clone()
            .unwrap_or_else(|| AgentError::new("Query closed before response received"));
        for (_, handler) in pending {
            handler(Err(failure.clone()));
        }
        {
            let mut inner = self.inner.borrow_mut();
            inner.unmatched.clear();
            inner.hook_callbacks.clear();
        }
        match error {
            Some(error) => self.messages.error(error),
            None => self.messages.done(),
        }
        let child = self.inner.borrow().child.clone();
        if let Some(child) = child {
            let _ = tokio::time::timeout(CLOSE_GRACE, child.wait_exit()).await;
        }
        self.cleaned_up.settle(());
    }

    /// `Query.command(body)`: the request is written before this returns.
    fn command(&self, body: JsObject) -> LocalBoxFuture<'static, ControlResult> {
        let pending = self.request(body);
        Box::pin(async move {
            let response = pending.await?;
            Ok(response
                .get("response")
                .cloned()
                .unwrap_or(JsValue::Undefined))
        })
    }
}

fn hooks_shape(hooks: &[(String, Vec<HookCallback>)]) -> JsValue {
    let mut shape = JsObject::new();
    for (event, callbacks) in hooks {
        let mut entry = JsObject::new();
        entry.insert(
            "hooks",
            JsValue::Array(callbacks.iter().map(|_| JsValue::Undefined).collect()),
        );
        shape.insert(event.clone(), JsValue::Array(vec![JsValue::Object(entry)]));
    }
    JsValue::Object(shape)
}

/// A control response: the response object, or its error.
fn control_outcome(response: &JsValue) -> ControlResult {
    if response.get("subtype").and_then(JsValue::as_str) == Some("success") {
        Ok(response.clone())
    } else {
        Err(AgentError::new(spocky_contracts::js::js_string(
            response.get("error"),
        )))
    }
}

/// `getProcessExitError(code, signal)` without a stderr tail (Paseo's spawn
/// override bypasses the SDK's stderr capture).
fn process_exit_error(exit: &ChildExit) -> Option<AgentError> {
    match (&exit.code, &exit.signal) {
        (Some(code), _) if *code != 0 => Some(AgentError::new(format!(
            "Claude Code process exited with code {code}"
        ))),
        (None, Some(signal)) => Some(AgentError::new(format!(
            "Claude Code process terminated by signal {signal}"
        ))),
        _ => None,
    }
}

fn subtype_body(subtype: &str) -> JsObject {
    let mut body = JsObject::new();
    body.insert("subtype", text(subtype));
    body
}

impl ClaudeQuery for Rc<ProcessQuery> {
    fn next(&self) -> LocalBoxFuture<'static, Option<Result<JsValue, AgentError>>> {
        let query = Rc::clone(self);
        Box::pin(async move {
            let next = query.messages.next().await;
            if !matches!(next, Some(Ok(_))) {
                query.cleanup(None).await;
            }
            next
        })
    }

    fn interrupt(&self) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        let pending = self.request(subtype_body("interrupt"));
        Box::pin(async move { pending.await.map(|_| ()) })
    }

    fn close(&self) {
        // `close()` calls the async `cleanup()`, whose body runs synchronously up
        // to its first await: a control request made right after sees the query
        // closed.
        let query = Rc::clone(self);
        run_inline(async move { query.cleanup(None).await });
    }

    fn return_(&self) -> LocalBoxFuture<'static, ()> {
        let query = Rc::clone(self);
        Box::pin(async move { query.cleanup(None).await })
    }

    fn set_permission_mode(&self, mode: &str) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        let mut body = subtype_body("set_permission_mode");
        body.insert("mode", text(mode));
        let pending = self.request(body);
        Box::pin(async move { pending.await.map(|_| ()) })
    }

    fn set_model(&self, model: Option<&str>) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        let mut body = subtype_body("set_model");
        body.insert("model", model.map_or(JsValue::Undefined, text));
        let pending = self.request(body);
        Box::pin(async move { pending.await.map(|_| ()) })
    }

    fn apply_flag_settings(
        &self,
        settings: JsValue,
    ) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        let mut body = subtype_body("apply_flag_settings");
        body.insert("settings", settings);
        let pending = self.request(body);
        Box::pin(async move { pending.await.map(|_| ()) })
    }

    fn supported_commands(&self) -> LocalBoxFuture<'static, Result<JsValue, AgentError>> {
        let query = Rc::clone(self);
        Box::pin(async move {
            let init = query.initialization.wait().await?;
            let commands = init
                .get("response")
                .and_then(|response| response.get("commands"))
                .cloned()
                .unwrap_or(JsValue::Undefined);
            Ok(query
                .inner
                .borrow()
                .latest_commands
                .clone()
                .unwrap_or(commands))
        })
    }

    fn rewind_files(
        &self,
        user_message_id: &str,
        dry_run: bool,
    ) -> LocalBoxFuture<'static, Result<JsValue, AgentError>> {
        let mut body = subtype_body("rewind_files");
        body.insert("user_message_id", text(user_message_id));
        body.insert("dry_run", JsValue::Bool(dry_run));
        self.command(body)
    }

    fn cancel_async_message(
        &self,
        uuid: &str,
    ) -> Option<LocalBoxFuture<'static, Result<JsValue, AgentError>>> {
        let mut body = subtype_body("cancel_async_message");
        body.insert("message_uuid", text(uuid));
        let pending = self.command(body);
        Some(Box::pin(async move {
            let response = pending.await?;
            Ok(response
                .get("cancelled")
                .cloned()
                .unwrap_or(JsValue::Undefined))
        }))
    }
}
