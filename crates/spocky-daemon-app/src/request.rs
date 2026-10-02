//! `Session.handleRequest` and the `ping` arm of `dispatchInboundMessage`
//! (`session.ts`): the authorization gate, the `rpc_error` a failing handler
//! produces, and the `activity_log` that follows it.

use std::any::Any;
use std::future::Future;
use std::sync::Arc;
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
        SessionInbound::AgentPermissionResponse(_) => "agent_permission_response",
        SessionInbound::CancelAgent(_) => "cancel_agent_request",
    }
}

/// `sessionRequestId(msg)`: the string `requestId`, or `None` for the
/// baseline's `null` when the message has none (`cancel_agent_request`
/// makes it optional).
#[must_use]
pub fn request_id(message: &SessionInbound) -> Option<&JsText> {
    Some(match message {
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
        SessionInbound::AgentPermissionResponse(m) => &m.request_id,
        SessionInbound::CancelAgent(m) => return m.request_id.as_ref(),
    })
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

/// Where a session's outbound frames go: `this.emit`, callable from any
/// task.
pub type Emit = Arc<dyn Fn(Value) + Send + Sync>;

/// `handleRequest(msg)`: refuses a message the session may not send with
/// `access_denied`, otherwise runs `dispatch`. A dispatch error becomes
/// `rpc_error` with `Request failed: <message>` and `handler_error`, then an
/// `activity_log` error entry, in that order. The handler is asynchronous,
/// as `dispatchInboundMessage` is awaited. The error is JavaScript text,
/// since handler messages often quote request fields.
///
/// Every frame, including those `dispatch` emits, passes the session's
/// `allowsOutbound` check first, as `SessionDelivery`'s send does. Replies go
/// to `sink`; the failure's `activity_log` is a session event and goes to
/// `events` (`emitSubscribedEvent`).
pub async fn handle_request<F, Fut>(
    authorization: Arc<SessionAuthorization>,
    message: SessionInbound,
    sink: Emit,
    events: Emit,
    dispatch: F,
) where
    F: FnOnce(SessionInbound, Emit) -> Fut,
    Fut: Future<Output = Result<(), JsText>> + Send + 'static,
{
    let emit: Emit = {
        let authorization = Arc::clone(&authorization);
        Arc::new(move |frame: Value| {
            if authorization.allows_outbound(&frame) {
                sink(frame);
            }
        })
    };
    let events: Emit = {
        let authorization = Arc::clone(&authorization);
        Arc::new(move |frame: Value| {
            if authorization.allows_outbound(&frame) {
                events(frame);
            }
        })
    };
    let id = request_id(&message).cloned();
    let kind = request_type(&message);
    if !authorization.allows_inbound(&message) {
        // `if (requestId)`: an empty request id gets no frame here, while the
        // handler failure below only checks the type and still answers.
        if let Some(id) = id.filter(|id| !id.as_str().is_empty()) {
            let error = JsText::new(&format!("Session is not authorized for {kind}"));
            emit(rpc_error(id, kind, error, "access_denied"));
        }
        return;
    }
    // A thrown handler error is `handler_error` in the baseline; a Rust
    // handler that panics is reported the same way instead of tearing down
    // the connection. The handler runs as its own task so its panic surfaces
    // as a `JoinError` here.
    let outcome = match tokio::spawn(dispatch(message, Arc::clone(&emit))).await {
        Ok(outcome) => outcome,
        Err(join) => Err(JsText::new(&match join.try_into_panic() {
            Ok(panic) => panic_message(panic.as_ref()),
            Err(_) => "handler cancelled".to_owned(),
        })),
    };
    if let Err(error) = outcome {
        let failure = JsText::from_js(format!("Request failed: {}", error.as_str()));
        // `typeof requestId === "string"`: a message without one gets only
        // the activity log entry.
        if let Some(id) = id {
            emit(rpc_error(id, kind, failure, "handler_error"));
        }
        events(json!({
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

/// The text of a panic payload: the `panic!` message when it is a string.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "handler panicked".to_owned())
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

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{Emit, handle_request, pong};
    use crate::authorization::SessionAuthorization;

    fn inbound(message: &Value) -> SessionInbound {
        parse_frame(&message.to_string()).expect("valid slice message")
    }

    fn collector() -> (Emit, Arc<Mutex<Vec<Value>>>) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&frames);
        (
            Arc::new(move |frame| sink.lock().unwrap().push(frame)),
            frames,
        )
    }

    fn run(
        permissions: &[DaemonPermission],
        message: &Value,
        result: Result<(), JsText>,
    ) -> (Vec<Value>, bool) {
        let (sink, frames) = collector();
        let called = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&called);
        block_on(handle_request(
            Arc::new(SessionAuthorization::new(permissions)),
            inbound(message),
            Arc::clone(&sink),
            sink,
            move |_, _| async move {
                flag.store(true, Ordering::SeqCst);
                result
            },
        ));
        let frames = frames.lock().unwrap().clone();
        (frames, called.load(Ordering::SeqCst))
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
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
    fn empty_request_id_skips_access_denied_but_not_handler_error() {
        let (denied, called) = run(
            &[],
            &json!({"type": "fetch_agents_request", "requestId": ""}),
            Ok(()),
        );
        assert!(!called);
        assert!(denied.is_empty());
        let (failed, called) = run(
            &DaemonPermission::ALL,
            &json!({"type": "fetch_agents_request", "requestId": ""}),
            Err(JsText::new("boom")),
        );
        assert!(called);
        assert_eq!(
            failed[0].to_string(),
            r#"{"type":"rpc_error","payload":{"requestId":"","requestType":"fetch_agents_request","error":"Request failed: boom","code":"handler_error"}}"#
        );
    }

    #[test]
    fn cancel_without_request_id_gets_no_rpc_error() {
        let cancel = json!({"type": "cancel_agent_request", "agentId": "a"});
        let (denied, called) = run(&[DaemonPermission::DaemonRead], &cancel, Ok(()));
        assert!(!called);
        assert!(denied.is_empty());
        let (failed, called) = run(&DaemonPermission::ALL, &cancel, Err(JsText::new("boom")));
        assert!(called);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["type"], "activity_log");
        assert_eq!(failed[0]["payload"]["content"], "Error: boom");
    }

    #[test]
    fn permission_response_and_cancel_use_the_baseline_permissions() {
        let respond = json!({
            "type": "agent_permission_response",
            "agentId": "a",
            "requestId": "p1",
            "response": {"behavior": "deny", "interrupt": true, "message": "no"}
        });
        let cancel = json!({"type": "cancel_agent_request", "agentId": "a", "requestId": "c1"});
        assert!(run(&[DaemonPermission::WorkspaceWrite], &respond, Ok(())).1);
        let (denied, called) = run(&[DaemonPermission::HubExecute], &respond, Ok(()));
        assert!(!called);
        assert_eq!(
            serde_json::to_string(&denied).unwrap(),
            r#"[{"type":"rpc_error","payload":{"requestId":"p1","requestType":"agent_permission_response","error":"Session is not authorized for agent_permission_response","code":"access_denied"}}]"#
        );
        assert!(run(&[DaemonPermission::WorkspaceWrite], &cancel, Ok(())).1);
        assert!(run(&[DaemonPermission::HubExecute], &cancel, Ok(())).1);
        let (denied, called) = run(&[DaemonPermission::WorkspaceRead], &cancel, Ok(()));
        assert!(!called);
        assert_eq!(denied[0]["payload"]["requestType"], "cancel_agent_request");
        assert_eq!(denied[0]["payload"]["code"], "access_denied");
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
    fn frames_the_session_may_not_receive_are_filtered() {
        // A hub-only session: `rpc_error` needs nothing, `activity_log`
        // needs workspace.read, so the failure reaches it without the log.
        let (sink, frames) = collector();
        block_on(handle_request(
            Arc::new(SessionAuthorization::new(&[DaemonPermission::HubExecute])),
            inbound(&json!({"type": "fetch_agent_request", "requestId": "f2", "agentId": "a"})),
            Arc::clone(&sink),
            sink,
            |_, emit| async move {
                emit(json!({"type": "pong", "payload": {}}));
                emit(json!({"type": "fetch_agent_response", "payload": {}}));
                Err(JsText::new("boom"))
            },
        ));
        let emitted = frames.lock().unwrap().clone();
        let kinds: Vec<&str> = emitted
            .iter()
            .map(|frame| frame["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["fetch_agent_response", "rpc_error"]);
    }

    #[test]
    fn a_panicking_handler_becomes_handler_error() {
        let (sink, frames) = collector();
        block_on(handle_request(
            Arc::new(SessionAuthorization::new(&DaemonPermission::ALL)),
            inbound(&json!({"type": "fetch_agents_request", "requestId": "p1"})),
            Arc::clone(&sink),
            sink,
            |_, _| async { panic!("index out of bounds") },
        ));
        let emitted = frames.lock().unwrap().clone();
        assert_eq!(
            emitted[0].to_string(),
            r#"{"type":"rpc_error","payload":{"requestId":"p1","requestType":"fetch_agents_request","error":"Request failed: index out of bounds","code":"handler_error"}}"#
        );
        assert_eq!(
            emitted[1]["payload"]["content"],
            "Error: index out of bounds"
        );
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
