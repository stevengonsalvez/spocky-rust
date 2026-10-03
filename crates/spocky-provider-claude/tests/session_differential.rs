//! Session differential: the pinned `ClaudeAgentClient` and this crate's
//! `ClaudeClient` run the same scripted scenarios against a scripted Query,
//! and their logs (stream events, query calls, step results) must be the same
//! text, line for line, in the order they happened. The Rust side drives the
//! session on one `LocalSet` (`ClaudeClient::create_local_session`), as the
//! baseline runs on one event loop. Both run in a cleared environment so the
//! full query options (environment included) can be compared. Normalized: any
//! UUID becomes `<uuid>` and the scratch directory `<tmp>`. Both sides carry
//! `NoDefaultCurrentDirectoryInExePath=1` (the pinned SDK sets it in
//! `process.env` when imported and `process_env()` does the same). One member
//! is dropped, from the pinned text only: macOS's `__CF_USER_TEXT_ENCODING`,
//! which CoreFoundation adds to a process started with a cleared environment.

mod support;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::client::{ClaudeClient, ClaudeClientOptions};
use spocky_provider_claude::local::{AsyncQueue, LocalBoxFuture, run_inline};
use spocky_provider_claude::sdk_query::{
    CanUseTool, CanUseToolOptions, ClaudeQuery, QueryFactory, QueryInput,
};
use spocky_provider_claude::session::ClaudeSession;
use spocky_session::agent_sdk::{AbortController, AgentError, AgentPromptInput};

/// The child run finds its scenario here, in its working directory, and writes
/// its log next to it: a file, not an environment variable, so the child's
/// environment is the pinned run's.
const CHILD_SCENARIO: &str = "session-child.json";
const CHILD_LOG: &str = "session-child.log";
const SETTLE: Duration = Duration::from_millis(120);
const REACTION_GAP: Duration = Duration::from_millis(25);

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/agent.js",
        "c8e8e12df50c2bb45b5d33d08bb4c070e454b5919a9ad62932ffa89337f57e69",
    ),
    (
        "server/agent/providers/claude/feature-definitions.js",
        "f847c3459c078daacd9bd456ea6ee9eff1a2eca541274e464590acb0f6191baf",
    ),
    (
        "server/agent/providers/claude/hooks.js",
        "3a48586faa291e9bae2f47dbea1ee20b0dc0a053b0be9d8c313a3b5076cf4d0b",
    ),
    (
        "server/agent/providers/claude/model-manifest.js",
        "9c23d9112ad98b7ee3a789fd30ee524f9f3f54b06fe4c7a887b263ad2b1311d1",
    ),
    (
        "server/agent/providers/claude/models.js",
        "b1b0c2017f73b016eb980ee6748dbb5df61ec9591cd484c1e7e22c2eebefac03",
    ),
    (
        "server/agent/providers/claude/options.js",
        "3f63e785c60e02b8e094487807e34143c4dd8c331b0c5d58b33e09dbf0ce1791",
    ),
    (
        "server/agent/providers/claude/partial-json.js",
        "882aca46cc529af7b930be1869faf7146fb33e03ca4bbc0cc978fd3ced00291f",
    ),
    (
        "server/agent/providers/claude/project-dir.js",
        "b257cd80c247948f22434299062a7d6f36a456b0854325c4902aec30d501235e",
    ),
    (
        "server/agent/providers/claude/query.js",
        "8e3c8cb6e7c09c6cd3b1a88d8fba3569333922556b35c718122bf82872f60842",
    ),
    (
        "server/agent/providers/claude/rewind.js",
        "08202ba87bfd72662f9a115fbea6f2a52462c939f50ff67cfbb00ecaf9a2a2f0",
    ),
    (
        "server/agent/providers/claude/sidechain-tracker.js",
        "6f2de8ccb0e52688caf8030c000c190e98829c37a8370d43bd639ab4e3a978b7",
    ),
    (
        "server/agent/providers/claude/task-notification-tool-call.js",
        "a02dc023fcabf224f368619371a4df2e22784241bd8b719c49176bd648001e2a",
    ),
    (
        "server/agent/providers/claude/task-state.js",
        "514e17f1f0ea7700a74f7ed52be65cf0370195a8b1bc9860f0fcf715ef424433",
    ),
    (
        "server/agent/providers/claude/test-rewind-claude-sdk.js",
        "cd9bd6441126a71d5ba86f84bbf68500f8c116a9fd1a1d116d97ad02b054b5bb",
    ),
    (
        "server/agent/providers/claude/tool-call-detail-parser.js",
        "cef7f2fe967057dfac71c0b0012875e4496c1a4755c0ddf7c9618b6be6174534",
    ),
    (
        "server/agent/providers/claude/tool-call-mapper.js",
        "ce7a4ce016271bc7637d8e226952b0ccee411352fcad9514efba5660339bfda1",
    ),
];

/// What the scenario file scripts, shared with the factory closures.
#[derive(Clone)]
struct Shared {
    log: Arc<Mutex<Vec<String>>>,
    prompt_count: Arc<Mutex<usize>>,
    commands: JsValue,
    rewind_replies: JsValue,
    on_prompt: Vec<Vec<JsValue>>,
    /// `holdSetPermissionMode`: `setPermissionMode` settles once the gate opens
    /// (`releaseSetPermissionMode`).
    hold_set_mode: bool,
    mode_gate: Arc<AtomicBool>,
}

/// A scripted query as the test sees it.
struct QueryHandle {
    frames: Rc<AsyncQueue<JsValue, AgentError>>,
    can_use_tool: Option<CanUseTool>,
}

thread_local! {
    /// Every query the session opened, oldest first.
    static QUERIES: RefCell<Vec<QueryHandle>> = const { RefCell::new(Vec::new()) };
}

fn last_query<T>(work: impl FnOnce(&QueryHandle) -> T) -> T {
    QUERIES.with(|queries| work(queries.borrow().last().expect("a query")))
}

/// `text.replace(UUID, "<uuid>")`.
fn normalize(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let shape = [8, 4, 4, 4, 12];
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let mut cursor = index;
        let mut matched = true;
        for (group, length) in shape.iter().enumerate() {
            if group > 0 {
                if bytes.get(cursor) == Some(&'-') {
                    cursor += 1;
                } else {
                    matched = false;
                    break;
                }
            }
            if (0..*length).all(|offset| {
                bytes
                    .get(cursor + offset)
                    .is_some_and(char::is_ascii_hexdigit)
            }) {
                cursor += length;
            } else {
                matched = false;
                break;
            }
        }
        if matched {
            out.push_str("<uuid>");
            index = cursor;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

impl Shared {
    fn put(&self, kind: &str, value: &str) {
        self.log
            .lock()
            .expect("log")
            .push(format!("{kind} {}", normalize(value)));
    }
}

/// One promise tick: an `await` in the baseline yields even for a settled promise.
async fn tick() {
    tokio::task::yield_now().await;
}

struct ScriptedQuery {
    shared: Shared,
    index: usize,
    frames: Rc<AsyncQueue<JsValue, AgentError>>,
}

impl ScriptedQuery {
    fn call(&self, text: &str) {
        self.shared.put("CALL", &format!("{text}#{}", self.index));
    }

    fn call_with(&self, name: &str, rest: &str) {
        self.shared
            .put("CALL", &format!("{name}#{} {rest}", self.index));
    }
}

impl ClaudeQuery for ScriptedQuery {
    fn next(&self) -> LocalBoxFuture<'static, Option<Result<JsValue, AgentError>>> {
        let frames = Rc::clone(&self.frames);
        Box::pin(async move { frames.next().await })
    }

    fn interrupt(&self) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call("interrupt");
        Box::pin(async {
            tick().await;
            Ok(())
        })
    }

    fn close(&self) {
        self.call("close");
        self.frames.done();
    }

    fn return_(&self) -> LocalBoxFuture<'static, ()> {
        self.call("return");
        self.frames.done();
        Box::pin(async { tick().await })
    }

    fn set_permission_mode(&self, mode: &str) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call_with("setPermissionMode", mode);
        let hold = self.shared.hold_set_mode;
        let gate = Arc::clone(&self.shared.mode_gate);
        Box::pin(async move {
            tick().await;
            while hold && !gate.load(Ordering::SeqCst) {
                tick().await;
            }
            Ok(())
        })
    }

    fn set_model(&self, model: Option<&str>) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call_with("setModel", model.unwrap_or("undefined"));
        Box::pin(async {
            tick().await;
            Ok(())
        })
    }

    fn apply_flag_settings(
        &self,
        settings: JsValue,
    ) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call_with("applyFlagSettings", &stringify(&settings));
        Box::pin(async {
            tick().await;
            Ok(())
        })
    }

    fn supported_commands(&self) -> LocalBoxFuture<'static, Result<JsValue, AgentError>> {
        let commands = self.shared.commands.clone();
        Box::pin(async move {
            tick().await;
            Ok(commands)
        })
    }

    fn rewind_files(
        &self,
        user_message_id: &str,
        dry_run: bool,
    ) -> LocalBoxFuture<'static, Result<JsValue, AgentError>> {
        self.call_with(
            "rewindFiles",
            &format!("{user_message_id} {{\"dryRun\":{dry_run}}}"),
        );
        let reply = self.shared.rewind_replies.get(user_message_id).cloned();
        Box::pin(async move {
            tick().await;
            match reply {
                Some(reply) if reply.as_str() == Some("throw") => {
                    Err(AgentError::new("rewind failed"))
                }
                Some(reply) => Ok(reply),
                None => parse(
                    r#"{"canRewind":true,"filesChanged":["a.ts"],"insertions":1,"deletions":2}"#,
                )
                .map_err(|error| AgentError::new(error.to_string())),
            }
        })
    }

    fn cancel_async_message(
        &self,
        uuid: &str,
    ) -> Option<LocalBoxFuture<'static, Result<JsValue, AgentError>>> {
        self.call_with("cancelAsyncMessage", uuid);
        Some(Box::pin(async {
            tick().await;
            Ok(JsValue::Bool(true))
        }))
    }
}

fn make_query(shared: &Shared, input: &QueryInput) -> Rc<dyn ClaudeQuery> {
    let index = QUERIES.with(|queries| queries.borrow().len());
    let frames: Rc<AsyncQueue<JsValue, AgentError>> = Rc::default();
    QUERIES.with(|queries| {
        queries.borrow_mut().push(QueryHandle {
            frames: Rc::clone(&frames),
            can_use_tool: input.options.can_use_tool.clone(),
        });
    });
    shared.put(
        "CALL",
        &format!(
            "query#{index} {}",
            stringify(&JsValue::Object(input.options.data.clone()))
        ),
    );
    // The prompt stream: every user message may trigger scripted frames.
    let prompt = Rc::clone(&input.prompt);
    let reaction_frames = Rc::clone(&frames);
    let reaction_shared = shared.clone();
    run_inline(async move {
        while let Some(message) = prompt.next().await {
            reaction_shared.put("CALL", &format!("prompt#{index} {}", prompt_text(&message)));
            let position = {
                let mut count = reaction_shared.prompt_count.lock().expect("count");
                let position = *count;
                *count += 1;
                position
            };
            for frame in reaction_shared
                .on_prompt
                .get(position)
                .cloned()
                .unwrap_or_default()
            {
                tokio::time::sleep(REACTION_GAP).await;
                reaction_frames.enqueue(frame);
            }
        }
    });
    Rc::new(ScriptedQuery {
        shared: shared.clone(),
        index,
        frames,
    })
}

/// `promptText(message)` of the harness.
fn prompt_text(message: &JsValue) -> String {
    let content = message
        .get("message")
        .and_then(|inner| inner.get("content"));
    match content {
        Some(JsValue::String(text)) => text.clone(),
        Some(JsValue::Array(blocks)) => blocks
            .iter()
            .map(|block| {
                block.get("text").and_then(JsValue::as_str).map_or_else(
                    || format!("<{}>", spocky_contracts::js::js_string(block.get("type"))),
                    str::to_owned,
                )
            })
            .collect::<Vec<_>>()
            .join("|"),
        _ => String::new(),
    }
}

fn prompt_of(value: &JsValue) -> AgentPromptInput {
    match value {
        JsValue::Array(blocks) => AgentPromptInput::Blocks(blocks.clone()),
        other => AgentPromptInput::Text(spocky_contracts::js::js_string(Some(other))),
    }
}

/// A non-negative whole number member of a step, `0` when absent.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Scenario literals.
fn count(step: &JsValue, key: &str) -> usize {
    step.get(key).and_then(JsValue::as_f64).unwrap_or(0.0) as usize
}

fn outcome<T>(result: Result<T, AgentError>, render: impl FnOnce(T) -> JsValue) -> String {
    match result {
        Ok(value) => match render(value) {
            JsValue::Undefined => "null".to_owned(),
            value => stringify(&value),
        },
        Err(error) => format!("ERROR {}", error.message),
    }
}

/// `{ "id": session.id }`.
fn id_result(session: &ClaudeSession) -> JsValue {
    let mut object = JsObject::new();
    object.insert("id", session.id().map_or(JsValue::Null, JsValue::String));
    JsValue::Object(object)
}

#[allow(clippy::too_many_lines)] // One arm per scenario operation.
/// A step's RESULT line. `await` in the baseline yields to the jobs already
/// queued even for a settled promise (a push to the prompt stream wakes its
/// reader before the caller resumes); one yield models that.
async fn log_result(shared: &Shared, text: &str) {
    tokio::task::yield_now().await;
    shared.put("RESULT", text);
}

#[allow(clippy::too_many_lines)] // One arm per scenario operation.
async fn run_scenario_steps(scenario: &JsValue, shared: &Shared, client: &ClaudeClient) {
    let mut session: Option<Rc<ClaudeSession>> = None;
    let mut aborts: Vec<AbortController> = Vec::new();
    let config = scenario.get("config").cloned().unwrap_or(JsValue::Null);
    for step in scenario
        .get("steps")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        let op = step.get("op").and_then(JsValue::as_str).unwrap_or_default();
        let active = || Rc::clone(session.as_ref().expect("a created session"));
        let prompt = || prompt_of(step.get("prompt").unwrap_or(&JsValue::Null));
        let client_message_id = || {
            step.get("options")
                .and_then(|options| options.get("clientMessageId"))
                .and_then(JsValue::as_str)
                .map(str::to_owned)
        };
        match op {
            "create" | "resume" => {
                let opened = if op == "create" {
                    client.create_local_session(&config)
                } else {
                    client.resume_local_session(
                        step.get("handle").cloned().unwrap_or(JsValue::Null),
                        step.get("overrides"),
                    )
                };
                let text = outcome(opened, |opened| {
                    let callback_shared = shared.clone();
                    opened.subscribe(Arc::new(move |event| {
                        callback_shared.put("EVENT", &stringify(&event));
                    }));
                    let result = id_result(&opened);
                    session = Some(opened);
                    result
                });
                log_result(shared, &text).await;
            }
            "startTurn" => {
                let result = active().start_turn(&prompt(), client_message_id()).await;
                log_result(
                    shared,
                    &outcome(result, |turn_id| {
                        let mut object = JsObject::new();
                        object.insert("turnId", JsValue::String(turn_id));
                        JsValue::Object(object)
                    }),
                )
                .await;
            }
            "run" => {
                let result = active().run(&prompt(), client_message_id()).await;
                log_result(shared, &outcome(result, |value| value)).await;
            }
            "emit" => last_query(|query| {
                query
                    .frames
                    .enqueue(step.get("message").cloned().unwrap_or(JsValue::Undefined));
            }),
            "emitEnd" => last_query(|query| query.frames.done()),
            "emitInterrupt" => {
                // The frames are buffered when the interrupt is requested.
                last_query(|query| {
                    for message in step
                        .get("messages")
                        .and_then(JsValue::as_array)
                        .unwrap_or_default()
                    {
                        query.frames.enqueue(message.clone());
                    }
                });
                let result = active().interrupt().await;
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "wait" => tokio::time::sleep(Duration::from_millis(count(step, "ms") as u64)).await,
            "interrupt" => {
                let result = active().interrupt().await;
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "steer" => {
                let options = step.get("options");
                let expected = options
                    .and_then(|options| options.get("expectedTurnId"))
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let clear = options
                    .and_then(|options| options.get("clearPendingPermissions"))
                    .and_then(JsValue::as_bool)
                    == Some(true);
                let result = active().steer_active_turn(&prompt(), &expected, clear);
                log_result(
                    shared,
                    &outcome(result, |steered| {
                        let mut object = JsObject::new();
                        let status = match steered {
                            spocky_session::agent_sdk::SteerResult::Accepted => "accepted",
                            spocky_session::agent_sdk::SteerResult::Unavailable => "unavailable",
                        };
                        object.insert("status", JsValue::String(status.to_owned()));
                        JsValue::Object(object)
                    }),
                )
                .await;
            }
            "setMode" => {
                let mode = step
                    .get("mode")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default();
                let result = active().set_mode(mode).await;
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "setModel" => {
                let model = step.get("model").and_then(JsValue::as_str);
                let result = active().set_model(model).await;
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "setThinking" => {
                let option = step.get("option").and_then(JsValue::as_str);
                let result = active().set_thinking_option(option);
                log_result(
                    shared,
                    &outcome(result, |notice| notice.unwrap_or(JsValue::Undefined)),
                )
                .await;
            }
            "setFeature" => {
                let id = step.get("id").and_then(JsValue::as_str).unwrap_or_default();
                let value = step.get("value").cloned().unwrap_or(JsValue::Undefined);
                let result = active().set_feature(id, &value).await;
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "canUseTool" => {
                let controller = AbortController::default();
                let signal = controller.signal();
                aborts.push(controller);
                let tool_name = step
                    .get("toolName")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let input = step.get("input").cloned().unwrap_or(JsValue::Undefined);
                let suggestions = step.get("suggestions").cloned();
                let tool_use_id = step
                    .get("toolUseID")
                    .and_then(JsValue::as_str)
                    .map(str::to_owned);
                let can_use_tool =
                    last_query(|query| query.can_use_tool.clone()).expect("canUseTool is set");
                let call = can_use_tool(
                    tool_name,
                    input,
                    CanUseToolOptions {
                        signal,
                        suggestions,
                        tool_use_id,
                    },
                );
                let result_shared = shared.clone();
                // The call starts now; its resolution is logged later.
                run_inline(async move {
                    let result = call.await;
                    result_shared.put(
                        "RESULT",
                        &match result {
                            Ok(value) => format!("canUseTool {}", stringify(&value)),
                            Err(error) => format!("canUseTool ERROR {}", error.message),
                        },
                    );
                });
            }
            "abortCanUseTool" => {
                aborts[count(step, "index")].abort(spocky_session::agent_sdk::AbortReason::Value(
                    JsValue::Undefined,
                ));
            }
            "respondPermission" | "respondPermissionAbort" => {
                let session = active();
                let index = count(step, "index");
                let pending = session.get_pending_permissions();
                let id = pending[index]
                    .get("id")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let response = step.get("response").cloned().unwrap_or(JsValue::Undefined);
                let mut responding = Box::pin(session.respond_to_permission(&id, &response));
                let result = if op == "respondPermissionAbort" {
                    // The abort fires while the response is still being handled.
                    let first = poll_once(responding.as_mut());
                    aborts[count(step, "abortIndex")].abort(
                        spocky_session::agent_sdk::AbortReason::Value(JsValue::Undefined),
                    );
                    match first {
                        std::task::Poll::Ready(result) => result,
                        std::task::Poll::Pending => responding.await,
                    }
                } else {
                    responding.await
                };
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "respondPermissionHeld" => {
                let session = active();
                let pending = session.get_pending_permissions();
                let id = pending[count(step, "index")]
                    .get("id")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let response = step.get("response").cloned().unwrap_or(JsValue::Undefined);
                let held = shared.clone();
                // Not awaited: the held setPermissionMode keeps it pending across
                // the next steps.
                run_inline(async move {
                    let result = session.respond_to_permission(&id, &response).await;
                    log_result(&held, &outcome(result, |()| JsValue::Undefined)).await;
                });
            }
            "releaseSetPermissionMode" => shared.mode_gate.store(true, Ordering::SeqCst),
            "pending" => {
                let pending = active().get_pending_permissions();
                shared.put("RESULT", &stringify(&JsValue::Array(pending)));
            }
            "state" => {
                let session = active();
                let mut state = JsObject::new();
                state.insert("id", session.id().map_or(JsValue::Null, JsValue::String));
                state.insert(
                    "mode",
                    session
                        .get_current_mode()
                        .map_or(JsValue::Null, JsValue::String),
                );
                state.insert(
                    "modes",
                    JsValue::Array(
                        session
                            .get_available_modes()
                            .iter()
                            .map(|mode| mode.get("id").cloned().unwrap_or(JsValue::Undefined))
                            .collect(),
                    ),
                );
                state.insert(
                    "persistence",
                    session.describe_persistence().unwrap_or(JsValue::Null),
                );
                state.insert("runtime", session.get_runtime_info());
                state.insert("features", JsValue::Array(session.features()));
                log_result(shared, &stringify(&JsValue::Object(state))).await;
            }
            "listCommands" => {
                let result = active().list_commands().await;
                log_result(shared, &outcome(result, |value| value)).await;
            }
            "history" => {
                let events = active().stream_history();
                log_result(shared, &stringify(&JsValue::Array(events))).await;
            }
            "revertFiles" | "revertConversation" => {
                let message_id = step
                    .get("messageId")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default();
                let result = if op == "revertFiles" {
                    active().revert_files(message_id).await
                } else {
                    active().revert_conversation(message_id).await
                };
                log_result(shared, &outcome(result, |()| JsValue::Undefined)).await;
            }
            "close" => {
                active().close().await;
                log_result(shared, &outcome(Ok(()), |()| JsValue::Undefined)).await;
            }
            other => panic!("unknown op {other}"),
        }
        tokio::time::sleep(SETTLE).await;
    }
}

/// Polls `future` once with no waker: the part of an async call that runs
/// before its first await.
fn poll_once<F: std::future::Future>(
    mut future: std::pin::Pin<&mut F>,
) -> std::task::Poll<F::Output> {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    future.as_mut().poll(&mut context)
}

fn run_scenario(scenario: &JsValue) -> Vec<String> {
    let shared = Shared {
        log: Arc::default(),
        prompt_count: Arc::default(),
        commands: scenario
            .get("commands")
            .cloned()
            .unwrap_or_else(|| JsValue::Array(Vec::new())),
        rewind_replies: scenario
            .get("rewindReplies")
            .cloned()
            .unwrap_or_else(|| JsValue::Object(JsObject::new())),
        on_prompt: scenario
            .get("onPrompt")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .map(|frames| frames.as_array().unwrap_or_default().to_vec())
            .collect(),
        hold_set_mode: scenario.get("holdSetPermissionMode") == Some(&JsValue::Bool(true)),
        mode_gate: Arc::default(),
    };
    let query_shared = shared.clone();
    let factory: Arc<dyn Fn() -> QueryFactory + Send + Sync> = Arc::new(move || {
        let shared = query_shared.clone();
        Rc::new(move |input: QueryInput| Ok(make_query(&shared, &input)))
    });
    let binary: Arc<dyn Fn() -> spocky_provider_claude::session::ResolveBinary + Send + Sync> =
        Arc::new(|| {
            Rc::new(|| {
                let future: LocalBoxFuture<'static, Result<String, AgentError>> =
                    Box::pin(async { Ok("/usr/bin/claude-scripted".to_owned()) });
                future
            })
        });
    let client = ClaudeClient::new(ClaudeClientOptions {
        query_factory: Some(factory),
        resolve_binary: Some(binary),
        ..ClaudeClientOptions::default()
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, run_scenario_steps(scenario, &shared, &client));
    let log = shared.log.lock().expect("log");
    log.clone()
}

/// The first line where `actual` departs from `expected`, with both lines.
fn first_difference(expected: &str, actual: &str) -> String {
    let mut expected_lines = expected.lines();
    let mut actual_lines = actual.lines();
    let mut line = 0;
    loop {
        match (expected_lines.next(), actual_lines.next()) {
            (None, None) => return "same".to_owned(),
            (node, rust) if node == rust => line += 1,
            (node, rust) => {
                return format!(
                    "line {line}\n  node: {}\n  rust: {}",
                    node.unwrap_or("<end>"),
                    rust.unwrap_or("<end>")
                );
            }
        }
    }
}

/// The environment both builds start from: nothing but these.
fn fixed_env(scratch: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", scratch.join("home")),
        ("CLAUDE_CONFIG_DIR", scratch.join("claude-config")),
    ]
}

/// Writes the scenario's transcript fixtures under the scratch config dir.
fn write_fixtures(scenario: &JsValue, scratch: &Path) {
    let root = scratch.join("claude-config");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("config dir");
    for fixture in scenario
        .get("fixtures")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        let path = root.join(
            fixture
                .get("path")
                .and_then(JsValue::as_str)
                .expect("fixture path"),
        );
        std::fs::create_dir_all(path.parent().expect("parent")).expect("fixture dir");
        let bytes: Vec<u8> = if let Some(hex) = fixture.get("hex").and_then(JsValue::as_str) {
            (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                .collect()
        } else {
            fixture
                .get("lines")
                .and_then(JsValue::as_array)
                .unwrap_or_default()
                .iter()
                .map(|line| stringify(line) + "\n")
                .collect::<String>()
                .into_bytes()
        };
        std::fs::write(path, bytes).expect("fixture");
    }
}

/// `text` without the member `"key":"value"` its runtime added to a JSON
/// object, with one neighbouring comma.
fn without_member(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\":\"");
    let mut out = text.to_owned();
    while let Some(start) = out.find(&needle) {
        let value_start = start + needle.len();
        let end = value_start + out[value_start..].find('"').expect("closing quote") + 1;
        let (from, to) = if out[..start].ends_with(',') {
            (start - 1, end)
        } else if out[end..].starts_with(',') {
            (start, end + 1)
        } else {
            (start, end)
        };
        out.replace_range(from..to, "");
    }
    out
}

fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

fn run_pinned(
    node: &std::ffi::OsString,
    dist: &Path,
    scenario_file: &Path,
    scratch: &Path,
) -> String {
    let output = Command::new(timeout_binary())
        .env_clear()
        .envs(fixed_env(scratch))
        .args(["--kill-after=5", "120"])
        .arg(node)
        .args([
            "--input-type=module",
            "-e",
            include_str!("session_harness.mjs"),
        ])
        .arg(dist)
        .arg(scenario_file)
        .current_dir(scratch)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("node stdout is UTF-8");
    without_member(&text, "__CF_USER_TEXT_ENCODING")
}

fn run_rust(scenario_file: &Path, scratch: &Path) -> String {
    std::fs::copy(scenario_file, scratch.join(CHILD_SCENARIO)).expect("child scenario");
    let out = scratch.join(CHILD_LOG);
    let _ = std::fs::remove_file(&out);
    let output = Command::new(timeout_binary())
        .env_clear()
        .envs(fixed_env(scratch))
        .args(["--kill-after=5", "120"])
        .arg(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "child_runs_the_scenario",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(scratch)
        .output();
    match output {
        Ok(output) if output.status.success() => {
            std::fs::read_to_string(&out).unwrap_or_else(|_| "<no log>\n".to_owned())
        }
        Ok(output) => format!(
            "<child failed>\n{}\n{}\n",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => format!("<child did not start: {error}>\n"),
    }
}

/// The child half: runs the scenario in its working directory against
/// `ClaudeClient` and writes the log. Does nothing outside the differential.
#[test]
fn child_runs_the_scenario() {
    let Ok(text) = std::fs::read_to_string(CHILD_SCENARIO) else {
        return;
    };
    let scenario = parse(&text).expect("JSON");
    let log = run_scenario(&scenario).join("\n") + "\n";
    std::fs::write(CHILD_LOG, log).expect("write the log");
}

#[test]
fn sessions_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scenarios = parse(include_str!("session_scenarios.json")).expect("scenarios JSON");
    let JsValue::Object(ref scenarios) = scenarios else {
        panic!("scenarios are an object");
    };
    let scratch = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp dir")
        .join(format!("spocky-session-diff-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("home")).expect("scratch");
    let mut failures = Vec::new();
    for (name, scenario) in scenarios.iter() {
        let file = scratch.join(format!("{name}.json"));
        std::fs::write(&file, stringify(scenario)).expect("scenario file");
        let mask = |text: String| text.replace(&scratch.to_string_lossy().into_owned(), "<tmp>");
        write_fixtures(scenario, &scratch);
        let expected = mask(run_pinned(&node, &dist, &file, &scratch));
        write_fixtures(scenario, &scratch);
        let actual = mask(run_rust(&file, &scratch));
        if let Some(dump) = std::env::var_os("SPOCKY_SESSION_DUMP") {
            let dump = PathBuf::from(dump);
            std::fs::write(dump.join(format!("node-{name}.log")), &expected).expect("dump");
            std::fs::write(dump.join(format!("rust-{name}.log")), &actual).expect("dump");
        }
        assert!(
            expected.lines().count() >= 2,
            "{name}: the pinned run produced no log"
        );
        if actual != expected {
            failures.push(format!("{name}: {}", first_difference(&expected, &actual)));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} scenarios differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
