//! Session messages carried inside `{ "type": "session", "message": ... }`.
//!
//! `SessionInbound` is the slice subset of `SessionInboundMessageSchema`;
//! `SessionOutbound` is the slice subset of `SessionOutboundMessageSchema`.
//! Inbound variants follow zod output order. Outbound variants follow the
//! construction order in `packages/server/src/server/session.ts`.

use serde::{Deserialize, Serialize, Serializer};

use crate::creation::CreationSnapshot;
use crate::field::optional;
use crate::json::{JsRecord, JsonValue, deserialize_tagged, serialize_passthrough};
use crate::number::Int;
use crate::request::{
    AgentCreateRequest, AgentPermissionResponseRequest, CancelAgentRequest, CreateAgentRequest,
    CreationSubscribeRequest, FetchAgentRequest, FetchAgentTimelineRequest, FetchAgentsRequest,
    FetchWorkspacesRequest, RefreshAgentRequest, ResumeAgentRequest, SendAgentMessageRequest,
    SessionEventsSetSubscriptionRequest, SetAgentTimelineSubscriptionRequest,
    SubscriptionReleaseRequest, WaitForFinishRequest, WorkspaceCreateRequest,
};
use crate::response::{
    AgentCreateResponse, AgentPermissionRequestEvent, AgentPermissionResolved,
    AgentRefreshedStatus, AgentResumedStatus, CancelAgentResponse, CreationSubscribeResponse,
    FetchAgentResponse, FetchAgentTimelineResponse, FetchAgentsResponse, FetchWorkspacesResponse,
    SendAgentMessageResponse, SessionEventsSetSubscriptionResponse,
    SetAgentTimelineSubscriptionResponse, SubscriptionReleaseResponse, WaitForFinishResponse,
    WorkspaceCreateResponse,
};
use crate::text::JsText;
use crate::ws::ServerInfo;

/// Session-level `ping` (`PingMessageSchema`), answered with [`SessionPong`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPing {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "clientSentAt",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub client_sent_at: Option<Int>,
}

/// The `pong` payload built in `session.ts` for a session `ping`.
/// `clientSentAt` is copied from the request and dropped when absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionPong {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "clientSentAt",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub client_sent_at: Option<Int>,
    #[serde(rename = "serverReceivedAt")]
    pub server_received_at: Int,
    #[serde(rename = "serverSentAt")]
    pub server_sent_at: Int,
}

/// `rpc_error.payload`. Handler failures (`session.ts`) and protocol
/// failures (`owned-subscriptions/index.ts`) both build
/// `requestId, requestType?, error, code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RpcError {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "requestType",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub request_type: Option<JsText>,
    pub error: JsText,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub code: Option<JsText>,
}

/// `status` payloads. `StatusMessageSchema` types `status` as an open
/// string with `.passthrough()`, so any other status is [`StatusPayload::Other`].
#[derive(Debug, Clone, PartialEq)]
pub enum StatusPayload {
    /// `status: "server_info"`.
    ServerInfo(Box<ServerInfo>),
    /// `status: "error"`, a protocol failure without a `requestId`.
    Error { message: JsText },
    /// `status: "agent_resumed"`.
    AgentResumed(Box<AgentResumedStatus>),
    /// `status: "agent_refreshed"`.
    AgentRefreshed(AgentRefreshedStatus),
    /// Any other status with its remaining keys; a `status` entry in
    /// `fields` is ignored.
    Other {
        status: JsText,
        fields: JsRecord<JsonValue>,
    },
}

impl Serialize for StatusPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Tagged<'a, T> {
            status: &'a str,
            #[serde(flatten)]
            rest: &'a T,
        }
        #[derive(Serialize)]
        struct ErrorFields<'a> {
            message: &'a JsText,
        }
        #[derive(Serialize)]
        struct StatusKey<'a> {
            status: &'a JsText,
        }
        match self {
            Self::ServerInfo(info) => Tagged {
                status: "server_info",
                rest: info.as_ref(),
            }
            .serialize(serializer),
            Self::Error { message } => Tagged {
                status: "error",
                rest: &ErrorFields { message },
            }
            .serialize(serializer),
            Self::AgentResumed(resumed) => Tagged {
                status: "agent_resumed",
                rest: resumed.as_ref(),
            }
            .serialize(serializer),
            Self::AgentRefreshed(refreshed) => Tagged {
                status: "agent_refreshed",
                rest: refreshed,
            }
            .serialize(serializer),
            Self::Other { status, fields } => {
                // `status` is written once, from the tag; a `status` entry in
                // `fields` would duplicate the key.
                let rest: JsRecord<JsonValue> = fields
                    .iter()
                    .filter(|(key, _)| key.as_str() != "status")
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                serialize_passthrough(&StatusKey { status }, &rest, serializer)
            }
        }
    }
}

/// Client-to-daemon session messages in the slice.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum SessionInbound {
    #[serde(rename = "ping")]
    Ping(SessionPing),
    #[serde(rename = "workspace.create.request")]
    WorkspaceCreate(Box<WorkspaceCreateRequest>),
    #[serde(rename = "fetch_workspaces_request")]
    FetchWorkspaces(FetchWorkspacesRequest),
    #[serde(rename = "create_agent_request")]
    CreateAgent(Box<CreateAgentRequest>),
    #[serde(rename = "agent.create.request")]
    AgentCreate(Box<AgentCreateRequest>),
    #[serde(rename = "creation.subscribe.request")]
    CreationSubscribe(CreationSubscribeRequest),
    #[serde(rename = "send_agent_message_request")]
    SendAgentMessage(SendAgentMessageRequest),
    #[serde(rename = "wait_for_finish_request")]
    WaitForFinish(WaitForFinishRequest),
    #[serde(rename = "fetch_agents_request")]
    FetchAgents(FetchAgentsRequest),
    #[serde(rename = "fetch_agent_request")]
    FetchAgent(FetchAgentRequest),
    #[serde(rename = "fetch_agent_timeline_request")]
    FetchAgentTimeline(FetchAgentTimelineRequest),
    #[serde(rename = "agent.timeline.set_subscription.request")]
    SetAgentTimelineSubscription(SetAgentTimelineSubscriptionRequest),
    #[serde(rename = "session.events.set_subscription.request")]
    SetSessionEventsSubscription(SessionEventsSetSubscriptionRequest),
    #[serde(rename = "subscription.release.request")]
    SubscriptionRelease(SubscriptionReleaseRequest),
    #[serde(rename = "agent_permission_response")]
    AgentPermissionResponse(AgentPermissionResponseRequest),
    #[serde(rename = "cancel_agent_request")]
    CancelAgent(CancelAgentRequest),
    #[serde(rename = "resume_agent_request")]
    ResumeAgent(Box<ResumeAgentRequest>),
    #[serde(rename = "refresh_agent_request")]
    RefreshAgent(RefreshAgentRequest),
}

deserialize_tagged!(SessionInbound, "type", {
    "ping" => |input| SessionPing::deserialize(input).map(SessionInbound::Ping),
    "workspace.create.request" => |input| {
        WorkspaceCreateRequest::deserialize(input).map(|r| SessionInbound::WorkspaceCreate(Box::new(r)))
    },
    "fetch_workspaces_request" => |input| {
        FetchWorkspacesRequest::deserialize(input).map(SessionInbound::FetchWorkspaces)
    },
    "create_agent_request" => |input| {
        CreateAgentRequest::deserialize(input).map(|r| SessionInbound::CreateAgent(Box::new(r)))
    },
    "agent.create.request" => |input| {
        AgentCreateRequest::deserialize(input).map(|r| SessionInbound::AgentCreate(Box::new(r)))
    },
    "creation.subscribe.request" => |input| {
        CreationSubscribeRequest::deserialize(input).map(SessionInbound::CreationSubscribe)
    },
    "send_agent_message_request" => |input| {
        SendAgentMessageRequest::deserialize(input).map(SessionInbound::SendAgentMessage)
    },
    "wait_for_finish_request" => |input| {
        WaitForFinishRequest::deserialize(input).map(SessionInbound::WaitForFinish)
    },
    "fetch_agents_request" => |input| {
        FetchAgentsRequest::deserialize(input).map(SessionInbound::FetchAgents)
    },
    "fetch_agent_request" => |input| {
        FetchAgentRequest::deserialize(input).map(SessionInbound::FetchAgent)
    },
    "fetch_agent_timeline_request" => |input| {
        FetchAgentTimelineRequest::deserialize(input).map(SessionInbound::FetchAgentTimeline)
    },
    "agent.timeline.set_subscription.request" => |input| {
        SetAgentTimelineSubscriptionRequest::deserialize(input)
            .map(SessionInbound::SetAgentTimelineSubscription)
    },
    "session.events.set_subscription.request" => |input| {
        SessionEventsSetSubscriptionRequest::deserialize(input)
            .map(SessionInbound::SetSessionEventsSubscription)
    },
    "subscription.release.request" => |input| {
        SubscriptionReleaseRequest::deserialize(input).map(SessionInbound::SubscriptionRelease)
    },
    "agent_permission_response" => |input| {
        AgentPermissionResponseRequest::deserialize(input)
            .map(SessionInbound::AgentPermissionResponse)
    },
    "cancel_agent_request" => |input| {
        CancelAgentRequest::deserialize(input).map(SessionInbound::CancelAgent)
    },
    "resume_agent_request" => |input| {
        ResumeAgentRequest::deserialize(input).map(|r| SessionInbound::ResumeAgent(Box::new(r)))
    },
    "refresh_agent_request" => |input| {
        RefreshAgentRequest::deserialize(input).map(SessionInbound::RefreshAgent)
    },
});

/// Daemon-to-client session messages in the slice.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum SessionOutbound {
    #[serde(rename = "status")]
    Status { payload: StatusPayload },
    #[serde(rename = "pong")]
    Pong { payload: SessionPong },
    #[serde(rename = "rpc_error")]
    RpcError { payload: RpcError },
    #[serde(rename = "workspace.create.update")]
    WorkspaceCreateUpdate { payload: Box<CreationSnapshot> },
    #[serde(rename = "agent.create.update")]
    AgentCreateUpdate { payload: Box<CreationSnapshot> },
    #[serde(rename = "workspace.create.response")]
    WorkspaceCreateResponse {
        payload: Box<WorkspaceCreateResponse>,
    },
    #[serde(rename = "agent.create.response")]
    AgentCreateResponse { payload: Box<AgentCreateResponse> },
    #[serde(rename = "creation.subscribe.response")]
    CreationSubscribeResponse {
        payload: Box<CreationSubscribeResponse>,
    },
    #[serde(rename = "fetch_agent_response")]
    FetchAgentResponse { payload: Box<FetchAgentResponse> },
    #[serde(rename = "fetch_agents_response")]
    FetchAgentsResponse { payload: FetchAgentsResponse },
    #[serde(rename = "fetch_workspaces_response")]
    FetchWorkspacesResponse { payload: FetchWorkspacesResponse },
    #[serde(rename = "fetch_agent_timeline_response")]
    FetchAgentTimelineResponse {
        payload: Box<FetchAgentTimelineResponse>,
    },
    #[serde(rename = "wait_for_finish_response")]
    WaitForFinishResponse { payload: Box<WaitForFinishResponse> },
    #[serde(rename = "send_agent_message_response")]
    SendAgentMessageResponse { payload: SendAgentMessageResponse },
    #[serde(rename = "session.events.set_subscription.response")]
    SessionEventsSetSubscriptionResponse {
        payload: SessionEventsSetSubscriptionResponse,
    },
    #[serde(rename = "agent.timeline.set_subscription.response")]
    SetAgentTimelineSubscriptionResponse {
        payload: SetAgentTimelineSubscriptionResponse,
    },
    #[serde(rename = "subscription.release.response")]
    SubscriptionReleaseResponse {
        payload: SubscriptionReleaseResponse,
    },
    #[serde(rename = "cancel_agent_response")]
    CancelAgentResponse { payload: Box<CancelAgentResponse> },
    #[serde(rename = "agent_permission_request")]
    AgentPermissionRequest {
        payload: Box<AgentPermissionRequestEvent>,
    },
    #[serde(rename = "agent_permission_resolved")]
    AgentPermissionResolved {
        payload: Box<AgentPermissionResolved>,
    },
}

#[cfg(test)]
mod tests {
    use super::{RpcError, SessionOutbound, StatusPayload};
    use crate::json::{JsRecord, JsonValue};

    #[test]
    fn other_status_writes_status_once() {
        let fields: JsRecord<JsonValue> =
            serde_json::from_str(r#"{"status":"x","b":1,"2":true}"#).unwrap();
        let status = SessionOutbound::Status {
            payload: StatusPayload::Other {
                status: "agent_refreshed".into(),
                fields,
            },
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"type":"status","payload":{"2":true,"status":"agent_refreshed","b":1}}"#
        );
    }

    #[test]
    fn rpc_error_and_status_error_keep_construction_order() {
        let error = SessionOutbound::RpcError {
            payload: RpcError {
                request_id: "r".into(),
                request_type: Some("fetch_agent_request".into()),
                error: "Request failed: x".into(),
                code: Some("handler_error".into()),
            },
        };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"type":"rpc_error","payload":{"requestId":"r","requestType":"fetch_agent_request","error":"Request failed: x","code":"handler_error"}}"#
        );
        let status = SessionOutbound::Status {
            payload: StatusPayload::Error {
                message: "bad".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"type":"status","payload":{"status":"error","message":"bad"}}"#
        );
    }
}
