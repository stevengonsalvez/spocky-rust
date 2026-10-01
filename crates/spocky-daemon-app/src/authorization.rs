//! `SessionAuthorization.allowsInbound` and `allowsOutbound` for the
//! slice's messages (`authorization/index.ts`,
//! `authorization/operation-permissions.ts`).

use serde_json::Value;
use spocky_contracts::session::SessionInbound;
use spocky_contracts::ws::DaemonPermission::{
    self, DaemonRead, HubExecute, WorkspaceManage, WorkspaceRead, WorkspaceWrite,
};

/// `PermissionRequirement`: `None` is the baseline's `null` (always allowed);
/// otherwise any one listed permission suffices. A single-permission string
/// in the baseline is a one-element list here, with the same verdict.
pub type Requirement = Option<&'static [DaemonPermission]>;

/// `requiredPermissionForInbound(message.type)` (`INBOUND_PERMISSION`).
#[must_use]
pub fn inbound_requirement(message: &SessionInbound) -> Requirement {
    match message {
        SessionInbound::Ping(_) => Some(&[DaemonRead]),
        SessionInbound::WorkspaceCreate(_) => Some(&[WorkspaceManage]),
        SessionInbound::CreationSubscribe(_) | SessionInbound::WaitForFinish(_) => {
            Some(&[WorkspaceRead])
        }
        SessionInbound::CreateAgent(_)
        | SessionInbound::AgentCreate(_)
        | SessionInbound::SendAgentMessage(_)
        | SessionInbound::CancelAgent(_) => Some(&[WorkspaceWrite, HubExecute]),
        SessionInbound::FetchWorkspaces(_)
        | SessionInbound::FetchAgents(_)
        | SessionInbound::FetchAgent(_)
        | SessionInbound::FetchAgentTimeline(_)
        | SessionInbound::SetAgentTimelineSubscription(_) => Some(&[WorkspaceRead, HubExecute]),
        SessionInbound::SetSessionEventsSubscription(_) => {
            Some(&[WorkspaceRead, DaemonRead, HubExecute])
        }
        SessionInbound::SubscriptionRelease(_) => None,
        SessionInbound::AgentPermissionResponse(_) => Some(&[WorkspaceWrite]),
    }
}

/// `requiredPermissionForOutbound(message)` (`OUTBOUND_PERMISSION`) for the
/// frames the slice emits. A legacy `status` carrying `agent_created` or
/// `agent_create_failed` needs agent-write access. `None` for a type outside
/// the slice, which the baseline's exhaustive table cannot hold.
#[must_use]
pub fn outbound_requirement(message: &Value) -> Option<Requirement> {
    const AGENT_WRITE: &[DaemonPermission] = &[WorkspaceWrite, HubExecute];
    const AGENT_READ: &[DaemonPermission] = &[WorkspaceRead, HubExecute];
    let kind = message.get("type").and_then(Value::as_str)?;
    Some(match kind {
        "status" => {
            let status = message
                .get("payload")
                .and_then(|payload| payload.get("status"))
                .and_then(Value::as_str);
            if matches!(status, Some("agent_created" | "agent_create_failed")) {
                Some(AGENT_WRITE)
            } else {
                Some(&[DaemonRead])
            }
        }
        "pong" => Some(&[DaemonRead]),
        "rpc_error" | "subscription.release.response" => None,
        "workspace.create.update" | "workspace.create.response" => Some(&[WorkspaceManage]),
        "agent.create.update" | "agent.create.response" | "send_agent_message_response" => {
            Some(AGENT_WRITE)
        }
        "creation.subscribe.response" | "wait_for_finish_response" | "activity_log" => {
            Some(&[WorkspaceRead])
        }
        "fetch_agent_response"
        | "fetch_agents_response"
        | "fetch_workspaces_response"
        | "fetch_agent_timeline_response"
        | "agent.timeline.set_subscription.response"
        | "agent_update"
        | "agent_stream"
        | "workspace_update" => Some(AGENT_READ),
        "session.events.set_subscription.response" => {
            Some(&[WorkspaceRead, DaemonRead, HubExecute])
        }
        _ => return None,
    })
}

/// `SessionAuthorization`: the permission set granted to one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAuthorization {
    permissions: Vec<DaemonPermission>,
}

impl SessionAuthorization {
    /// `new SessionAuthorization(permissions)`; duplicates are harmless, as
    /// the baseline stores a `Set`.
    #[must_use]
    pub fn new(permissions: &[DaemonPermission]) -> Self {
        Self {
            permissions: permissions.to_vec(),
        }
    }

    /// `allowsInbound(message)`.
    #[must_use]
    pub fn allows_inbound(&self, message: &SessionInbound) -> bool {
        self.allows(inbound_requirement(message))
    }

    /// `allowsOutbound(message)`. A frame type outside the slice is refused:
    /// the baseline throws on it, so it is never delivered.
    #[must_use]
    pub fn allows_outbound(&self, message: &Value) -> bool {
        outbound_requirement(message).is_some_and(|requirement| self.allows(requirement))
    }

    /// `allows(requirement)`: `null` passes, otherwise `some` permission held.
    #[must_use]
    pub fn allows(&self, requirement: Requirement) -> bool {
        requirement.is_none_or(|any| any.iter().any(|needed| self.permissions.contains(needed)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use spocky_contracts::frame::parse_frame;
    use spocky_contracts::session::SessionInbound;
    use spocky_contracts::ws::DaemonPermission;

    use super::SessionAuthorization;

    fn inbound(message: &serde_json::Value) -> SessionInbound {
        parse_frame(&message.to_string()).expect("valid slice message")
    }

    #[test]
    fn hub_execute_alone_matches_the_baseline_table() {
        // `authorization/index.test.ts`: hub.execute may operate agents and
        // list workspaces, but not ping, wait, or create workspaces.
        let hub = SessionAuthorization::new(&[DaemonPermission::HubExecute]);
        let allowed = [
            json!({"type": "fetch_agents_request", "requestId": "r"}),
            json!({"type": "fetch_agent_request", "requestId": "r", "agentId": "a"}),
            json!({"type": "fetch_workspaces_request", "requestId": "r"}),
            json!({"type": "send_agent_message_request", "requestId": "r", "agentId": "a", "text": "t"}),
            json!({"type": "agent.timeline.set_subscription.request", "requestId": "r", "agentIds": []}),
            json!({"type": "session.events.set_subscription.request", "requestId": "r", "events": []}),
            json!({"type": "subscription.release.request", "requestId": "r", "subscriptionId": "s"}),
        ];
        for message in &allowed {
            assert!(hub.allows_inbound(&inbound(message)), "{message}");
        }
        let denied = [
            json!({"type": "ping", "requestId": "r"}),
            json!({"type": "wait_for_finish_request", "requestId": "r", "agentId": "a"}),
            json!({"type": "workspace.create.request", "requestId": "r", "idempotencyKey": "k",
                   "source": {"kind": "directory", "path": "/p"}}),
        ];
        for message in &denied {
            assert!(!hub.allows_inbound(&inbound(message)), "{message}");
        }
    }

    #[test]
    fn outbound_table_matches_the_baseline_for_hub_and_read_only_sessions() {
        let hub = SessionAuthorization::new(&[DaemonPermission::HubExecute]);
        for allowed in [
            json!({"type": "rpc_error", "payload": {}}),
            json!({"type": "agent_update"}),
            json!({"type": "agent_stream"}),
            json!({"type": "workspace_update"}),
            json!({"type": "fetch_agents_response"}),
            json!({"type": "agent.create.response"}),
            json!({"type": "status", "payload": {"status": "agent_created"}}),
            json!({"type": "subscription.release.response"}),
        ] {
            assert!(hub.allows_outbound(&allowed), "{allowed}");
        }
        for denied in [
            json!({"type": "activity_log"}),
            json!({"type": "pong"}),
            json!({"type": "wait_for_finish_response"}),
            json!({"type": "workspace.create.response"}),
            json!({"type": "status", "payload": {"status": "server_info"}}),
            json!({"type": "not_a_slice_frame"}),
            json!({"payload": {}}),
        ] {
            assert!(!hub.allows_outbound(&denied), "{denied}");
        }
        let reader = SessionAuthorization::new(&[DaemonPermission::WorkspaceRead]);
        assert!(reader.allows_outbound(&json!({"type": "activity_log"})));
        assert!(!reader.allows_outbound(&json!({"type": "agent.create.update"})));
        assert!(!reader.allows_outbound(
            &json!({"type": "status", "payload": {"status": "agent_create_failed"}})
        ));
    }

    #[test]
    fn null_requirement_passes_with_no_permissions() {
        let none = SessionAuthorization::new(&[]);
        assert!(none.allows_inbound(&inbound(
            &json!({"type": "subscription.release.request", "requestId": "r", "subscriptionId": "s"})
        )));
        assert!(!none.allows_inbound(&inbound(&json!({"type": "ping", "requestId": "r"}))));
    }

    #[test]
    fn owner_permissions_allow_every_slice_message() {
        let owner = SessionAuthorization::new(&DaemonPermission::ALL);
        assert!(owner.allows_inbound(&inbound(&json!({"type": "ping", "requestId": "r"}))));
        assert!(owner.allows_inbound(&inbound(&json!({
            "type": "workspace.create.request", "requestId": "r", "idempotencyKey": "k",
            "source": {"kind": "directory", "path": "/p"}
        }))));
    }
}
