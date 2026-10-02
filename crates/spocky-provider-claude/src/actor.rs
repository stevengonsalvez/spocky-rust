//! The session's thread: a current-thread runtime with a `LocalSet`, so the
//! session runs on one event loop as the baseline does, and the
//! [`AgentSession`] handle that talks to it from any thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::agent_sdk::{
    AgentError, AgentEventStream, AgentPromptInput, AgentResult, AgentSession, AgentStreamEvent,
    BoxFuture, ImportedTimelineEntry, SteerActiveTurnOptions, SteerResult, StreamCallback,
    Unsubscribe,
};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::local::LocalBoxFuture;
use crate::session::{ClaudeSession, SessionOptions, claude_capabilities};

static NEXT_ACTOR_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The session of the actor running on this thread, with its id.
    static CURRENT: RefCell<Option<(u64, Rc<ClaudeSession>)>> = const { RefCell::new(None) };
}

type Job = Box<dyn FnOnce(Rc<ClaudeSession>) -> LocalBoxFuture<'static, ()> + Send>;

enum Message {
    Job(Job),
    Stop,
}

/// Builds the non-`Send` session options on the actor's own thread.
pub type OptionsFactory = Box<dyn FnOnce() -> SessionOptions + Send>;

/// The thread that owns one [`ClaudeSession`].
pub struct ClaudeActor {
    id: u64,
    sender: tokio_mpsc::UnboundedSender<Message>,
}

impl ClaudeActor {
    /// Starts the thread, builds the session on it, and returns once the
    /// constructor has finished.
    ///
    /// # Errors
    ///
    /// The session constructor's failure, or a thread that could not start.
    pub fn spawn(config: JsObject, options: OptionsFactory) -> Result<Arc<Self>, AgentError> {
        let id = NEXT_ACTOR_ID.fetch_add(1, Ordering::Relaxed);
        let (sender, mut receiver) = tokio_mpsc::unbounded_channel::<Message>();
        let (ready_sender, ready_receiver) = mpsc::channel::<Result<(), AgentError>>();
        let spawned = std::thread::Builder::new()
            .name("spocky-claude-session".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_sender.send(Err(AgentError::new(error.to_string())));
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    let session = match ClaudeSession::new(config, options()) {
                        Ok(session) => session,
                        Err(error) => {
                            let _ = ready_sender.send(Err(error));
                            return;
                        }
                    };
                    CURRENT.with(|current| {
                        *current.borrow_mut() = Some((id, Rc::clone(&session)));
                    });
                    let _ = ready_sender.send(Ok(()));
                    while let Some(message) = receiver.recv().await {
                        match message {
                            Message::Job(job) => {
                                tokio::task::spawn_local(job(Rc::clone(&session)));
                            }
                            Message::Stop => break,
                        }
                    }
                    CURRENT.with(|current| *current.borrow_mut() = None);
                });
            });
        spawned.map_err(|error| AgentError::new(error.to_string()))?;
        ready_receiver
            .recv()
            .map_err(|_| AgentError::new("Claude session thread stopped"))??;
        Ok(Arc::new(Self { id, sender }))
    }

    fn on_actor_thread(&self) -> Option<Rc<ClaudeSession>> {
        CURRENT.with(|current| {
            current
                .borrow()
                .as_ref()
                .filter(|(id, _)| *id == self.id)
                .map(|(_, session)| Rc::clone(session))
        })
    }

    /// Runs `work` on the actor and resolves its result.
    pub fn call<T, F>(self: &Arc<Self>, work: F) -> BoxFuture<'static, AgentResult<T>>
    where
        T: Send + 'static,
        F: FnOnce(Rc<ClaudeSession>) -> LocalBoxFuture<'static, T> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |session| {
            let future = work(session);
            Box::pin(async move {
                let _ = reply.send(future.await);
            })
        });
        let sent = self.sender.send(Message::Job(job));
        Box::pin(async move {
            sent.map_err(|_| AgentError::new("Claude session is closed"))?;
            answer
                .await
                .map_err(|_| AgentError::new("Claude session is closed"))
        })
    }

    /// Runs a synchronous `work` against the session, directly when the
    /// caller is the actor's own thread (a subscriber callback), else by
    /// blocking until the actor has run it.
    pub fn query<T, F>(&self, work: F) -> Option<T>
    where
        T: Send + 'static,
        F: FnOnce(&Rc<ClaudeSession>) -> T + Send + 'static,
    {
        if let Some(session) = self.on_actor_thread() {
            return Some(work(&session));
        }
        let (reply, answer) = mpsc::channel();
        let job: Job = Box::new(move |session| {
            let value = work(&session);
            Box::pin(async move {
                let _ = reply.send(value);
            })
        });
        self.sender.send(Message::Job(job)).ok()?;
        answer.recv().ok()
    }

    /// Stops the thread after the queued work.
    pub fn stop(&self) {
        let _ = self.sender.send(Message::Stop);
    }
}

/// The history stream: the session's replayed events, read on first use.
struct HistoryStream {
    actor: Arc<ClaudeActor>,
    events: Option<std::collections::VecDeque<AgentStreamEvent>>,
}

impl AgentEventStream for HistoryStream {
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>> {
        Box::pin(async move {
            if self.events.is_none() {
                let drained = self
                    .actor
                    .query(|session| session.stream_history())
                    .unwrap_or_default();
                self.events = Some(drained.into());
            }
            self.events.as_mut()?.pop_front().map(Ok)
        })
    }
}

/// The `AgentSession` of a Claude session on its thread.
pub struct ClaudeSessionHandle {
    actor: Arc<ClaudeActor>,
}

impl ClaudeSessionHandle {
    /// Wraps the actor.
    #[must_use]
    pub fn new(actor: Arc<ClaudeActor>) -> Self {
        Self { actor }
    }
}

fn flatten<T>(outcome: AgentResult<AgentResult<T>>) -> AgentResult<T> {
    outcome?
}

impl AgentSession for ClaudeSessionHandle {
    fn provider(&self) -> String {
        "claude".to_owned()
    }

    fn id(&self) -> Option<String> {
        self.actor.query(|session| session.id()).flatten()
    }

    fn capabilities(&self) -> JsValue {
        claude_capabilities()
    }

    fn features(&self) -> Option<JsValue> {
        self.actor
            .query(|session| JsValue::Array(session.features()))
    }

    fn initial_timeline(&self) -> Option<Vec<ImportedTimelineEntry>> {
        None
    }

    fn get_usage_reference(&self) -> Option<BoxFuture<'_, AgentResult<Option<JsValue>>>> {
        Some(Box::pin(async move {
            Ok(self
                .actor
                .query(|session| session.get_usage_reference())
                .flatten())
        }))
    }

    fn run(
        &self,
        prompt: AgentPromptInput,
        options: Option<spocky_session::agent_sdk::AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        let client_message_id = options.and_then(|options| options.client_message_id);
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.run(&prompt, client_message_id).await })
        });
        Box::pin(async move { flatten(call.await) })
    }

    fn start_turn(
        &self,
        prompt: AgentPromptInput,
        options: Option<spocky_session::agent_sdk::AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>> {
        let client_message_id = options.and_then(|options| options.client_message_id);
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.start_turn(&prompt, client_message_id).await })
        });
        Box::pin(async move { flatten(call.await) })
    }

    fn steer_active_turn(
        &self,
        prompt: &AgentPromptInput,
        options: &SteerActiveTurnOptions,
    ) -> Option<BoxFuture<'_, AgentResult<SteerResult>>> {
        let prompt = prompt.clone();
        let expected = options.expected_turn_id.clone();
        let clear = options.clear_pending_permissions == Some(true);
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.steer_active_turn(&prompt, &expected, clear) })
        });
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn subscribe(&self, callback: StreamCallback) -> Unsubscribe {
        let id = self
            .actor
            .query(move |session| session.subscribe(callback))
            .unwrap_or(0);
        let actor = Arc::clone(&self.actor);
        Box::new(move || {
            let _ = actor.query(move |session| session.unsubscribe(id));
        })
    }

    fn stream_history(&self) -> Box<dyn AgentEventStream> {
        Box::new(HistoryStream {
            actor: Arc::clone(&self.actor),
            events: None,
        })
    }

    fn get_runtime_info(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        let call = self
            .actor
            .call(|session| Box::pin(async move { session.get_runtime_info() }));
        Box::pin(call)
    }

    fn get_available_modes(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        let call = self
            .actor
            .call(|session| Box::pin(async move { JsValue::Array(session.get_available_modes()) }));
        Box::pin(call)
    }

    fn get_current_mode(&self) -> BoxFuture<'_, AgentResult<Option<String>>> {
        let call = self
            .actor
            .call(|session| Box::pin(async move { session.get_current_mode() }));
        Box::pin(call)
    }

    fn set_mode(&self, mode_id: &str) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        let mode_id = mode_id.to_owned();
        let call = self
            .actor
            .call(move |session| Box::pin(async move { session.set_mode(&mode_id).await }));
        Box::pin(async move { flatten(call.await).map(|()| None) })
    }

    fn get_pending_permissions(&self) -> AgentResult<Vec<JsValue>> {
        self.actor
            .query(|session| session.get_pending_permissions())
            .ok_or_else(|| AgentError::new("Claude session is closed"))
    }

    fn respond_to_permission(
        &self,
        request_id: &str,
        response: JsValue,
    ) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        let request_id = request_id.to_owned();
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.respond_to_permission(&request_id, &response).await })
        });
        Box::pin(async move { flatten(call.await).map(|()| None) })
    }

    fn describe_persistence(&self) -> Option<JsValue> {
        self.actor
            .query(|session| session.describe_persistence())
            .flatten()
    }

    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>> {
        let call = self
            .actor
            .call(|session| Box::pin(async move { session.interrupt().await }));
        Box::pin(async move { flatten(call.await) })
    }

    fn close(&self) -> BoxFuture<'_, AgentResult<()>> {
        let call = self.actor.call(|session| {
            Box::pin(async move {
                session.close().await;
            })
        });
        let actor = Arc::clone(&self.actor);
        Box::pin(async move {
            let outcome = call.await;
            actor.stop();
            outcome
        })
    }

    fn list_commands(&self) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        let call = self
            .actor
            .call(|session| Box::pin(async move { session.list_commands().await }));
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn set_model(&self, model_id: Option<&str>) -> Option<BoxFuture<'_, AgentResult<()>>> {
        let model_id = model_id.map(str::to_owned);
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.set_model(model_id.as_deref()).await })
        });
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn set_thinking_option(
        &self,
        thinking_option_id: Option<&str>,
    ) -> Option<BoxFuture<'_, AgentResult<Option<JsValue>>>> {
        let option = thinking_option_id.map(str::to_owned);
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.set_thinking_option(option.as_deref()) })
        });
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn set_feature(
        &self,
        feature_id: &str,
        value: JsValue,
    ) -> Option<BoxFuture<'_, AgentResult<()>>> {
        let feature_id = feature_id.to_owned();
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.set_feature(&feature_id, &value).await })
        });
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn revert_conversation(&self, message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        let message_id = message_id.to_owned();
        let call = self.actor.call(move |session| {
            Box::pin(async move { session.revert_conversation(&message_id).await })
        });
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn revert_files(&self, message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        let message_id = message_id.to_owned();
        let call = self
            .actor
            .call(move |session| Box::pin(async move { session.revert_files(&message_id).await }));
        Some(Box::pin(async move { flatten(call.await) }))
    }

    fn revert_both(&self, message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        let message_id = message_id.to_owned();
        let call = self
            .actor
            .call(move |session| Box::pin(async move { session.revert_both(&message_id).await }));
        Some(Box::pin(async move { flatten(call.await) }))
    }
}
