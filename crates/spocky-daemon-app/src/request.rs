//! `Session.handleRequest` and the `ping` arm of `dispatchInboundMessage`
//! (`session.ts`): the authorization gate, the `rpc_error` a failing handler
//! produces, and the `activity_log` that follows it.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_contracts::number::Int;
use spocky_contracts::session::{
    RpcError, SessionInbound, SessionOutbound, SessionPing, SessionPong,
};
use spocky_contracts::text::JsText;
use spocky_session::clock::{now_iso, random_uuid};

use crate::authorization::SessionAuthorization;

/// `msg.type` for a slice message.
#[must_use]
pub fn request_type(message: &SessionInbound) -> &'static str {
    match message {
        SessionInbound::Ping(_) => "ping",
        SessionInbound::WorkspaceCreate(_) => "workspace.create.request",
        SessionInbound::FetchWorkspaces(_) => "fetch_workspaces_request",
        SessionInbound::CreateAgent(_) => "create_agent_request",
        SessionInbound::AgentCreate(_) => "agent.create.request",
        SessionInbound::CreationSubscribe(_) => "creation.subscribe.request",
        SessionInbound::SendAgentMessage(_) => "send_agent_message_request",
        SessionInbound::WaitForFinish(_) => "wait_for_finish_request",
        SessionInbound::FetchAgents(_) => "fetch_agents_request",
        SessionInbound::FetchAgent(_) => "fetch_agent_request",
        SessionInbound::FetchAgentTimeline(_) => "fetch_agent_timeline_request",
        SessionInbound::SetAgentTimelineSubscription(_) => {
            "agent.timeline.set_subscription.request"
        }
        SessionInbound::SetSessionEventsSubscription(_) => {
            "session.events.set_subscription.request"
        }
        SessionInbound::SubscriptionRelease(_) => "subscription.release.request",
    }
}

/// `sessionRequestId(msg)`: every slice message carries a string `requestId`.
#[must_use]
pub fn request_id(message: &SessionInbound) -> &JsText {
    match message {
        SessionInbound::Ping(m) => &m.request_id,
        SessionInbound::WorkspaceCreate(m) => &m.request_id,
        SessionInbound::FetchWorkspaces(m) => &m.request_id,
        SessionInbound::CreateAgent(m) => &m.request_id,
        SessionInbound::AgentCreate(m) => &m.request_id,
        SessionInbound::CreationSubscribe(m) => &m.request_id,
        SessionInbound::SendAgentMessage(m) => &m.request_id,
        SessionInbound::WaitForFinish(m) => &m.request_id,
        SessionInbound::FetchAgents(m) => &m.request_id,
        SessionInbound::FetchAgent(m) => &m.request_id,
        SessionInbound::FetchAgentTimeline(m) => &m.request_id,
        SessionInbound::SetAgentTimelineSubscription(m) => &m.request_id,
        SessionInbound::SetSessionEventsSubscription(m) => &m.request_id,
        SessionInbound::SubscriptionRelease(m) => &m.request_id,
    }
}

/// Serializes an outbound session message with its construction key order.
/// Strings stay in the `js_value` encoding of [`JsText`]; the transport
/// writes the frame through `js_wire_text`, as `frame_text` does.
///
/// # Panics
///
/// Never in practice: contract types serialize to JSON without failure.
#[must_use]
pub fn outbound(message: &SessionOutbound) -> Value {
    serde_json::to_value(message).expect("contract messages serialize to JSON")
}

fn rpc_error(request_id: JsText, request_type: &str, error: JsText, code: &str) -> Value {
    outbound(&SessionOutbound::RpcError {
        payload: RpcError {
            request_id,
            request_type: Some(JsText::new(request_type)),
            error,
            code: Some(JsText::new(code)),
        },
    })
}

/// `handleRequest(msg)`: refuses a message the session may not send with
/// `access_denied`, otherwise runs `dispatch`. A dispatch error becomes
/// `rpc_error` with `Request failed: <message>` and `handler_error`, then an
/// `activity_log` error entry, in that order. The error is JavaScript text,
/// since handler messages often quote request fields.
pub fn handle_request(
    authorization: &SessionAuthorization,
    message: SessionInbound,
    emit: &mut dyn FnMut(Value),
    dispatch: impl FnOnce(SessionInbound, &mut dyn FnMut(Value)) -> Result<(), JsText>,
) {
    let id = request_id(&message).clone();
    let kind = request_type(&message);
    if !authorization.allows_inbound(&message) {
        let error = JsText::new(&format!("Session is not authorized for {kind}"));
        emit(rpc_error(id, kind, error, "access_denied"));
        return;
    }
    if let Err(error) = dispatch(message, emit) {
        let failure = JsText::from_js(format!("Request failed: {}", error.as_str()));
        emit(rpc_error(id, kind, failure, "handler_error"));
        emit(json!({
            "type": "activity_log",
            "payload": {
                "id": random_uuid(),
                "timestamp": now_iso(),
                "type": "error",
                "content": format!("Error: {}", error.as_str()),
            }
        }));
    }
}

/// `Date.now()`.
///
/// # Panics
///
/// When the system clock is before 1970 or past JavaScript's safe range.
#[must_use]
pub fn now_millis() -> Int {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after 1970")
        .as_millis();
    Int::new(i64::try_from(millis).expect("millis fit i64")).expect("millis are a safe integer")
}

/// The `ping` arm: `pong` with the request's `clientSentAt` (dropped when
/// absent) and one `Date.now()` for both server times.
#[must_use]
pub fn pong(ping: SessionPing, now: Int) -> Value {
    outbound(&SessionOutbound::Pong {
        payload: SessionPong {
            request_id: ping.request_id,
            client_sent_at: ping.client_sent_at,
            server_received_at: now,
            server_sent_at: now,
        },
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use spocky_contracts::frame::parse_frame;
    use spocky_contracts::number::Int;
    use spocky_contracts::session::SessionInbound;
    use spocky_contracts::text::JsText;
    use spocky_contracts::ws::DaemonPermission;

    use super::{handle_request, pong};
    use crate::authorization::SessionAuthorization;

    fn inbound(message: &Value) -> SessionInbound {
        parse_frame(&message.to_string()).expect("valid slice message")
    }

    fn run(
        permissions: &[DaemonPermission],
        message: &Value,
        result: Result<(), JsText>,
    ) -> (Vec<Value>, bool) {
        let mut emitted = Vec::new();
        let mut called = false;
        handle_request(
            &SessionAuthorization::new(permissions),
            inbound(message),
            &mut |value| emitted.push(value),
            |_, _| {
                called = true;
                result
            },
        );
        (emitted, called)
    }

    #[test]
    fn unauthorized_request_gets_access_denied_and_is_not_dispatched() {
        let (emitted, called) = run(
            &[DaemonPermission::HubExecute],
            &json!({"type": "wait_for_finish_request", "requestId": "w1", "agentId": "a"}),
            Ok(()),
        );
        assert!(!called);
        assert_eq!(
            serde_json::to_string(&emitted).unwrap(),
            r#"[{"type":"rpc_error","payload":{"requestId":"w1","requestType":"wait_for_finish_request","error":"Session is not authorized for wait_for_finish_request","code":"access_denied"}}]"#
        );
    }

    #[test]
    fn handler_failure_emits_rpc_error_then_activity_log() {
        let (emitted, called) = run(
            &DaemonPermission::ALL,
            &json!({"type": "fetch_agent_request", "requestId": "f1", "agentId": "a"}),
            Err(JsText::new("Agent not found: a")),
        );
        assert!(called);
        assert_eq!(emitted.len(), 2);
        assert_eq!(
            emitted[0].to_string(),
            r#"{"type":"rpc_error","payload":{"requestId":"f1","requestType":"fetch_agent_request","error":"Request failed: Agent not found: a","code":"handler_error"}}"#
        );
        let log = &emitted[1];
        assert_eq!(log["type"], "activity_log");
        let keys: Vec<&str> = log["payload"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["id", "timestamp", "type", "content"]);
        assert_eq!(log["payload"]["type"], "error");
        assert_eq!(log["payload"]["content"], "Error: Agent not found: a");
        let timestamp = log["payload"]["timestamp"].as_str().unwrap();
        assert_eq!(timestamp.len(), "2026-10-01T16:56:11.482Z".len());
        assert!(timestamp.ends_with('Z'));
        assert!(spocky_contracts::id::is_zod_uuid(
            log["payload"]["id"].as_str().unwrap()
        ));
    }

    #[test]
    fn successful_dispatch_emits_nothing_itself() {
        let (emitted, called) = run(
            &DaemonPermission::ALL,
            &json!({"type": "fetch_agents_request", "requestId": "l1"}),
            Ok(()),
        );
        assert!(called);
        assert!(emitted.is_empty());
    }

    #[test]
    fn pong_copies_client_sent_at_only_when_present() {
        let now = Int::new(1_790_873_728_431).unwrap();
        let SessionInbound::Ping(with) =
            inbound(&json!({"type": "ping", "requestId": "p", "clientSentAt": 5}))
        else {
            panic!("ping");
        };
        assert_eq!(
            pong(with, now).to_string(),
            r#"{"type":"pong","payload":{"requestId":"p","clientSentAt":5,"serverReceivedAt":1790873728431,"serverSentAt":1790873728431}}"#
        );
        let SessionInbound::Ping(without) = inbound(&json!({"type": "ping", "requestId": "p"}))
        else {
            panic!("ping");
        };
        assert_eq!(
            pong(without, now).to_string(),
            r#"{"type":"pong","payload":{"requestId":"p","serverReceivedAt":1790873728431,"serverSentAt":1790873728431}}"#
        );
    }
}
