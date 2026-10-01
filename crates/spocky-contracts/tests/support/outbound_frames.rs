//! Typed Rust values for the `out.*` golden cases in
//! `scripts/phase3/contracts-cases.mjs`. Each must write the daemon bytes.

use spocky_contracts::agent::{AgentStatus, AttentionReason};
use spocky_contracts::creation::{CreationPhase, CreationSnapshot, FailedStage};
use spocky_contracts::field::Nullable;
use spocky_contracts::frame::WsOutbound;
use spocky_contracts::js_value::JsValue;
use spocky_contracts::json::{JsRecord, JsonValue};
use spocky_contracts::number::{BoundedInt, Int, JsNumber, NonNegativeInt};
use spocky_contracts::request::{CreationKind, TimelineDirection, TimelineProjection};
use spocky_contracts::response::{
    AgentCreateResponse, AgentDirectoryEntry, FetchAgentResponse, FetchAgentTimelineResponse,
    FetchAgentsResponse, FetchWorkspacesResponse, PageInfo, SendAgentMessageResponse,
    SetAgentTimelineSubscriptionResponse, WaitForFinishResponse, WaitStatus,
    WorkspaceCreateResponse,
};
use spocky_contracts::session::{RpcError, SessionOutbound, SessionPong, StatusPayload};
use spocky_contracts::snapshot::{
    ActiveTurn, AgentFeature, AgentMode, AgentSnapshot, AgentUsage, CapabilityFlags,
    CapabilityFlagsKnown, LiveAgentSnapshot, PersistenceHandle, RuntimeInfo, StoredAgentSnapshot,
};
use spocky_contracts::timeline::{
    AssistantMessageLayout, SeqRange, ShellDetail, TimelineCollapse, TimelineCursor, TimelineEntry,
    TimelineItem, TimelineWindow, ToolCallDetail, ToolCallItem, ToolCallLayout, ToolCallStatus,
    UserMessageLayout,
};
use spocky_contracts::workspace::{
    DiffStat, GitHubRuntime, GitRuntime, PlacementCheckout, ProjectKind, ProjectPlacement,
    WorkspaceDescriptor, WorkspaceKind, WorkspaceStatus,
};

const T0: &str = "2026-10-01T00:00:00.000Z";
const T1: &str = "2026-10-01T00:00:01.000Z";
const AGENT_ID: &str = "123e4567-e89b-42d3-a456-426614174000";
const THREAD: &str = "019a0000-0000-7000-8000-000000000001";
const WKS: &str = "wks_0123456789abcdef";

fn s(text: &str) -> String {
    text.to_owned()
}

fn n(value: f64) -> JsNumber {
    JsNumber::new(value).unwrap()
}

fn seq(value: i64) -> NonNegativeInt {
    BoundedInt::new(value).unwrap()
}

fn session(message: SessionOutbound) -> WsOutbound {
    WsOutbound::Session(Box::new(message))
}

fn codex_capabilities() -> CapabilityFlags {
    CapabilityFlags {
        known: CapabilityFlagsKnown {
            supports_streaming: true,
            supports_session_persistence: true,
            supports_session_listing: Some(true),
            supports_dynamic_modes: false,
            supports_mcp_servers: true,
            supports_reasoning_stream: true,
            supports_tool_invocations: true,
            supports_rewind_conversation: true,
            supports_rewind_files: false,
            supports_rewind_both: false,
        },
        extra: JsRecord::new(),
    }
}

fn codex_modes() -> Vec<AgentMode> {
    let mode = |id: &str, label: &str, description: &str| AgentMode {
        id: s(id),
        label: s(label),
        description: Some(s(description)),
        icon: None,
        color_tier: None,
    };
    vec![
        mode(
            "auto",
            "Default Permissions",
            "Edit files and run commands with Codex's default approval flow.",
        ),
        mode(
            "full-access",
            "Full Access",
            "Edit files, run commands, and access the network without additional prompts.",
        ),
    ]
}

fn persistence_metadata() -> JsRecord<JsonValue> {
    let text = |value: &str| JsonValue(JsValue::String(s(value)));
    let null = || JsonValue(JsValue::Null);
    [
        ("provider", text("codex")),
        ("cwd", text("/tmp/project")),
        ("title", null()),
        ("threadId", text(THREAD)),
        ("modeId", text("full-access")),
        ("model", null()),
        ("thinkingOptionId", null()),
        ("asyncQuestions", JsonValue(JsValue::Array(Vec::new()))),
    ]
    .into_iter()
    .map(|(key, value)| (s(key), value))
    .collect()
}

fn live_agent(idle: bool) -> AgentSnapshot {
    AgentSnapshot::Live(Box::new(LiveAgentSnapshot {
        id: s(AGENT_ID),
        provider: s("codex"),
        cwd: s("/tmp/project"),
        workspace_id: Some(s(WKS)),
        model: None,
        thinking_option_id: None,
        effective_thinking_option_id: None,
        runtime_info: Some(RuntimeInfo {
            provider: s("codex"),
            session_id: Some(s(THREAD)),
            model: Some(Nullable::Null),
            thinking_option_id: Some(Nullable::Null),
            mode_id: Some(Nullable::Value(s("full-access"))),
            extra: None,
        }),
        created_at: s(T0),
        updated_at: s(T1),
        last_user_message_at: Some(s(T0)),
        status: if idle {
            AgentStatus::Idle
        } else {
            AgentStatus::Running
        },
        active_turn: (!idle).then(|| ActiveTurn {
            turn_id: s("turn-1"),
            started_at: Some(s(T0)),
        }),
        capabilities: codex_capabilities(),
        current_mode_id: Some(s("full-access")),
        available_modes: codex_modes(),
        features: vec![AgentFeature::Toggle {
            id: s("plan_mode"),
            label: s("Plan"),
            description: Some(s("Switch Codex into planning-only collaboration mode")),
            tooltip: Some(s("Toggle plan mode")),
            icon: Some(s("list-todo")),
            value: false,
        }],
        pending_permissions: Vec::new(),
        persistence: Some(PersistenceHandle {
            provider: s("codex"),
            session_id: s(THREAD),
            native_handle: Some(s(THREAD)),
            metadata: Some(persistence_metadata()),
        }),
        title: None,
        labels: JsRecord::new(),
        last_usage: idle.then(|| AgentUsage {
            input_tokens: Some(n(1200.0)),
            cached_input_tokens: Some(n(0.0)),
            output_tokens: Some(n(34.0)),
            total_cost_usd: Some(n(0.0123)),
            context_window_max_tokens: None,
            context_window_used_tokens: None,
        }),
        last_error: None,
        requires_attention: idle,
        attention_reason: idle.then_some(AttentionReason::Finished),
        attention_timestamp: idle.then(|| s(T1)),
        archived_at: None,
    }))
}

fn stored_agent() -> AgentSnapshot {
    AgentSnapshot::Stored(Box::new(StoredAgentSnapshot {
        id: s(AGENT_ID),
        provider: s("codex"),
        cwd: s("/tmp/project"),
        workspace_id: Some(s(WKS)),
        model: None,
        thinking_option_id: None,
        effective_thinking_option_id: None,
        runtime_info: None,
        created_at: s(T0),
        updated_at: s(T1),
        last_user_message_at: Some(s(T0)),
        status: AgentStatus::Closed,
        capabilities: CapabilityFlags::stored_default(),
        current_mode_id: Some(s("full-access")),
        available_modes: Vec::new(),
        pending_permissions: Vec::new(),
        persistence: Some(PersistenceHandle {
            provider: s("codex"),
            session_id: s(THREAD),
            native_handle: Some(s(THREAD)),
            metadata: None,
        }),
        title: None,
        requires_attention: false,
        attention_reason: None,
        attention_timestamp: None,
        archived_at: None,
        labels: JsRecord::new(),
        provider_unavailable: None,
    }))
}

fn placement() -> ProjectPlacement {
    ProjectPlacement {
        project_key: s("proj_golden"),
        project_name: s("project"),
        workspace_name: s("main"),
        checkout: PlacementCheckout::Git {
            cwd: s("/tmp/project"),
            current_branch: Some(s("main")),
            worktree_root: s("/tmp/project"),
            is_paseo_owned_worktree: false,
            main_repo_root: None,
        },
    }
}

fn workspace() -> WorkspaceDescriptor {
    WorkspaceDescriptor {
        id: s(WKS),
        project_id: s("proj_golden"),
        project_display_name: s("project"),
        project_custom_name: None,
        project_custom_icon_revision: None,
        project_root_path: s("/tmp/project"),
        workspace_directory: s("/tmp/project"),
        worktree_slug: None,
        project_kind: ProjectKind::Git,
        workspace_kind: WorkspaceKind::LocalCheckout,
        name: s("main"),
        title: None,
        pinned_at: None,
        labels: None,
        archiving_at: None,
        status: WorkspaceStatus::Done,
        status_entered_at: None,
        activity_at: None,
        diff_stat: None,
        scripts: Vec::new(),
        project: Some(placement()),
        git_runtime: None,
        github_runtime: None,
        forge: None,
    }
}

fn creation(kind: CreationKind, revision: i64, phase: CreationPhase) -> CreationSnapshot {
    CreationSnapshot {
        kind,
        idempotency_key: s("8b0c3c8e-1a2b-4c3d-8e9f-000000000002"),
        revision: seq(revision),
        phase,
        error: None,
        workspace_id: Some(s(WKS)),
        agent_id: (kind == CreationKind::Agent).then(|| s(AGENT_ID)),
        workspace: None,
        setup_skipped_reason: None,
        agent: None,
        error_code: None,
        failed_stage: None,
        outcome_unknown: None,
    }
}

fn no_more_pages() -> PageInfo {
    PageInfo {
        next_cursor: None,
        prev_cursor: None,
        has_more: false,
    }
}

fn entry(seq_start: i64, seq_end: i64, timestamp: &str, item: TimelineItem) -> TimelineEntry {
    let collapsed = match &item {
        TimelineItem::ToolCall(_) => vec![TimelineCollapse::ToolLifecycle],
        TimelineItem::AssistantMessage { .. } => vec![TimelineCollapse::AssistantMerge],
        _ => Vec::new(),
    };
    TimelineEntry {
        provider: s("codex"),
        item,
        timestamp: s(timestamp),
        seq_start: seq(seq_start),
        seq_end: seq(seq_end),
        source_seq_ranges: vec![SeqRange {
            start_seq: seq(seq_start),
            end_seq: seq(seq_end),
        }],
        turn_id: Some(s("turn-1")),
        collapsed,
    }
}

fn timeline_entries() -> Vec<TimelineEntry> {
    vec![
        entry(
            1,
            1,
            T0,
            TimelineItem::UserMessage {
                text: s("Say hello"),
                message_id: Some(s("cm-1")),
                client_message_id: Some(s("cm-1")),
                layout: UserMessageLayout::Submitted,
            },
        ),
        entry(
            2,
            2,
            T0,
            TimelineItem::Reasoning {
                text: s("Thinking"),
            },
        ),
        entry(
            3,
            4,
            T0,
            TimelineItem::ToolCall(Box::new(ToolCallItem {
                call_id: s("call-1"),
                name: s("shell"),
                status: ToolCallStatus::Completed,
                error: JsonValue(JsValue::Null),
                detail: ToolCallDetail::Shell(ShellDetail {
                    command: s("ls"),
                    cwd: Some(s("/tmp/project")),
                    output: Some(s("a\n")),
                    exit_code: Some(Nullable::Value(n(0.0))),
                }),
                metadata: None,
                layout: ToolCallLayout::Mapper,
            })),
        ),
        entry(
            5,
            6,
            T1,
            TimelineItem::AssistantMessage {
                text: s("Hello!"),
                message_id: Some(s("msg-1")),
                layout: AssistantMessageLayout::Schema,
            },
        ),
    ]
}

/// The Rust value for an `out.*` case id.
#[allow(clippy::too_many_lines)]
pub fn frame(id: &str) -> Option<WsOutbound> {
    let message = match id {
        "out.workspace_create.update_accepted" => SessionOutbound::WorkspaceCreateUpdate {
            payload: Box::new(CreationSnapshot {
                workspace_id: Some(s(WKS)),
                ..creation(CreationKind::Workspace, 0, CreationPhase::Accepted)
            }),
        },
        "out.workspace_create.update_ready" => SessionOutbound::WorkspaceCreateUpdate {
            payload: Box::new(CreationSnapshot {
                workspace: Some(Box::new(workspace())),
                ..creation(CreationKind::Workspace, 1, CreationPhase::WorkspaceReady)
            }),
        },
        "out.workspace_create.response" => SessionOutbound::WorkspaceCreateResponse {
            payload: Box::new(WorkspaceCreateResponse {
                request_id: s("r"),
                workspace: Some(Box::new(workspace())),
                agent: None,
                creation: Some(Box::new(CreationSnapshot {
                    workspace: Some(Box::new(workspace())),
                    ..creation(CreationKind::Workspace, 2, CreationPhase::Completed)
                })),
                setup_skipped_reason: None,
                error: None,
                error_code: None,
                setup_terminal_id: None,
            }),
        },
        "out.workspace_create.response_error" => SessionOutbound::WorkspaceCreateResponse {
            payload: Box::new(WorkspaceCreateResponse {
                request_id: s("r"),
                workspace: None,
                agent: None,
                creation: None,
                setup_skipped_reason: None,
                error: Some(s("Directory not found")),
                error_code: Some(s("directory_not_found")),
                setup_terminal_id: None,
            }),
        },
        "out.agent_create.update_ready" => SessionOutbound::AgentCreateUpdate {
            payload: Box::new(CreationSnapshot {
                agent: Some(live_agent(false)),
                ..creation(CreationKind::Agent, 1, CreationPhase::AgentReady)
            }),
        },
        "out.agent_create.update_failed" => SessionOutbound::AgentCreateUpdate {
            payload: Box::new(CreationSnapshot {
                error: Some(s("Codex exited")),
                failed_stage: Some(FailedStage::Agent),
                outcome_unknown: Some(false),
                ..creation(CreationKind::Agent, 1, CreationPhase::Failed)
            }),
        },
        "out.agent_create.response" => SessionOutbound::AgentCreateResponse {
            payload: Box::new(AgentCreateResponse {
                request_id: s("r"),
                agent: Some(live_agent(false)),
                error: None,
                creation: Some(Box::new(CreationSnapshot {
                    agent: Some(live_agent(false)),
                    ..creation(CreationKind::Agent, 3, CreationPhase::Completed)
                })),
            }),
        },
        "out.fetch_agent.response" => SessionOutbound::FetchAgentResponse {
            payload: Box::new(FetchAgentResponse {
                request_id: s("r"),
                agent: Some(live_agent(true)),
                project: Some(placement()),
                error: None,
            }),
        },
        "out.fetch_agent.not_found" => SessionOutbound::FetchAgentResponse {
            payload: Box::new(FetchAgentResponse {
                request_id: s("r"),
                agent: None,
                project: None,
                error: Some(s("Agent not found: x")),
            }),
        },
        "out.fetch_agent.stored" => SessionOutbound::FetchAgentResponse {
            payload: Box::new(FetchAgentResponse {
                request_id: s("r"),
                agent: Some(stored_agent()),
                project: Some(placement()),
                error: None,
            }),
        },
        "out.fetch_agents.response" => SessionOutbound::FetchAgentsResponse {
            payload: FetchAgentsResponse {
                request_id: s("r"),
                subscription_id: None,
                entries: vec![AgentDirectoryEntry {
                    agent: live_agent(true),
                    project: placement(),
                }],
                page_info: no_more_pages(),
            },
        },
        "out.fetch_workspaces.response_git_data" => SessionOutbound::FetchWorkspacesResponse {
            payload: FetchWorkspacesResponse {
                request_id: s("r"),
                subscription_id: Some(s("sub-1")),
                entries: vec![WorkspaceDescriptor {
                    diff_stat: Some(DiffStat {
                        additions: n(3.0),
                        deletions: n(1.0),
                    }),
                    git_runtime: Some(GitRuntime {
                        current_branch: Some(s("main")),
                        remote_url: None,
                        is_paseo_owned_worktree: false,
                        is_dirty: Some(true),
                        ahead_behind: None,
                        ahead_of_origin: None,
                        behind_of_origin: None,
                    }),
                    github_runtime: Some(Nullable::Value(GitHubRuntime {
                        features_enabled: false,
                        pull_request: None,
                        error: None,
                    })),
                    forge: Some(s("github")),
                    ..workspace()
                }],
                empty_projects: Vec::new(),
                page_info: no_more_pages(),
            },
        },
        "out.fetch_agent_timeline.response" => SessionOutbound::FetchAgentTimelineResponse {
            payload: Box::new(FetchAgentTimelineResponse {
                request_id: s("r"),
                agent_id: s(AGENT_ID),
                agent: Some(live_agent(true)),
                direction: TimelineDirection::Tail,
                projection: TimelineProjection::Projected,
                epoch: s("ep-1"),
                reset: false,
                stale_cursor: false,
                gap: false,
                window: TimelineWindow {
                    min_seq: seq(1),
                    max_seq: seq(6),
                    next_seq: seq(7),
                },
                start_cursor: Some(TimelineCursor {
                    epoch: s("ep-1"),
                    seq: seq(1),
                }),
                end_cursor: Some(TimelineCursor {
                    epoch: s("ep-1"),
                    seq: seq(6),
                }),
                has_older: false,
                has_newer: false,
                merge_window: None,
                entries: timeline_entries(),
                error: None,
            }),
        },
        "out.fetch_agent_timeline.error" => SessionOutbound::FetchAgentTimelineResponse {
            payload: Box::new(FetchAgentTimelineResponse {
                request_id: s("r"),
                agent_id: s("missing"),
                agent: None,
                direction: TimelineDirection::Tail,
                projection: TimelineProjection::Projected,
                epoch: String::new(),
                reset: false,
                stale_cursor: false,
                gap: false,
                window: TimelineWindow {
                    min_seq: seq(0),
                    max_seq: seq(0),
                    next_seq: seq(0),
                },
                start_cursor: None,
                end_cursor: None,
                has_older: false,
                has_newer: false,
                merge_window: Some(true),
                entries: Vec::new(),
                error: Some(s("Agent not found: missing")),
            }),
        },
        "out.wait_for_finish.idle" => SessionOutbound::WaitForFinishResponse {
            payload: Box::new(WaitForFinishResponse {
                request_id: s("r"),
                status: WaitStatus::Idle,
                final_agent: Some(live_agent(true)),
                error: None,
                last_message: Some(s("Hello!")),
            }),
        },
        "out.session_pong" => SessionOutbound::Pong {
            payload: SessionPong {
                request_id: s("r"),
                client_sent_at: Some(Int::new(1).unwrap()),
                server_received_at: Int::new(1_790_000_000_000).unwrap(),
                server_sent_at: Int::new(1_790_000_000_000).unwrap(),
            },
        },
        "out.rpc_error" => SessionOutbound::RpcError {
            payload: RpcError {
                request_id: s("r"),
                request_type: Some(s("fetch_agent_request")),
                error: s("Invalid message"),
                code: Some(s("invalid_message")),
            },
        },
        "out.status_error" => SessionOutbound::Status {
            payload: StatusPayload::Error {
                message: s("Invalid message"),
            },
        },
        "out.subscription_responses" => SessionOutbound::SetAgentTimelineSubscriptionResponse {
            payload: SetAgentTimelineSubscriptionResponse {
                agent_ids: vec![s("a"), s("b")],
                request_id: s("r"),
                subscription_id: Some(s("sub-1")),
            },
        },
        "out.send_agent_message.response" => SessionOutbound::SendAgentMessageResponse {
            payload: SendAgentMessageResponse {
                request_id: s("r"),
                agent_id: s(AGENT_ID),
                accepted: true,
                error: None,
            },
        },
        _ => return None,
    };
    Some(session(message))
}
