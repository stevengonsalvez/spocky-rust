//! Permission requests: `canUseTool`, the pending table, and responses.

use std::rc::Rc;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::agent_sdk::AgentError;

use super::{ClaudeSession, PendingPermission, text};
use crate::local::Deferred;
use crate::sdk_query::CanUseToolOptions;
use crate::tool_call_mapper::{MapperParams, map_completed, map_failed, map_running};
use crate::transcript::{
    is_permission_update, normalize_ask_user_question_request_input,
    normalize_ask_user_question_updated_input, question_permission_summary,
    resolve_permission_kind,
};

const STEER_SUPERSEDED_PERMISSION_MESSAGE: &str =
    "The user answered with a message instead of approving. Their message follows.";

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `getClaudeModeLabel(modeId)`.
fn mode_label(mode_id: &str) -> String {
    super::default_modes()
        .iter()
        .find(|mode| str_of(mode, "id") == Some(mode_id))
        .and_then(|mode| str_of(mode, "label").map(str::to_owned))
        .unwrap_or_else(|| mode_id.to_owned())
}

/// `buildClaudePlanPermissionActions(resumeMode)`.
fn plan_permission_actions(resume_mode: Option<&str>) -> JsValue {
    let action = |id: &str, label: &str, behavior: &str, variant: &str, intent: &str| {
        let mut object = JsObject::new();
        object.insert("id", text(id));
        object.insert("label", text(label));
        object.insert("behavior", text(behavior));
        object.insert("variant", text(variant));
        object.insert("intent", text(intent));
        JsValue::Object(object)
    };
    let mut actions = vec![
        action("reject", "Reject", "deny", "danger", "dismiss"),
        action("implement", "Implement", "allow", "primary", "implement"),
    ];
    if resume_mode == Some("bypassPermissions") {
        actions.push(action(
            "implement_resume",
            &format!("Implement with {}", mode_label("bypassPermissions")),
            "allow",
            "secondary",
            "implement_resume",
        ));
    }
    JsValue::Array(actions)
}

fn permission_event(kind: &str, fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", text(kind));
    event.insert("provider", text("claude"));
    for (key, value) in fields {
        event.insert(key, value);
    }
    JsValue::Object(event)
}

fn deny_response(message: &str) -> JsValue {
    let mut response = JsObject::new();
    response.insert("behavior", text("deny"));
    response.insert("message", text(message));
    JsValue::Object(response)
}

impl ClaudeSession {
    /// `getPendingPermissions()`.
    pub fn get_pending_permissions(&self) -> Vec<JsValue> {
        self.state
            .borrow()
            .pending_permissions
            .iter()
            .map(|(_, pending)| pending.request.clone())
            .collect()
    }

    /// `planToolCallId(request)`.
    fn plan_tool_call_id(request: &JsValue) -> String {
        request
            .get("metadata")
            .and_then(|metadata| str_of(metadata, "toolUseId"))
            .or_else(|| str_of(request, "id"))
            .unwrap_or_default()
            .to_owned()
    }

    /// `recordDeniedPermissionTimeline(request, response)`.
    fn record_denied_permission_timeline(
        &self,
        request: &JsValue,
        response: &JsValue,
    ) -> Result<(), AgentError> {
        let kind = str_of(request, "kind").unwrap_or_default();
        let input = request.get("input");
        match kind {
            "tool" => {
                let call_id = request
                    .get("metadata")
                    .and_then(|metadata| str_of(metadata, "toolUseId"))
                    .or_else(|| str_of(request, "id"))
                    .map(str::to_owned);
                let mut error = JsObject::new();
                error.insert(
                    "message",
                    text(str_of(response, "message").unwrap_or("Permission denied")),
                );
                let input = input.cloned().unwrap_or(JsValue::Null);
                let failed = map_failed(
                    &MapperParams {
                        call_id: call_id.as_deref(),
                        name: str_of(request, "name").unwrap_or_default(),
                        input: Some(&input),
                        output: Some(&JsValue::Null),
                        metadata: None,
                    },
                    Some(&JsValue::Object(error)),
                )?;
                if let Some(failed) = failed {
                    self.enqueue_timeline(failed);
                }
            }
            "plan" => {
                let call_id = Self::plan_tool_call_id(request);
                let mut metadata = JsObject::new();
                metadata.insert(
                    "actionId",
                    text(str_of(response, "selectedActionId").unwrap_or("reject")),
                );
                let error = text(str_of(response, "message").unwrap_or("Permission denied"));
                let failed = map_failed(
                    &MapperParams {
                        call_id: Some(&call_id),
                        name: "ExitPlanMode",
                        input,
                        output: None,
                        metadata: Some(&metadata),
                    },
                    Some(&error),
                )?;
                if let Some(failed) = failed {
                    self.enqueue_timeline(failed);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `resolveDeniedPermission(request, response)`: the `PermissionResult`.
    fn resolve_denied_permission(
        &self,
        request: &JsValue,
        response: &JsValue,
    ) -> Result<JsValue, AgentError> {
        self.record_denied_permission_timeline(request, response)?;
        self.push_event(permission_event(
            "permission_resolved",
            vec![
                (
                    "requestId",
                    request.get("id").cloned().unwrap_or(JsValue::Undefined),
                ),
                ("resolution", response.clone()),
            ],
        ));
        let mut result = JsObject::new();
        result.insert("behavior", text("deny"));
        result.insert(
            "message",
            text(str_of(response, "message").unwrap_or("Permission request denied")),
        );
        result.insert(
            "interrupt",
            response
                .get("interrupt")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        Ok(JsValue::Object(result))
    }

    /// `denyPendingPermissionsSupersededBySteer()`.
    pub(crate) fn deny_pending_permissions_superseded_by_steer(&self) -> Result<(), AgentError> {
        loop {
            let pending = {
                let mut state = self.state.borrow_mut();
                if state.pending_permissions.is_empty() {
                    break;
                }
                state.pending_permissions.remove(0).1
            };
            pending.cleanup.settle(());
            let denied = self.resolve_denied_permission(
                &pending.request,
                &deny_response(STEER_SUPERSEDED_PERMISSION_MESSAGE),
            )?;
            pending.resolve.settle(Ok(denied));
        }
        Ok(())
    }

    /// `handlePermissionRequest(toolName, input, options)`: `canUseTool`.
    ///
    /// # Errors
    ///
    /// A mapper throw, or the abort of the request.
    #[allow(clippy::too_many_lines)] // The baseline's canUseTool.
    pub(crate) async fn handle_permission_request(
        self: &Rc<Self>,
        tool_name: String,
        input: JsValue,
        options: CanUseToolOptions,
    ) -> Result<JsValue, AgentError> {
        let request_id = format!("permission-{}", uuid::Uuid::new_v4());
        let kind = resolve_permission_kind(&tool_name, &input);
        let request_input = normalize_ask_user_question_request_input(&tool_name, &input);
        let mut metadata = JsObject::new();
        if let Some(tool_use_id) = options.tool_use_id.as_deref().filter(|id| !id.is_empty()) {
            metadata.insert("toolUseId", text(tool_use_id));
        }
        if tool_name == "ExitPlanMode"
            && let Some(plan) = str_of(&input, "plan")
        {
            metadata.insert("planText", text(plan));
        }
        let detail = if kind == "tool" {
            let call_id = options
                .tool_use_id
                .clone()
                .unwrap_or_else(|| request_id.clone());
            map_running(&MapperParams {
                call_id: Some(&call_id),
                name: &tool_name,
                input: Some(&input),
                output: Some(&JsValue::Null),
                metadata: None,
            })?
            .and_then(|call| call.get("detail").cloned())
        } else {
            None
        };
        let mut request = JsObject::new();
        request.insert("id", text(&request_id));
        request.insert("provider", text("claude"));
        request.insert("name", text(&tool_name));
        request.insert("kind", text(kind));
        let (title, description) = question_permission_summary(&tool_name, &input);
        if let Some(title) = title {
            request.insert("title", JsValue::String(title));
            if let Some(description) = description {
                request.insert("description", JsValue::String(description));
            }
        }
        request.insert("input", request_input);
        request.insert("detail", detail.unwrap_or(JsValue::Undefined));
        request.insert(
            "suggestions",
            match options.suggestions.as_ref().and_then(JsValue::as_array) {
                Some(suggestions) => JsValue::Array(
                    suggestions
                        .iter()
                        .map(|suggestion| {
                            JsValue::Object(spocky_contracts::js::spread(Some(suggestion)))
                        })
                        .collect(),
                ),
                None => JsValue::Undefined,
            },
        );
        let resume_mode = self.state.borrow().plan_resume_mode.clone();
        request.insert(
            "actions",
            if kind == "plan" {
                plan_permission_actions(resume_mode.as_deref())
            } else {
                JsValue::Undefined
            },
        );
        request.insert(
            "metadata",
            if metadata.is_empty() {
                JsValue::Undefined
            } else {
                JsValue::Object(metadata)
            },
        );
        let request = JsValue::Object(request);
        self.push_event(permission_event(
            "permission_requested",
            vec![("request", request.clone())],
        ));
        if !self
            .state
            .borrow()
            .permission_clearing_steer_uuids
            .is_empty()
        {
            return self.resolve_denied_permission(
                &request,
                &deny_response(STEER_SUPERSEDED_PERMISSION_MESSAGE),
            );
        }
        let resolve: Rc<Deferred<Result<JsValue, AgentError>>> = Deferred::new();
        let cleanup: Rc<Deferred<()>> = Deferred::new();
        if options.signal.aborted() {
            self.abort_permission(&request_id);
            return Err(AgentError::new("Permission request aborted"));
        }
        self.state.borrow_mut().pending_permissions.push((
            request_id.clone(),
            PendingPermission {
                request,
                resolve: Rc::clone(&resolve),
                cleanup: Rc::clone(&cleanup),
            },
        ));
        let weak = Rc::downgrade(self);
        let watched = Rc::clone(&resolve);
        let id = request_id;
        let signal = options.signal;
        tokio::task::spawn_local(async move {
            // The listener is gone once the request is settled or cleaned up,
            // whichever branch is ready: those win over a signal that fired
            // meanwhile.
            tokio::select! {
                biased;
                () = cleanup.wait() => {}
                _ = watched.wait() => {}
                () = signal.wait() => {
                    if let Some(session) = weak.upgrade() {
                        session.abort_permission(&id);
                    }
                    watched.settle(Err(AgentError::new("Permission request aborted")));
                }
            }
        });
        resolve.wait().await
    }

    /// The `abortHandler` of a pending request.
    fn abort_permission(&self, request_id: &str) {
        self.state
            .borrow_mut()
            .pending_permissions
            .retain(|(id, _)| id != request_id);
        self.push_event(permission_event(
            "permission_resolved",
            vec![
                ("requestId", text(request_id)),
                ("resolution", deny_response("Permission request canceled")),
            ],
        ));
    }

    /// `normalizePermissionUpdates(updates)`.
    fn normalize_permission_updates(updates: Option<&JsValue>) -> JsValue {
        let Some(updates) = updates
            .and_then(JsValue::as_array)
            .filter(|list| !list.is_empty())
        else {
            return JsValue::Undefined;
        };
        let normalized: Vec<JsValue> = updates
            .iter()
            .filter(|update| is_permission_update(update))
            .cloned()
            .collect();
        if normalized.is_empty() {
            JsValue::Undefined
        } else {
            JsValue::Array(normalized)
        }
    }

    /// `respondToPermission(requestId, response)`.
    ///
    /// # Errors
    ///
    /// An unknown request id, or a throw from `setMode` or the mapper.
    pub async fn respond_to_permission(
        self: &Rc<Self>,
        request_id: &str,
        response: &JsValue,
    ) -> Result<(), AgentError> {
        let pending = {
            let mut state = self.state.borrow_mut();
            let position = state
                .pending_permissions
                .iter()
                .position(|(id, _)| id == request_id);
            position.map(|position| state.pending_permissions.remove(position).1)
        };
        let Some(pending) = pending else {
            return Err(AgentError::new(format!(
                "No pending permission request with id '{request_id}'"
            )));
        };
        pending.cleanup.settle(());
        let request = pending.request.clone();
        if str_of(response, "behavior") == Some("allow") {
            if str_of(&request, "kind") == Some("plan") {
                let selected = str_of(response, "selectedActionId");
                let resume_mode = self.state.borrow().plan_resume_mode.clone();
                let target_mode = if selected == Some("implement_resume")
                    && resume_mode.as_deref() == Some("bypassPermissions")
                {
                    "bypassPermissions"
                } else {
                    "acceptEdits"
                };
                self.set_mode(target_mode).await?;
                let mut output = JsObject::new();
                output.insert("approved", JsValue::Bool(true));
                output.insert("actionId", text(selected.unwrap_or("implement")));
                let output = JsValue::Object(output);
                let call_id = Self::plan_tool_call_id(&request);
                let input = request.get("input").cloned().unwrap_or(JsValue::Null);
                let completed = map_completed(&MapperParams {
                    call_id: Some(&call_id),
                    name: "ExitPlanMode",
                    input: Some(&input),
                    output: Some(&output),
                    metadata: None,
                })?;
                if let Some(completed) = completed {
                    self.enqueue_timeline(completed);
                }
            }
            let updated_input = if str_of(&request, "kind") == Some("question") {
                normalize_ask_user_question_updated_input(
                    response.get("updatedInput"),
                    request.get("input"),
                )
            } else {
                response
                    .get("updatedInput")
                    .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
                    .or_else(|| {
                        request
                            .get("input")
                            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
                    })
                    .cloned()
                    .unwrap_or_else(|| JsValue::Object(JsObject::new()))
            };
            let mut result = JsObject::new();
            result.insert("behavior", text("allow"));
            result.insert("updatedInput", updated_input);
            result.insert(
                "updatedPermissions",
                Self::normalize_permission_updates(response.get("updatedPermissions")),
            );
            pending.resolve.settle(Ok(JsValue::Object(result)));
        } else {
            let denied = self.resolve_denied_permission(&request, response)?;
            pending.resolve.settle(Ok(denied));
            return Ok(());
        }
        self.push_event(permission_event(
            "permission_resolved",
            vec![
                ("requestId", text(request_id)),
                ("resolution", response.clone()),
            ],
        ));
        Ok(())
    }
}
