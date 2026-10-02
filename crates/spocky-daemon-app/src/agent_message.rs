//! `send_agent_message_request`, as `session.ts`
//! `handleSendAgentMessageRequest` handles it, over `MessageReceipts` and
//! `agent-prompt.ts` `sendPromptToAgent` and `startAgentRun`.
//!
//! An archived agent (`unarchiveAgentState`), the `steer` active-turn
//! behavior (`steerOrReplaceActiveTurn`), and replacing an in-flight run are
//! not ported; those sends fail loudly. A send whose socket went away still
//! answers, where the baseline returns silently on the aborted request
//! signal.

use std::future::Future;
use std::sync::Arc;

use spocky_contracts::js_value::{self, JsObject, JsValue};
use spocky_contracts::json::js_wire_text;
use spocky_contracts::request::{ActiveTurnBehavior, SendAgentMessageRequest};
use spocky_message_receipts::Delivery;
use spocky_session::agent_identity::resolve_create_agent_titles;
use spocky_session::agent_sdk::{AgentPromptInput, AgentRunOptions};

use crate::agent_control::activity_error;
use crate::agent_create::{build_agent_prompt, start_agent_run, wait_for_run_start};
use crate::request::Emit;
use crate::session::{RequestContext, ensure_agent_loaded, frame, js_text, resolve_agent};

/// The request as zod outputs it.
fn request_object(request: &SendAgentMessageRequest) -> JsValue {
    let text = serde_json::to_string(request).expect("requests serialize");
    js_value::parse(&js_wire_text(&text)).expect("serde_json writes JSON")
}

fn prompt_value(prompt: &AgentPromptInput) -> JsValue {
    match prompt {
        AgentPromptInput::Text(text) => JsValue::String(text.clone()),
        AgentPromptInput::Blocks(blocks) => JsValue::Array(blocks.clone()),
    }
}

/// One message's provider side for `messageReceipts.send`.
struct SendDelivery {
    context: Arc<RequestContext>,
    agent_id: String,
    text: String,
    prompt: AgentPromptInput,
    message_id: Option<String>,
    steer: bool,
}

/// `prepareAgentMessage(agentId, text)`: load the agent, then give an
/// untitled agent with no user message yet the provisional title.
async fn prepare_agent_message(
    context: &RequestContext,
    agent_id: &str,
    text: &str,
) -> Result<(), String> {
    ensure_agent_loaded(context, agent_id).await?;
    let stored = context.services.storage.get(agent_id).await;
    let untouched = stored.as_ref().is_some_and(|record| {
        let empty = |key: &str| {
            matches!(
                record.get(key),
                None | Some(JsValue::Null | JsValue::Undefined)
            ) || record.get(key).and_then(JsValue::as_str) == Some("")
        };
        empty("title") && empty("lastUserMessageAt")
    });
    if untouched
        && let Some(title) = resolve_create_agent_titles(None, Some(text)).provisional_title
    {
        context
            .services
            .manager
            .set_title(agent_id, &title)
            .await
            .map_err(|error| error.message)?;
    }
    Ok(())
}

/// `sendPromptToAgent` with `clearPendingPermissions` and the request's
/// active-turn behavior, then `waitForAgentRunStartWithTimeout` for a
/// started turn.
async fn send_prompt(delivery: &SendDelivery) -> Result<(), String> {
    let context = &delivery.context;
    let agent_id = delivery.agent_id.as_str();
    let archived = context
        .services
        .storage
        .get(agent_id)
        .await
        .is_some_and(|record| {
            !matches!(
                record.get("archivedAt"),
                None | Some(JsValue::Null | JsValue::Undefined)
            )
        });
    if archived {
        return Err("Unarchiving an agent is not ported in spocky-daemon-app yet".to_owned());
    }
    ensure_agent_loaded(context, agent_id).await?;
    if delivery.steer {
        return Err("Steering an active turn is not ported in spocky-daemon-app yet".to_owned());
    }
    let run_options = delivery
        .message_id
        .clone()
        .map(|message_id| AgentRunOptions {
            client_message_id: Some(message_id),
            ..AgentRunOptions::default()
        });
    let manager = &context.services.manager;
    if start_agent_run(
        manager,
        agent_id,
        delivery.prompt.clone(),
        run_options,
        true,
    )
    .await?
    {
        wait_for_run_start(manager, agent_id, Some(context.request_signal.clone())).await?;
    }
    Ok(())
}

impl Delivery for SendDelivery {
    type Error = String;

    fn prepare(&mut self) -> impl Future<Output = Result<(), String>> + Send {
        let context = Arc::clone(&self.context);
        let agent_id = self.agent_id.clone();
        let text = self.text.clone();
        async move { prepare_agent_message(&context, &agent_id, &text).await }
    }

    async fn send(&mut self) -> Result<(), String> {
        send_prompt(self).await
    }
}

fn respond(emit: &Emit, request_id: JsValue, agent_id: &str, error: Option<String>) {
    let mut payload = JsObject::new();
    payload.insert("requestId", request_id);
    payload.insert("agentId", JsValue::String(agent_id.to_owned()));
    payload.insert("accepted", JsValue::Bool(error.is_none()));
    payload.insert("error", error.map_or(JsValue::Null, JsValue::String));
    emit(frame("send_agent_message_response", payload));
}

/// `handleSendAgentMessageRequest(msg)`.
pub(crate) async fn send_agent_message(
    context: &Arc<RequestContext>,
    request: SendAgentMessageRequest,
    emit: &Emit,
) {
    let request_id = js_text(&request.request_id);
    let agent_id = match resolve_agent(context, request.agent_id.as_str()).await {
        Ok(agent_id) => agent_id,
        Err(error) => {
            respond(emit, request_id, request.agent_id.as_str(), Some(error));
            return;
        }
    };
    let wire = request_object(&request);
    let prompt = build_agent_prompt(
        request.text.as_str(),
        wire.get("images"),
        wire.get("attachments"),
    );
    let behavior = wire
        .get("activeTurnBehavior")
        .and_then(JsValue::as_str)
        .unwrap_or("interrupt")
        .to_owned();
    let delivery = SendDelivery {
        context: Arc::clone(context),
        agent_id: agent_id.clone(),
        text: request.text.as_str().to_owned(),
        prompt,
        message_id: request.message_id.as_ref().map(|id| id.as_str().to_owned()),
        steer: request.active_turn_behavior == Some(ActiveTurnBehavior::Steer),
    };
    let outcome = match request.message_id.as_ref() {
        Some(message_id) => {
            let mut receipt = JsObject::new();
            receipt.insert("prompt", prompt_value(&delivery.prompt));
            receipt.insert("activeTurnBehavior", JsValue::String(behavior));
            context
                .services
                .receipts
                .send(
                    &agent_id,
                    message_id.as_str(),
                    &JsValue::Object(receipt),
                    delivery,
                )
                .await
                .map_err(|error| error.to_string())
        }
        None => send_prompt(&delivery).await,
    };
    // The run's state changes reach subscribers before this reply.
    context.services.manager.dispatched().await;
    context.updates.flush().await;
    match outcome {
        Ok(()) => respond(emit, request_id, &agent_id, None),
        // `if (this.delivery.requestSignal.aborted) return;`
        Err(_) if context.request_signal.aborted() => {}
        Err(error) => {
            // `handleAgentRunError(agentId, error, "Failed to send agent message")`.
            activity_error(context, format!("Failed to send agent message: {error}"));
            respond(emit, request_id, &agent_id, Some(error));
        }
    }
}
