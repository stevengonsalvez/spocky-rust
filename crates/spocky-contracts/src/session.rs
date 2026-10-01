//! Session messages carried inside `{ "type": "session", "message": ... }`.
//!
//! `SessionInbound` is the slice subset of `SessionInboundMessageSchema`;
//! `SessionOutbound` is the slice subset of `SessionOutboundMessageSchema`.
//! Inbound variants follow zod output order. Outbound variants follow the
//! construction order in `packages/server/src/server/session.ts`.

use serde::{Deserialize, Serialize, Serializer};

use crate::creation::CreationSnapshot;
use crate::field::optional;
use crate::json::{JsRecord, JsonValue, serialize_passthrough};
use crate::number::Int;
use crate::request::{
    AgentCreateRequest, CreateAgentRequest, CreationSubscribeRequest, FetchAgentRequest,
    FetchAgentTimelineRequest, FetchAgentsRequest, FetchWorkspacesRequest, SendAgentMessageRequest,
    SessionEventsSetSubscriptionRequest, SetAgentTimelineSubscriptionRequest,
    SubscriptionReleaseRequest, WaitForFinishRequest, WorkspaceCreateRequest,
};
use crate::response::{
    AgentCreateResponse, CreationSubscribeResponse, FetchAgentResponse, FetchAgentTimelineResponse,
    FetchAgentsResponse, FetchWorkspacesResponse, SendAgentMessageResponse,
    SessionEventsSetSubscriptionResponse, SetAgentTimelineSubscriptionResponse,
    SubscriptionReleaseResponse, WaitForFinishResponse, WorkspaceCreateResponse,
};
use crate::ws::ServerInfo;

/// Session-level `ping` (`PingMessageSchema`), answered with [`SessionPong`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPing {
    #[serde(rename = "requestId")]
    pub request_id: String,
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
    pub request_id: String,
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
    pub request_id: String,
    #[serde(
        rename = "requestType",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub request_type: Option<String>,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub code: Option<String>,
}

/// `status` payloads. `StatusMessageSchema` types `status` as an open
/// string with `.passthrough()`, so any other status is [`StatusPayload::Other`].
#[derive(Debug, Clone, PartialEq)]
pub enum StatusPayload {
    /// `status: "server_info"`.
    ServerInfo(Box<ServerInfo>),
    /// `status: "error"`, a protocol failure without a `requestId`.
    Error { message: String },
    /// Any other status with its remaining keys.
    Other {
        status: String,
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
            message: &'a str,
        }
        #[derive(Serialize)]
        struct StatusKey<'a> {
            status: &'a str,
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
            Self::Other { status, fields } => {
                serialize_passthrough(&StatusKey { status }, fields, serializer)
            }
        }
    }
}

/// Client-to-daemon session messages in the slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

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
}

#[cfg(test)]
mod tests {
    use super::{RpcError, SessionOutbound, StatusPayload};

    #[test]
    fn rpc_error_and_status_error_keep_construction_order() {
        let error = SessionOutbound::RpcError {
            payload: RpcError {
                request_id: "r".to_owned(),
                request_type: Some("fetch_agent_request".to_owned()),
                error: "Request failed: x".to_owned(),
                code: Some("handler_error".to_owned()),
            },
        };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"type":"rpc_error","payload":{"requestId":"r","requestType":"fetch_agent_request","error":"Request failed: x","code":"handler_error"}}"#
        );
        let status = SessionOutbound::Status {
            payload: StatusPayload::Error {
                message: "bad".to_owned(),
            },
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"type":"status","payload":{"status":"error","message":"bad"}}"#
        );
    }
}
