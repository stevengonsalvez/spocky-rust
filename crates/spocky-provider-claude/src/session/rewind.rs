//! Rewind: the `/rewind` command, file and conversation reverts, and the
//! transcript anchors they rely on.

use std::rc::Rc;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::{is_js_whitespace, js_trim};
use spocky_session::agent_sdk::AgentError;

use super::turns::SlashCommand;
use super::{ClaudeSession, RewindAnchor, RewindSdk, text};
use crate::fork_session::RealRewindSdk;
use crate::sdk_query::ClaudeQuery;
use crate::transcript::{
    is_synthetic_user_entry, is_tool_result_user_entry, read_parent_tool_use_id,
    read_trimmed_string,
};

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `UUID_PATTERN`: `/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i`.
fn is_uuid(candidate: &str) -> bool {
    let groups: Vec<&str> = candidate.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    if groups.len() != 5 {
        return false;
    }
    let hex = |group: &str| group.chars().all(|character| character.is_ascii_hexdigit());
    groups
        .iter()
        .zip(lengths)
        .all(|(group, length)| group.len() == length && hex(group))
        && matches!(groups[2].as_bytes()[0], b'1'..=b'8')
        && matches!(
            groups[3].as_bytes()[0].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
}

/// `ClaudeConversationRewindTarget`.
enum RewindTarget {
    Fork(String),
    FreshSession,
}

/// The result of `attemptRewind`.
struct RewindAttempt {
    message_id: Option<String>,
    result: Option<JsValue>,
    error: Option<String>,
}

/// `createClaudeSessionChangedNotice(oldSessionId, newSessionId)`.
pub(crate) fn session_changed_notice(old_session_id: &str, new_session_id: &str) -> JsValue {
    let mut item = JsObject::new();
    item.insert("type", text("assistant_message"));
    item.insert(
        "text",
        JsValue::String(format!(
            "Claude switched to a new session: {old_session_id} -> {new_session_id}"
        )),
    );
    JsValue::Object(item)
}

fn plural_files(count: usize) -> String {
    format!("{count} file{}", if count == 1 { "" } else { "s" })
}

impl ClaudeSession {
    /// `rememberUserMessageId(messageId)`.
    pub(crate) fn remember_user_message_id(&self, message_id: Option<&str>) {
        let Some(message_id) = message_id.filter(|id| !id.is_empty()) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        if state.user_message_ids.last().map(String::as_str) == Some(message_id) {
            return;
        }
        state.user_message_ids.push(message_id.to_owned());
    }

    /// `rememberEmittedUserMessageId(messageId)`.
    pub(crate) fn remember_emitted_user_message_id(&self, message_id: Option<&str>) {
        if let Some(message_id) = message_id.filter(|id| !id.is_empty()) {
            self.state
                .borrow_mut()
                .emitted_user_message_ids
                .insert(message_id.to_owned());
        }
    }

    /// `rememberRewindUserAnchor(userMessageId)`.
    pub(crate) fn remember_rewind_user_anchor(&self, user_message_id: Option<&str>) {
        let Some(id) = user_message_id.filter(|id| !id.is_empty()) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        if state
            .rewind_turn_anchors
            .iter()
            .any(|anchor| anchor.user_message_id == id)
        {
            return;
        }
        state.rewind_turn_anchors.push(RewindAnchor {
            user_message_id: id.to_owned(),
            assistant_message_id: None,
        });
    }

    /// `rememberRewindAssistantAnchor(assistantMessageId)`.
    pub(crate) fn remember_rewind_assistant_anchor(&self, assistant_message_id: Option<&str>) {
        let Some(id) = assistant_message_id.filter(|id| !id.is_empty()) else {
            return;
        };
        if let Some(anchor) = self.state.borrow_mut().rewind_turn_anchors.last_mut() {
            anchor.assistant_message_id = Some(id.to_owned());
        }
    }

    /// `rememberTranscriptProgress(message, messageId)`.
    pub(crate) fn remember_transcript_progress(&self, message: &JsValue, message_id: Option<&str>) {
        let Some(message_id) = message_id.filter(|id| !id.is_empty()) else {
            return;
        };
        // Subagent frames carry uuids that live on the subagent's sidechain, so
        // forkSession cannot resolve one; the live stream skips them too.
        if read_parent_tool_use_id(message).is_some() {
            return;
        }
        match str_of(message, "type") {
            Some("user")
                if !is_synthetic_user_entry(message) && !is_tool_result_user_entry(message) =>
            {
                self.remember_rewind_user_anchor(Some(message_id));
            }
            Some("assistant") => self.remember_rewind_assistant_anchor(Some(message_id)),
            Some("stream_event") => {
                let event_type = message
                    .get("event")
                    .filter(|event| event.is_object())
                    .and_then(|event| read_trimmed_string(event.get("type")));
                if event_type.as_deref() == Some("message_start") {
                    self.remember_rewind_assistant_anchor(Some(message_id));
                }
            }
            _ => {}
        }
    }

    /// `resolveConversationRewindTarget(messageId)`.
    fn resolve_conversation_rewind_target(
        &self,
        message_id: &str,
    ) -> Result<RewindTarget, AgentError> {
        let state = self.state.borrow();
        let Some(index) = state
            .rewind_turn_anchors
            .iter()
            .position(|anchor| anchor.user_message_id == message_id)
        else {
            return Err(AgentError::new(format!(
                "Claude rewind target {message_id} is not in the tracked conversation"
            )));
        };
        // A turn the model never answered carries no state worth preserving:
        // fork at the most recent earlier turn that did answer.
        for previous in (0..index).rev() {
            if let Some(assistant) = state.rewind_turn_anchors[previous]
                .assistant_message_id
                .as_ref()
                .filter(|id| !id.is_empty())
            {
                return Ok(RewindTarget::Fork(assistant.clone()));
            }
        }
        Ok(RewindTarget::FreshSession)
    }

    /// `startFreshConversationSession()`.
    fn start_fresh_conversation_session(&self) {
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut state = self.state.borrow_mut();
        state.claude_session_id = Some(session_id.clone());
        state.pending_fresh_session_id = Some(session_id);
        state.persistence = None;
        state.cached_runtime_info = None;
        state.query_restart_needed = true;
        state.persisted_history.clear();
        state.persisted_provider_subagent_events.clear();
        state.history_pending = false;
        state.user_message_ids.clear();
        state.emitted_user_message_ids.clear();
        state.rewind_turn_anchors.clear();
        state.task_state.reset();
    }

    /// `rebindConversationSession(sessionId)`.
    fn rebind_conversation_session(&self, session_id: &str) {
        let old_session_id = {
            let mut state = self.state.borrow_mut();
            let old = state.claude_session_id.replace(session_id.to_owned());
            state.pending_fresh_session_id = None;
            state.persistence = None;
            state.cached_runtime_info = None;
            state.query_restart_needed = true;
            state.persisted_history.clear();
            state.persisted_provider_subagent_events.clear();
            state.history_pending = false;
            state.user_message_ids.clear();
            state.emitted_user_message_ids.clear();
            state.rewind_turn_anchors.clear();
            state.task_state.reset();
            old
        };
        self.load_persisted_history(session_id);
        if let Some(old) = old_session_id.filter(|old| !old.is_empty() && old != session_id) {
            let mut timeline = JsObject::new();
            timeline.insert("type", text("timeline"));
            timeline.insert("provider", text("claude"));
            timeline.insert("item", session_changed_notice(&old, session_id));
            let mut started = JsObject::new();
            started.insert("type", text("thread_started"));
            started.insert("provider", text("claude"));
            started.insert("sessionId", text(session_id));
            self.dispatch_events(vec![JsValue::Object(timeline), JsValue::Object(started)]);
        }
    }

    /// `revertConversation({ messageId })`.
    ///
    /// # Errors
    ///
    /// An untracked message, a missing session, or the fork's failure.
    pub async fn revert_conversation(self: &Rc<Self>, message_id: &str) -> Result<(), AgentError> {
        match self.resolve_conversation_rewind_target(message_id)? {
            RewindTarget::FreshSession => {
                self.start_fresh_conversation_session();
                Ok(())
            }
            RewindTarget::Fork(target) => {
                let session_id = self
                    .state
                    .borrow()
                    .claude_session_id
                    .clone()
                    .filter(|id| !id.is_empty());
                let Some(session_id) = session_id else {
                    return Err(AgentError::new("Claude session is not ready for rewind"));
                };
                let sdk: Rc<dyn RewindSdk> = self
                    .options
                    .rewind_sdk
                    .clone()
                    .unwrap_or_else(|| Rc::new(RealRewindSdk));
                let forked = sdk.fork_session(&session_id, &target).await?;
                self.rebind_conversation_session(&forked);
                Ok(())
            }
        }
    }

    /// `revertFiles({ messageId })`.
    ///
    /// # Errors
    ///
    /// The query's failure, or a missing checkpoint.
    pub async fn revert_files(self: &Rc<Self>, message_id: &str) -> Result<(), AgentError> {
        let query = self.ensure_query().await?;
        let result = query.rewind_files(message_id, false).await?;
        if spocky_contracts::js::truthy(result.get("canRewind")) {
            return Ok(());
        }
        Err(AgentError::new(str_of(&result, "error").map_or_else(
            || format!("No file checkpoint found for message {message_id}"),
            str::to_owned,
        )))
    }

    /// `revertBoth({ messageId })`.
    ///
    /// # Errors
    ///
    /// Either revert's failure.
    pub async fn revert_both(self: &Rc<Self>, message_id: &str) -> Result<(), AgentError> {
        self.revert_files(message_id).await?;
        self.revert_conversation(message_id).await
    }

    /// `buildRewindSuccessMessage(targetUserMessageId, rewindResult)`.
    fn build_rewind_success_message(target_user_message_id: &str, result: &JsValue) -> String {
        let mut stats: Vec<String> = Vec::new();
        if let Some(files) = result.get("filesChanged").and_then(JsValue::as_array) {
            stats.push(plural_files(files.len()));
        }
        let number = |key: &str| {
            result
                .get(key)
                .and_then(JsValue::as_f64)
                .map(|value| spocky_contracts::js::js_string(Some(&JsValue::Number(value))))
        };
        if let Some(insertions) = number("insertions") {
            stats.push(format!("{insertions} insertions"));
        }
        if let Some(deletions) = number("deletions") {
            stats.push(format!("{deletions} deletions"));
        }
        if stats.is_empty() {
            format!("Rewound tracked files to message {target_user_message_id}.")
        } else {
            format!(
                "Rewound tracked files to message {target_user_message_id} ({}).",
                stats.join(", ")
            )
        }
    }

    /// `ensureFreshQuery()`.
    async fn ensure_fresh_query(self: &Rc<Self>) -> Result<Rc<dyn ClaudeQuery>, AgentError> {
        if self.state.borrow().query.is_some() {
            self.state.borrow_mut().query_restart_needed = true;
        }
        self.ensure_query().await
    }

    /// `rewindFilesOnce(messageId)`.
    async fn rewind_files_once(self: &Rc<Self>, message_id: &str) -> Result<JsValue, AgentError> {
        let outcome = async {
            let query = self.ensure_fresh_query().await?;
            query.rewind_files(message_id, false).await
        }
        .await;
        if outcome.is_err() {
            // The transport can close after a rewind call: mark the query
            // stale so a follow-up attempt uses a fresh one.
            self.state.borrow_mut().query_restart_needed = true;
        }
        outcome
    }

    /// `getRewindCandidateUserMessageIds()`.
    fn rewind_candidate_user_message_ids(&self) -> Vec<String> {
        let state = self.state.borrow();
        let mut candidates: Vec<String> = Vec::new();
        let mut push_unique = |value: Option<&str>| {
            if let Some(value) = value.filter(|value| !value.is_empty())
                && !candidates.iter().any(|existing| existing == value)
            {
                candidates.push(value.to_owned());
            }
        };
        for (item, _) in state.persisted_history.iter().rev() {
            if str_of(item, "type") == Some("user_message") {
                push_unique(str_of(item, "messageId"));
            }
        }
        for id in state.user_message_ids.iter().rev() {
            push_unique(Some(id));
        }
        candidates
    }

    /// `attemptRewind(args)`.
    async fn attempt_rewind(self: &Rc<Self>, args: Option<&str>) -> RewindAttempt {
        if let Some(args) = args.filter(|args| !js_trim(args).is_empty()) {
            let candidate = js_trim(args)
                .split(is_js_whitespace)
                .next()
                .unwrap_or_default()
                .to_owned();
            if !is_uuid(&candidate) {
                return RewindAttempt {
                    message_id: None,
                    result: None,
                    error: Some(
                        "Invalid message UUID. Usage: /rewind <user_message_uuid> or /rewind"
                            .to_owned(),
                    ),
                };
            }
            return match self.rewind_files_once(&candidate).await {
                Ok(result) if spocky_contracts::js::truthy(result.get("canRewind")) => {
                    RewindAttempt {
                        message_id: Some(candidate),
                        result: Some(result),
                        error: None,
                    }
                }
                Ok(result) => RewindAttempt {
                    message_id: None,
                    error: Some(str_of(&result, "error").map_or_else(
                        || format!("No file checkpoint found for message {candidate}."),
                        str::to_owned,
                    )),
                    result: None,
                },
                Err(error) => RewindAttempt {
                    message_id: None,
                    result: None,
                    error: Some(error.message),
                },
            };
        }
        let candidates = self.rewind_candidate_user_message_ids();
        if candidates.is_empty() {
            return RewindAttempt {
                message_id: None,
                result: None,
                error: Some(
                    "No prior user message available to rewind. Use /rewind <user_message_uuid>."
                        .to_owned(),
                ),
            };
        }
        let mut last_error: Option<String> = None;
        for candidate in candidates {
            match self.rewind_files_once(&candidate).await {
                Ok(result) => {
                    if spocky_contracts::js::truthy(result.get("canRewind")) {
                        return RewindAttempt {
                            message_id: Some(candidate),
                            result: Some(result),
                            error: None,
                        };
                    }
                    if let Some(error) = str_of(&result, "error").filter(|error| !error.is_empty())
                    {
                        last_error = Some(error.to_owned());
                    }
                }
                Err(error) => last_error = Some(error.message),
            }
        }
        RewindAttempt {
            message_id: None,
            result: None,
            error: Some(last_error.unwrap_or_else(|| {
                "No rewind checkpoints are currently available for this session.".to_owned()
            })),
        }
    }

    /// `executeRewindTurn(turnId, invocation)`.
    pub(crate) async fn execute_rewind_turn(self: &Rc<Self>, invocation: &SlashCommand) {
        let mut started = JsObject::new();
        started.insert("type", text("turn_started"));
        started.insert("provider", text("claude"));
        self.notify_subscribers(JsValue::Object(started));
        let attempt = self.attempt_rewind(invocation.args.as_deref()).await;
        let failed = |error: &str| {
            let mut event = JsObject::new();
            event.insert("type", text("turn_failed"));
            event.insert("provider", text("claude"));
            event.insert("error", text(error));
            JsValue::Object(event)
        };
        let (Some(message_id), Some(result)) =
            (attempt.message_id.as_deref(), attempt.result.as_ref())
        else {
            let error = attempt.error.unwrap_or_else(|| {
                "No prior user message available to rewind. Use /rewind <user_message_uuid>."
                    .to_owned()
            });
            let _ = self.finish_foreground_turn(failed(&error));
            return;
        };
        let mut item = JsObject::new();
        item.insert("type", text("assistant_message"));
        item.insert(
            "text",
            JsValue::String(Self::build_rewind_success_message(message_id, result)),
        );
        let mut timeline = JsObject::new();
        timeline.insert("type", text("timeline"));
        timeline.insert("provider", text("claude"));
        timeline.insert("item", JsValue::Object(item));
        self.notify_subscribers(JsValue::Object(timeline));
        let mut completed = JsObject::new();
        completed.insert("type", text("turn_completed"));
        completed.insert("provider", text("claude"));
        let _ = self.finish_foreground_turn(JsValue::Object(completed));
    }
}
