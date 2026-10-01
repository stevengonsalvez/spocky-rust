//! `SessionAuthorization.allowsInbound` for the slice's inbound messages
//! (`authorization/index.ts`, `authorization/operation-permissions.ts`).

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
        | SessionInbound::SendAgentMessage(_) => Some(&[WorkspaceWrite, HubExecute]),
        SessionInbound::FetchWorkspaces(_)
        | SessionInbound::FetchAgents(_)
        | SessionInbound::FetchAgent(_)
        | SessionInbound::FetchAgentTimeline(_)
        | SessionInbound::SetAgentTimelineSubscription(_) => Some(&[WorkspaceRead, HubExecute]),
        SessionInbound::SetSessionEventsSubscription(_) => {
            Some(&[WorkspaceRead, DaemonRead, HubExecute])
        }
        SessionInbound::SubscriptionRelease(_) => None,
    }
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
