//! Session differential: the pinned `ClaudeAgentClient` and this crate's
//! `ClaudeClient` run the same scripted scenarios against a scripted Query,
//! and their logs (stream events, query calls, step results) must be the same
//! text. Only generated ids are normalized: any UUID becomes `<uuid>`.

mod support;

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::client::{ClaudeClient, ClaudeClientOptions};
use spocky_provider_claude::local::{AsyncQueue, LocalBoxFuture};
use spocky_provider_claude::sdk_query::{CanUseToolOptions, ClaudeQuery, QueryFactory, QueryInput};
use spocky_session::agent_sdk::{
    AbortController, AgentClient, AgentError, AgentPromptInput, AgentRunOptions, AgentSession,
    SteerActiveTurnOptions,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

const SETTLE: Duration = Duration::from_millis(60);
const REACTION_GAP: Duration = Duration::from_millis(5);

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

/// A job the test sends to a scripted query's own thread.
type QueryJob = Box<
    dyn FnOnce(spocky_provider_claude::sdk_query::CanUseTool) -> LocalBoxFuture<'static, ()> + Send,
>;

struct QueryHandle {
    frames: UnboundedSender<Option<JsValue>>,
    jobs: UnboundedSender<QueryJob>,
}

#[derive(Clone)]
struct Shared {
    log: Arc<Mutex<Vec<String>>>,
    /// RESULT lines, flushed into `log` at the end of their step.
    results: Arc<Mutex<Vec<String>>>,
    queries: Arc<Mutex<Vec<QueryHandle>>>,
    prompt_count: Arc<Mutex<usize>>,
    commands: JsValue,
    rewind_replies: JsValue,
    on_prompt: Vec<Vec<JsValue>>,
}

/// Moves the RESULT lines into the log, sorted: see `session_harness.mjs`.
fn flush_results(shared: &Shared) {
    let mut results = shared.results.lock().expect("results");
    results.sort();
    shared.log.lock().expect("log").append(&mut results);
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
        let target = if kind == "RESULT" {
            &self.results
        } else {
            &self.log
        };
        target
            .lock()
            .expect("log")
            .push(format!("{kind} {}", normalize(value)));
    }
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
        Box::pin(async { Ok(()) })
    }

    fn close(&self) {
        self.call("close");
        self.frames.done();
    }

    fn return_(&self) -> LocalBoxFuture<'static, ()> {
        self.call("return");
        self.frames.done();
        Box::pin(async {})
    }

    fn set_permission_mode(&self, mode: &str) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call_with("setPermissionMode", mode);
        Box::pin(async { Ok(()) })
    }

    fn set_model(&self, model: Option<&str>) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call_with("setModel", model.unwrap_or("undefined"));
        Box::pin(async { Ok(()) })
    }

    fn apply_flag_settings(
        &self,
        settings: JsValue,
    ) -> LocalBoxFuture<'static, Result<(), AgentError>> {
        self.call_with("applyFlagSettings", &stringify(&settings));
        Box::pin(async { Ok(()) })
    }

    fn supported_commands(&self) -> LocalBoxFuture<'static, Result<JsValue, AgentError>> {
        let commands = self.shared.commands.clone();
        Box::pin(async move { Ok(commands) })
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
        Some(Box::pin(async { Ok(JsValue::Bool(true)) }))
    }
}

fn make_query(shared: &Shared, input: &QueryInput) -> Rc<dyn ClaudeQuery> {
    let index = shared.queries.lock().expect("queries").len();
    let frames: Rc<AsyncQueue<JsValue, AgentError>> = Rc::default();
    let (frame_sender, mut frame_receiver) = unbounded_channel::<Option<JsValue>>();
    let (job_sender, mut job_receiver) = unbounded_channel::<QueryJob>();
    shared.queries.lock().expect("queries").push(QueryHandle {
        frames: frame_sender,
        jobs: job_sender,
    });
    let data = &input.options.data;
    let mut summary = JsObject::new();
    for key in ["resume", "model", "permissionMode"] {
        summary.insert(key, data.get(key).cloned().unwrap_or(JsValue::Undefined));
    }
    shared.put(
        "CALL",
        &format!("query#{index} {}", stringify(&JsValue::Object(summary))),
    );
    // External frames and jobs arrive from the test thread.
    let forward = Rc::clone(&frames);
    tokio::task::spawn_local(async move {
        while let Some(frame) = frame_receiver.recv().await {
            match frame {
                Some(frame) => forward.enqueue(frame),
                None => forward.done(),
            }
        }
    });
    if let Some(can_use_tool) = input.options.can_use_tool.clone() {
        tokio::task::spawn_local(async move {
            while let Some(job) = job_receiver.recv().await {
                tokio::task::spawn_local(job(Rc::clone(&can_use_tool)));
            }
        });
    }
    // The prompt stream: every user message may trigger scripted frames.
    let prompt = Rc::clone(&input.prompt);
    let reaction_frames = Rc::clone(&frames);
    let reaction_shared = shared.clone();
    tokio::task::spawn_local(async move {
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

fn run_options(value: Option<&JsValue>) -> Option<AgentRunOptions> {
    let value = value?;
    Some(AgentRunOptions {
        client_message_id: value
            .get("clientMessageId")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        ..AgentRunOptions::default()
    })
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

#[allow(clippy::too_many_lines)] // One arm per scenario operation.
fn run_scenario(scenario: &JsValue) -> Vec<String> {
    let shared = Shared {
        log: Arc::default(),
        results: Arc::default(),
        queries: Arc::default(),
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
    let mut session: Option<Arc<dyn AgentSession>> = None;
    let mut aborts: Vec<AbortController> = Vec::new();
    let config = scenario.get("config").cloned().unwrap_or(JsValue::Null);
    for step in scenario
        .get("steps")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        let op = step.get("op").and_then(JsValue::as_str).unwrap_or_default();
        let active = || Arc::clone(session.as_ref().expect("a created session"));
        match op {
            "create" => {
                let created = runtime.block_on(client.create_session(config.clone(), None, None));
                let text = outcome(created, |created| {
                    let id = created.id();
                    let callback_shared = shared.clone();
                    let _unsubscribe = created.subscribe(Arc::new(move |event| {
                        callback_shared.put("EVENT", &stringify(&event));
                    }));
                    session = Some(created);
                    let mut object = JsObject::new();
                    object.insert("id", id.map_or(JsValue::Null, JsValue::String));
                    JsValue::Object(object)
                });
                shared.put("RESULT", &text);
            }
            "startTurn" => {
                let session = active();
                let result = runtime.block_on(session.start_turn(
                    prompt_of(step.get("prompt").unwrap_or(&JsValue::Null)),
                    run_options(step.get("options")),
                ));
                shared.put(
                    "RESULT",
                    &outcome(result, |turn_id| {
                        let mut object = JsObject::new();
                        object.insert("turnId", JsValue::String(turn_id));
                        JsValue::Object(object)
                    }),
                );
            }
            "run" => {
                let session = active();
                let result = runtime.block_on(session.run(
                    prompt_of(step.get("prompt").unwrap_or(&JsValue::Null)),
                    run_options(step.get("options")),
                ));
                shared.put("RESULT", &outcome(result, |value| value));
            }
            "emit" | "emitEnd" => {
                let queries = shared.queries.lock().expect("queries");
                let handle = queries.last().expect("a query");
                let frame = (op == "emit")
                    .then(|| step.get("message").cloned())
                    .flatten();
                let _ = handle.frames.send(frame);
            }
            "wait" => {
                std::thread::sleep(Duration::from_millis(count(step, "ms") as u64));
            }
            "interrupt" => {
                let session = active();
                let result = runtime.block_on(session.interrupt());
                shared.put("RESULT", &outcome(result, |()| JsValue::Undefined));
            }
            "steer" => {
                let session = active();
                let options = step.get("options");
                let steer = SteerActiveTurnOptions {
                    run: AgentRunOptions::default(),
                    clear_pending_permissions: options
                        .and_then(|options| options.get("clearPendingPermissions"))
                        .and_then(JsValue::as_bool),
                    expected_turn_id: options
                        .and_then(|options| options.get("expectedTurnId"))
                        .and_then(JsValue::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                };
                let prompt = prompt_of(step.get("prompt").unwrap_or(&JsValue::Null));
                let future = session.steer_active_turn(&prompt, &steer);
                let result = future.map(|future| runtime.block_on(future));
                shared.put(
                    "RESULT",
                    &match result {
                        Some(result) => outcome(result, |steered| {
                            let mut object = JsObject::new();
                            object.insert(
                                "status",
                                JsValue::String(
                                    match steered {
                                        spocky_session::agent_sdk::SteerResult::Accepted => {
                                            "accepted"
                                        }
                                        spocky_session::agent_sdk::SteerResult::Unavailable => {
                                            "unavailable"
                                        }
                                    }
                                    .to_owned(),
                                ),
                            );
                            JsValue::Object(object)
                        }),
                        None => "null".to_owned(),
                    },
                );
            }
            "setMode" => {
                let session = active();
                let mode = step
                    .get("mode")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default();
                let result = runtime.block_on(session.set_mode(mode));
                shared.put(
                    "RESULT",
                    &outcome(result, |notice| notice.unwrap_or(JsValue::Undefined)),
                );
            }
            "setModel" => {
                let session = active();
                let model = step.get("model").and_then(JsValue::as_str);
                let future = session.set_model(model).expect("setModel");
                let result = runtime.block_on(future);
                shared.put("RESULT", &outcome(result, |()| JsValue::Undefined));
            }
            "setThinking" => {
                let session = active();
                let option = step.get("option").and_then(JsValue::as_str);
                let future = session
                    .set_thinking_option(option)
                    .expect("setThinkingOption");
                let result = runtime.block_on(future);
                shared.put(
                    "RESULT",
                    &outcome(result, |notice| notice.unwrap_or(JsValue::Undefined)),
                );
            }
            "setFeature" => {
                let session = active();
                let id = step.get("id").and_then(JsValue::as_str).unwrap_or_default();
                let value = step.get("value").cloned().unwrap_or(JsValue::Undefined);
                let future = session.set_feature(id, value).expect("setFeature");
                let result = runtime.block_on(future);
                shared.put("RESULT", &outcome(result, |()| JsValue::Undefined));
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
                let result_shared = shared.clone();
                let job: QueryJob = Box::new(move |can_use_tool| {
                    Box::pin(async move {
                        let result = can_use_tool(
                            tool_name,
                            input,
                            CanUseToolOptions {
                                signal,
                                suggestions,
                                tool_use_id,
                            },
                        )
                        .await;
                        result_shared.put(
                            "RESULT",
                            &match result {
                                Ok(value) => format!("canUseTool {}", stringify(&value)),
                                Err(error) => format!("canUseTool ERROR {}", error.message),
                            },
                        );
                    })
                });
                let queries = shared.queries.lock().expect("queries");
                let _ = queries.last().expect("a query").jobs.send(job);
            }
            "abortCanUseTool" => {
                let index = count(step, "index");
                aborts[index].abort(spocky_session::agent_sdk::AbortReason::Value(
                    JsValue::Undefined,
                ));
            }
            "respondPermission" => {
                let session = active();
                let index = count(step, "index");
                let pending = session.get_pending_permissions().expect("pending");
                let id = pending[index]
                    .get("id")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let response = step.get("response").cloned().unwrap_or(JsValue::Undefined);
                let result = runtime.block_on(session.respond_to_permission(&id, response));
                shared.put(
                    "RESULT",
                    &outcome(result, |value| value.unwrap_or(JsValue::Undefined)),
                );
            }
            "pending" => {
                let session = active();
                let pending = session.get_pending_permissions().expect("pending");
                shared.put("RESULT", &stringify(&JsValue::Array(pending)));
            }
            "state" => {
                let session = active();
                let mut state = JsObject::new();
                state.insert("id", session.id().map_or(JsValue::Null, JsValue::String));
                state.insert(
                    "mode",
                    runtime
                        .block_on(session.get_current_mode())
                        .ok()
                        .flatten()
                        .map_or(JsValue::Null, JsValue::String),
                );
                let modes = runtime
                    .block_on(session.get_available_modes())
                    .unwrap_or(JsValue::Undefined);
                state.insert(
                    "modes",
                    JsValue::Array(
                        modes
                            .as_array()
                            .unwrap_or_default()
                            .iter()
                            .map(|mode| mode.get("id").cloned().unwrap_or(JsValue::Undefined))
                            .collect(),
                    ),
                );
                state.insert(
                    "persistence",
                    session.describe_persistence().unwrap_or(JsValue::Null),
                );
                state.insert(
                    "runtime",
                    runtime
                        .block_on(session.get_runtime_info())
                        .unwrap_or(JsValue::Undefined),
                );
                state.insert("features", session.features().unwrap_or(JsValue::Undefined));
                shared.put("RESULT", &stringify(&JsValue::Object(state)));
            }
            "listCommands" => {
                let session = active();
                let future = session.list_commands().expect("listCommands");
                let result = runtime.block_on(future);
                shared.put("RESULT", &outcome(result, |value| value));
            }
            "history" => {
                let session = active();
                let mut stream = session.stream_history();
                let mut events = Vec::new();
                while let Some(Ok(event)) = runtime.block_on(stream.next()) {
                    events.push(event);
                }
                shared.put("RESULT", &stringify(&JsValue::Array(events)));
            }
            "revertFiles" | "revertConversation" => {
                let session = active();
                let message_id = step
                    .get("messageId")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default();
                let future = if op == "revertFiles" {
                    session.revert_files(message_id)
                } else {
                    session.revert_conversation(message_id)
                }
                .expect(op);
                let result = runtime.block_on(future);
                shared.put("RESULT", &outcome(result, |()| JsValue::Undefined));
            }
            "close" => {
                let session = active();
                let result = runtime.block_on(session.close());
                shared.put("RESULT", &outcome(result, |()| JsValue::Undefined));
            }
            other => panic!("unknown op {other}"),
        }
        std::thread::sleep(SETTLE);
        if op == "wait" {
            flush_results(&shared);
        }
    }
    flush_results(&shared);
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
    let scratch = std::env::temp_dir().join(format!("spocky-session-diff-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    let script = include_str!("session_harness.mjs");
    let mut failures = Vec::new();
    for (name, scenario) in scenarios.iter() {
        let file = scratch.join(format!("{name}.json"));
        std::fs::write(&file, stringify(scenario)).expect("scenario file");
        let expected =
            support::run_node(&node, &dist, script, &[file.to_string_lossy().into_owned()]);
        let actual = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_scenario(scenario).join("\n") + "\n"
        }))
        .unwrap_or_else(|_| "<panic>\n".to_owned());
        if let Some(dump) = std::env::var_os("SPOCKY_SESSION_DUMP") {
            let dump = std::path::PathBuf::from(dump);
            std::fs::write(dump.join(format!("node-{name}.log")), &expected).expect("dump");
            std::fs::write(dump.join(format!("rust-{name}.log")), &actual).expect("dump");
        }
        assert!(
            expected.lines().count() > 3,
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
