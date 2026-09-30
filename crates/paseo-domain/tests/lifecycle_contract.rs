use paseo_domain::{
    AgentId, AgentLifecycle, AgentLifecycleMachine, AgentLifecycleRecord, AgentStateBucket,
    AttentionReason, CancellationOutcome, DomainError, PermissionDecision, PermissionRequestId,
};
use std::fs;

fn agent() -> AgentLifecycleMachine {
    AgentLifecycleMachine::create(AgentId::new("agent-contract").expect("valid agent id"))
}

#[test]
fn create_send_permission_cancel_restart_resume_archive_flow() {
    let mut agent = agent();
    assert_eq!(agent.lifecycle(), AgentLifecycle::Initializing);
    assert_eq!(agent.bucket(), AgentStateBucket::Done);

    agent
        .initialization_succeeded()
        .expect("creation reaches idle");
    assert_eq!(agent.lifecycle(), AgentLifecycle::Idle);

    agent.send().expect("idle agent accepts send");
    assert_eq!(agent.lifecycle(), AgentLifecycle::Running);
    assert_eq!(agent.bucket(), AgentStateBucket::Running);

    let request = PermissionRequestId::new("permission-1").expect("valid request id");
    agent
        .request_permission(request.clone())
        .expect("running agent requests permission");
    assert_eq!(agent.bucket(), AgentStateBucket::NeedsInput);
    assert_eq!(agent.attention_reason(), Some(AttentionReason::Permission));

    agent
        .respond_to_permission(&request, PermissionDecision::Allow)
        .expect("matching permission response accepted");
    assert_eq!(agent.lifecycle(), AgentLifecycle::Running);
    assert_eq!(agent.bucket(), AgentStateBucket::Running);

    assert!(
        agent
            .cancel(CancellationOutcome::Settled)
            .expect("cancel accepted")
    );
    assert_eq!(agent.lifecycle(), AgentLifecycle::Idle);

    agent
        .close_for_restart()
        .expect("live agent closes on restart");
    assert_eq!(agent.lifecycle(), AgentLifecycle::Closed);
    agent.resume().expect("stored closed agent resumes");
    assert_eq!(agent.lifecycle(), AgentLifecycle::Initializing);
    agent
        .initialization_succeeded()
        .expect("resumed session reaches idle");

    agent.send().expect("resumed agent accepts send");
    agent.archive().expect("archive normalizes active agent");
    assert!(agent.is_archived());
    assert_eq!(agent.lifecycle(), AgentLifecycle::Closed);
    assert_eq!(agent.bucket(), AgentStateBucket::Done);
    assert_eq!(agent.send(), Err(DomainError::Archived));
    assert_eq!(agent.resume(), Err(DomainError::Archived));
}

#[test]
fn cancellation_distinguishes_not_running_race_and_refusal() {
    let mut agent = agent();
    agent
        .initialization_succeeded()
        .expect("creation reaches idle");
    assert_eq!(agent.cancel(CancellationOutcome::Settled), Ok(false));

    agent.send().expect("start first turn");
    assert_eq!(agent.cancel(CancellationOutcome::AlreadySettled), Ok(false));
    assert_eq!(agent.lifecycle(), AgentLifecycle::Idle);

    agent.send().expect("start second turn");
    assert_eq!(
        agent.cancel(CancellationOutcome::Refused),
        Err(DomainError::CancellationRefused {
            agent_id: "agent-contract".into()
        })
    );
    assert_eq!(agent.lifecycle(), AgentLifecycle::Running);
}

#[test]
fn recovery_retains_closed_state_after_failed_resume() {
    let mut agent = agent();
    agent
        .initialization_succeeded()
        .expect("creation reaches idle");
    agent
        .close_for_restart()
        .expect("restart closes live session");
    agent.resume().expect("resume starts initialization");
    agent
        .initialization_failed("resume unavailable")
        .expect("failed resume recorded");

    assert_eq!(agent.lifecycle(), AgentLifecycle::Closed);
    assert_eq!(agent.last_error(), Some("resume unavailable"));
    agent.resume().expect("closed record remains recoverable");
    agent
        .initialization_succeeded()
        .expect("retry reaches idle");
    assert_eq!(agent.lifecycle(), AgentLifecycle::Idle);
    assert_eq!(agent.last_error(), None);
}

#[test]
fn invalid_transitions_and_permission_ids_are_rejected() {
    let mut agent = agent();
    assert_eq!(agent.send(), Err(DomainError::InvalidTransition));
    assert_eq!(agent.resume(), Err(DomainError::InvalidTransition));
    assert_eq!(
        agent.request_permission(PermissionRequestId::new("early").expect("valid id")),
        Err(DomainError::InvalidTransition)
    );

    agent
        .initialization_succeeded()
        .expect("creation reaches idle");
    agent.send().expect("turn starts");
    let expected = PermissionRequestId::new("expected").expect("valid id");
    let wrong = PermissionRequestId::new("wrong").expect("valid id");
    agent
        .request_permission(expected)
        .expect("permission becomes pending");
    assert_eq!(
        agent.respond_to_permission(&wrong, PermissionDecision::Deny),
        Err(DomainError::PermissionRequestNotFound)
    );
}

#[test]
fn bucket_priority_matches_protocol_contract() {
    assert!(AgentStateBucket::NeedsInput.priority() < AgentStateBucket::Failed.priority());
    assert!(AgentStateBucket::Failed.priority() < AgentStateBucket::Running.priority());
    assert!(AgentStateBucket::Running.priority() < AgentStateBucket::Attention.priority());
    assert!(AgentStateBucket::Attention.priority() < AgentStateBucket::Done.priority());
}

#[test]
fn streamed_completion_returns_idle_and_marks_finished_attention() {
    let mut agent = agent();
    agent
        .initialization_succeeded()
        .expect("creation reaches idle");
    agent.send().expect("turn starts");

    agent
        .complete_streamed_turn()
        .expect("stream completion settles the turn");

    assert_eq!(agent.lifecycle(), AgentLifecycle::Idle);
    assert_eq!(agent.attention_reason(), Some(AttentionReason::Finished));
    assert_eq!(agent.bucket(), AgentStateBucket::Attention);
}

#[test]
fn archived_history_can_recover_read_only_without_unarchiving() {
    let mut agent = agent();
    agent
        .initialization_succeeded()
        .expect("creation reaches idle");
    agent.archive().expect("agent archives");

    agent
        .recover_archived_history()
        .expect("archived history loads");

    assert!(agent.is_archived());
    assert_eq!(agent.lifecycle(), AgentLifecycle::Idle);
    assert_eq!(agent.send(), Err(DomainError::Archived));
}

#[test]
fn lifecycle_record_roundtrips_machine_session_and_history() {
    let root = std::env::temp_dir().join(format!("paseo-lifecycle-record-{}", std::process::id()));
    let path = root.join("agents/agent-contract.json");
    let mut machine = agent();
    machine
        .initialization_succeeded()
        .expect("creation reaches idle");
    machine
        .close_for_restart()
        .expect("restart closes live session");
    let mut record = AgentLifecycleRecord::new(machine, "provider-session-1");
    record.record_assistant_message("STREAM_OK");
    record
        .save(&path)
        .expect("persist lifecycle record atomically");

    drop(record);
    let loaded = AgentLifecycleRecord::load(&path).expect("reconstruct lifecycle from disk");
    assert_eq!(loaded.machine().lifecycle(), AgentLifecycle::Closed);
    assert_eq!(loaded.provider_session_id(), "provider-session-1");
    assert_eq!(loaded.assistant_message_count(), 1);

    fs::remove_dir_all(root).expect("remove lifecycle record fixture");
}
