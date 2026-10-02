//! Session events (`emitSubscribedEvent` in `session.ts`) and the
//! `SessionDelivery` sources they reach.
//!
//! An event goes to every attached socket that does not own its
//! subscriptions and wants it (`wantsEvent`, `legacyWantsEvent`). A modern
//! socket gets an event only through `session.events.set_subscription`, which
//! is not ported, so no modern socket receives one here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use spocky_contracts::js_value::JsValue;
use spocky_daemon::session_api::{SessionSink, SocketId};
use spocky_session::agent_sdk::{AbortController, AbortReason, AbortSignal};

use crate::authorization::SessionAuthorization;

/// `sessionEventCategory(message)` for the event types this daemon emits.
fn session_event_category(message: &Value) -> Option<&str> {
    let kind = message.get("type")?.as_str()?;
    matches!(
        kind,
        "agent_attention_required"
            | "agent_permission_request"
            | "agent_permission_resolved"
            | "activity_log"
    )
    .then_some(kind)
}

/// `legacyWantsEvent(event, capabilities)` for those types.
fn legacy_wants_event(event: &str, capabilities: Option<&Value>) -> bool {
    let explicit = capabilities.and_then(|caps| caps.get("explicit_event_subscriptions"))
        == Some(&Value::Bool(true));
    match event {
        "agent_attention_required" | "agent_permission_request" | "agent_permission_resolved" => {
            !explicit
        }
        _ => true,
    }
}

/// One `SessionDelivery` source.
struct Source {
    modern: bool,
    cancellation: AbortController,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One session's sockets and how events reach them.
pub struct EventDelivery {
    sink: Arc<dyn SessionSink>,
    authorization: Arc<SessionAuthorization>,
    capabilities: Arc<Mutex<Option<Value>>>,
    /// Attached sockets: whether each owns its subscriptions
    /// (`owned_subscriptions`), and its `cancellation`, aborted on detach.
    sources: Mutex<HashMap<SocketId, Source>>,
}

impl EventDelivery {
    #[must_use]
    pub fn new(
        sink: Arc<dyn SessionSink>,
        authorization: Arc<SessionAuthorization>,
        capabilities: Arc<Mutex<Option<Value>>>,
    ) -> Self {
        Self {
            sink,
            authorization,
            capabilities,
            sources: Mutex::new(HashMap::new()),
        }
    }

    /// `delivery.attach(source, modern)`: the first hello decides.
    pub fn attach(&self, source: SocketId, capabilities: Option<&Value>) {
        lock(&self.sources).entry(source).or_insert_with(|| Source {
            modern: capabilities.and_then(|caps| caps.get("owned_subscriptions"))
                == Some(&Value::Bool(true)),
            cancellation: AbortController::default(),
        });
    }

    /// `delivery.detach(source)`: the source's requests see their signal
    /// abort.
    pub fn detach(&self, source: SocketId) {
        if let Some(detached) = lock(&self.sources).remove(&source) {
            detached
                .cancellation
                .abort(AbortReason::Value(JsValue::Undefined));
        }
    }

    /// `delivery.requestSignal` for a request from `source`; a socket never
    /// attached is attached as legacy, as `delivery.request` does.
    #[must_use]
    pub fn request_signal(&self, source: SocketId) -> AbortSignal {
        lock(&self.sources)
            .entry(source)
            .or_insert_with(|| Source {
                modern: false,
                cancellation: AbortController::default(),
            })
            .cancellation
            .signal()
    }

    /// `delivery.isModern(source)`; a socket never attached is legacy, as
    /// `delivery.request` attaches it.
    #[must_use]
    pub fn is_modern(&self, source: SocketId) -> bool {
        lock(&self.sources)
            .get(&source)
            .is_some_and(|source| source.modern)
    }

    /// `emitSubscribedEvent(message)`: `false` when the message is not a
    /// session event.
    pub fn emit(&self, message: &Value) -> bool {
        let Some(event) = session_event_category(message) else {
            return false;
        };
        if !self.authorization.allows_outbound(message) {
            return true;
        }
        let wants = legacy_wants_event(event, lock(&self.capabilities).as_ref());
        let legacy: Vec<SocketId> = lock(&self.sources)
            .iter()
            .filter(|(_, source)| !source.modern)
            .map(|(source, _)| *source)
            .collect();
        if wants {
            for source in legacy {
                self.sink.send_to_source(source, message);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use spocky_contracts::ws::DaemonPermission;
    use spocky_daemon::session_api::{SessionSink, SocketId};

    use super::EventDelivery;
    use crate::authorization::SessionAuthorization;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<SocketId>>);

    impl SessionSink for Recorder {
        fn send_to_connection(&self, _message: &Value) {}
        fn send_to_source(&self, source: SocketId, _message: &Value) {
            self.0.lock().unwrap().push(source);
        }
        fn buffered_amount(&self, _source: Option<SocketId>) -> Option<usize> {
            None
        }
    }

    fn delivery(sink: &Arc<Recorder>, capabilities: Value) -> EventDelivery {
        EventDelivery::new(
            Arc::clone(sink) as Arc<dyn SessionSink>,
            Arc::new(SessionAuthorization::new(&DaemonPermission::ALL)),
            Arc::new(Mutex::new(Some(capabilities))),
        )
    }

    #[test]
    fn events_reach_legacy_sockets_only() {
        let sink = Arc::new(Recorder::default());
        let events = delivery(&sink, json!({}));
        events.attach(1, Some(&json!({"owned_subscriptions": true})));
        events.attach(2, Some(&json!({})));
        assert!(events.emit(&json!({"type": "activity_log", "payload": {}})));
        assert!(!events.emit(&json!({"type": "agent_update", "payload": {}})));
        assert_eq!(*sink.0.lock().unwrap(), [2]);
    }

    #[test]
    fn detaching_a_socket_aborts_its_requests() {
        let sink = Arc::new(Recorder::default());
        let events = delivery(&sink, json!({}));
        events.attach(4, Some(&json!({})));
        let signal = events.request_signal(4);
        let other = events.request_signal(5);
        assert!(!signal.aborted());
        events.detach(4);
        assert!(signal.aborted());
        assert!(!other.aborted());
    }

    #[test]
    fn explicit_subscribers_skip_implicit_permission_events() {
        let sink = Arc::new(Recorder::default());
        let events = delivery(&sink, json!({"explicit_event_subscriptions": true}));
        events.attach(2, Some(&json!({})));
        assert!(events.emit(&json!({"type": "agent_permission_request", "payload": {}})));
        assert!(sink.0.lock().unwrap().is_empty());
        assert!(events.emit(&json!({"type": "activity_log", "payload": {}})));
        assert_eq!(*sink.0.lock().unwrap(), [2]);
    }
}
