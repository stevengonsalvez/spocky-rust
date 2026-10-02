//! `cancel_agent_request` and `agent_permission_response`, as `session.ts`
//! handles them (`handleCancelAgentRequest`, `handleAgentPermissionResponse`)
//! over `lifecycle-command.ts` `cancelAgentRunCommand` and
//! `permission-response.ts` `respondToAgentPermission`.
//!
//! A permission result that asks for a follow-up turn (`followUpPrompt`)
//! needs `startAgentRun` with `replaceRunning`, which is not ported: that
//! response fails loudly after the permission is answered.

use spocky_contracts::js_value::{self, JsObject, JsValue};
use spocky_contracts::json::js_wire_text;
use spocky_contracts::request::{AgentPermissionResponseRequest, CancelAgentRequest};
use spocky_contracts::text::JsText;
use spocky_session::agent_manager::AgentRunCancellationResult;
use spocky_session::clock::{now_iso, random_uuid};

use crate::request::Emit;
use crate::session::{RequestContext, agent_payload, frame, js_text};

/// `requestAgentRunCancellation` then `cancelAgentRunCommand`'s refusal
/// check.
async fn cancel_agent_run(context: &RequestContext, agent_id: &str) -> Result<(), String> {
    let manager = &context.services.manager;
    if manager.get_agent(agent_id).is_none() {
        return Err(format!("Agent {agent_id} not found"));
    }
    if !manager.has_in_flight_run(agent_id) {
        return Ok(());
    }
    let cancellation = manager
        .cancel_agent_run(agent_id)
        .await
        .map_err(|error| error.message)?;
    if cancellation == AgentRunCancellationResult::Refused {
        return Err(format!(
            "Cannot stop agent {agent_id} because its active run cancellation was not acknowledged"
        ));
    }
    Ok(())
}

/// `activity_log` with `type: "error"`, a session event.
pub(crate) fn activity_error(context: &RequestContext, message: String) {
    let mut payload = JsObject::new();
    payload.insert("id", JsValue::String(random_uuid()));
    payload.insert("timestamp", JsValue::String(now_iso()));
    payload.insert("type", JsValue::String("error".to_owned()));
    payload.insert("content", JsValue::String(message));
    context.events.emit(&frame("activity_log", payload));
}

/// `handleCancelAgentRequest(agentId, requestId)`.
pub(crate) async fn cancel_agent(
    context: &RequestContext,
    request: CancelAgentRequest,
    emit: &Emit,
) {
    let agent_id = request.agent_id.as_str();
    let outcome = cancel_agent_run(context, agent_id).await;
    let Some(request_id) = request
        .request_id
        .as_ref()
        .filter(|id| !id.as_str().is_empty())
    else {
        if let Err(message) = outcome {
            // `handleAgentRunError(agentId, error, context)`.
            activity_error(
                context,
                format!("Failed to cancel running agent on request: {message}"),
            );
        }
        return;
    };
    let agent = match context.services.manager.get_agent(agent_id) {
        Some(agent) => agent_payload(&context.services, &agent)
            .await
            .unwrap_or(JsValue::Null),
        None => JsValue::Null,
    };
    let mut payload = JsObject::new();
    payload.insert("requestId", js_text(request_id));
    payload.insert("agentId", JsValue::String(agent_id.to_owned()));
    payload.insert("agent", agent);
    payload.insert(
        "error",
        outcome.err().map_or(JsValue::Null, JsValue::String),
    );
    emit(frame("cancel_agent_response", payload));
}

/// The response as zod outputs it.
fn response_value(request: &AgentPermissionResponseRequest) -> JsValue {
    let text = serde_json::to_string(&request.response).expect("responses serialize");
    js_value::parse(&js_wire_text(&text)).expect("serde_json writes JSON")
}

/// `handleAgentPermissionResponse(agentId, requestId, response)`.
///
/// # Errors
///
/// The manager's error (unknown agent, unknown or answered request, the
/// session's error), after the `activity_log` the baseline emits.
pub(crate) async fn agent_permission_response(
    context: &RequestContext,
    request: AgentPermissionResponseRequest,
    emit: &Emit,
) -> Result<(), JsText> {
    let agent_id = request.agent_id.as_str();
    let request_id = request.request_id.as_str();
    let response = response_value(&request);
    let answered = context
        .services
        .manager
        .respond_to_permission(agent_id, request_id, response.clone())
        .await
        .map_err(|error| error.message)
        .and_then(|result| {
            if result
                .as_ref()
                .and_then(|result| result.get("followUpPrompt"))
                .is_some_and(|prompt| !matches!(prompt, JsValue::Undefined | JsValue::Null))
            {
                Err("Permission follow-up turns are not ported in spocky-daemon-app yet".to_owned())
            } else {
                Ok(())
            }
        });
    match answered {
        Ok(()) => {
            // COMPAT(ownedSubscriptions): a legacy client consumes the domain
            // resolution event instead.
            if context.modern {
                let mut payload = JsObject::new();
                payload.insert("agentId", JsValue::String(agent_id.to_owned()));
                payload.insert("requestId", JsValue::String(request_id.to_owned()));
                payload.insert("resolution", response);
                emit(frame("agent_permission_resolved", payload));
            }
            Ok(())
        }
        Err(message) => {
            activity_error(
                context,
                format!("Failed to respond to permission: {message}"),
            );
            Err(JsText::new(&message))
        }
    }
}
