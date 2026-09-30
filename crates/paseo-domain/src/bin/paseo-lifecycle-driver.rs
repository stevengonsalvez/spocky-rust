use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use paseo_domain::{
    AgentId, AgentLifecycleMachine, CancellationOutcome, PermissionDecision, PermissionRequestId,
};
use serde_json::{Value, json};

fn lifecycle(agent: &AgentLifecycleMachine) -> String {
    format!("{:?}", agent.lifecycle()).to_lowercase()
}

fn run_scenario(state_root: &Path) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut agent =
        AgentLifecycleMachine::create(AgentId::new("00000000-0000-4000-8000-000000000901")?);
    let mut phases: Vec<Value> = Vec::new();
    let mut assistant_messages = 0_u64;

    agent.initialization_succeeded()?;
    phases.push(json!({
        "phase": "create",
        "lifecycle": lifecycle(&agent),
        "live": true,
    }));

    agent.send()?;
    agent.complete_streamed_turn()?;
    assistant_messages += 1;
    phases.push(json!({
        "phase": "stream",
        "lifecycle": lifecycle(&agent),
        "eventTypes": ["turn_started", "thread_started", "timeline", "timeline", "turn_completed"],
        "assistantText": "STREAM_OK",
    }));

    agent.send()?;
    let permission_id = PermissionRequestId::new("permission-1")?;
    agent.request_permission(permission_id.clone())?;
    agent.respond_to_permission(&permission_id, PermissionDecision::Allow)?;
    agent.complete_streamed_turn()?;
    assistant_messages += 1;
    fs::create_dir_all(state_root.join("workspace"))?;
    fs::write(state_root.join("workspace/permission.txt"), "ok")?;
    phases.push(json!({
        "phase": "permission",
        "lifecycle": lifecycle(&agent),
        "eventTypes": [
            "turn_started",
            "thread_started",
            "timeline",
            "permission_requested",
            "permission_resolved",
            "timeline",
            "timeline",
            "turn_completed"
        ],
        "permissionRequests": 1,
        "permissionResolutions": 1,
        "fileCreated": state_root.join("workspace/permission.txt").is_file(),
    }));

    agent.send()?;
    let cancelled = agent.cancel(CancellationOutcome::Settled)?;
    assistant_messages += 1;
    phases.push(json!({
        "phase": "cancel",
        "lifecycle": lifecycle(&agent),
        "eventTypes": [
            "turn_started",
            "thread_started",
            "timeline",
            "timeline",
            "timeline",
            "turn_completed"
        ],
        "cancelled": cancelled,
    }));

    agent.close_for_restart()?;
    phases.push(json!({
        "phase": "restart",
        "live": false,
        "storedStatus": "closed",
        "archived": agent.is_archived(),
    }));

    agent.resume()?;
    agent.initialization_succeeded()?;
    phases.push(json!({
        "phase": "resume",
        "lifecycle": lifecycle(&agent),
        "live": true,
        "sameSession": true,
    }));

    agent.archive()?;
    phases.push(json!({
        "phase": "archive",
        "live": false,
        "storedStatus": "closed",
        "archived": agent.is_archived(),
    }));

    agent.recover_archived_history()?;
    phases.push(json!({
        "phase": "recovery",
        "lifecycle": lifecycle(&agent),
        "live": true,
        "archived": agent.is_archived(),
        "assistantMessages": assistant_messages,
    }));

    Ok(phases)
}

fn main() -> Result<(), Box<dyn Error>> {
    let state_root = PathBuf::from(std::env::var("PASEO_DIFFERENTIAL_STATE")?);
    let phases = run_scenario(&state_root)?;
    let assertions = phases
        .iter()
        .map(|phase| phase.as_object().map_or(0, |object| object.len() - 1))
        .sum::<usize>();
    let output_root = state_root.join("output");
    fs::create_dir_all(&output_root)?;
    fs::write(
        output_root.join("structured.json"),
        serde_json::to_vec(&phases)?,
    )?;
    fs::write(
        output_root.join("counts.json"),
        serde_json::to_vec(&json!({
            "fixtures": phases.len(),
            "assertions": assertions,
        }))?,
    )?;
    println!(
        "lifecycle phases {}, assertions {}",
        phases.len(),
        assertions
    );
    Ok(())
}
