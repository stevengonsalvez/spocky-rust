use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use paseo_domain::{
    AgentId, AgentLifecycleMachine, AgentLifecycleRecord, CancellationOutcome, PermissionDecision,
    PermissionRequestId,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct RuntimeEvent {
    event_type: &'static str,
    assistant_text: Option<&'static str>,
}

#[derive(Debug, Default)]
struct Observation {
    events: Vec<RuntimeEvent>,
    permission_requests: usize,
    permission_resolutions: usize,
}

impl Observation {
    fn emit(&mut self, event_type: &'static str) {
        self.events.push(RuntimeEvent {
            event_type,
            assistant_text: None,
        });
    }

    fn emit_assistant(&mut self, text: &'static str) {
        self.events.push(RuntimeEvent {
            event_type: "timeline",
            assistant_text: Some(text),
        });
    }

    fn event_types(&self) -> Vec<&str> {
        self.events.iter().map(|event| event.event_type).collect()
    }

    fn assistant_text(&self) -> String {
        self.events
            .iter()
            .filter_map(|event| event.assistant_text)
            .collect()
    }
}

struct LifecycleRuntime {
    record: AgentLifecycleRecord,
}

impl LifecycleRuntime {
    fn new(record: AgentLifecycleRecord) -> Self {
        Self { record }
    }

    fn start_turn(&mut self) -> Result<Observation, Box<dyn Error>> {
        self.record.machine_mut().send()?;
        let mut observation = Observation::default();
        observation.emit("turn_started");
        observation.emit("thread_started");
        observation.emit("timeline");
        Ok(observation)
    }

    fn complete_turn(
        &mut self,
        observation: &mut Observation,
        assistant_text: &'static str,
    ) -> Result<(), Box<dyn Error>> {
        observation.emit_assistant(assistant_text);
        self.record.record_assistant_message(assistant_text);
        self.record.machine_mut().complete_streamed_turn()?;
        observation.emit("turn_completed");
        Ok(())
    }

    fn allow_permission(
        &mut self,
        observation: &mut Observation,
        request_id: &PermissionRequestId,
        side_effect_path: &Path,
    ) -> Result<(), Box<dyn Error>> {
        self.record
            .machine_mut()
            .request_permission(request_id.clone())?;
        observation.permission_requests += 1;
        observation.emit("permission_requested");
        fs::write(side_effect_path, "ok")?;
        self.record
            .machine_mut()
            .respond_to_permission(request_id, PermissionDecision::Allow)?;
        observation.permission_resolutions += 1;
        observation.emit("permission_resolved");
        observation.emit("timeline");
        Ok(())
    }

    fn cancel_turn(&mut self, observation: &mut Observation) -> Result<bool, Box<dyn Error>> {
        observation.emit("timeline");
        let cancelled = self
            .record
            .machine_mut()
            .cancel(CancellationOutcome::Settled)?;
        observation.emit_assistant("CANCELLED");
        self.record.record_assistant_message("CANCELLED");
        observation.emit("turn_completed");
        Ok(cancelled)
    }
}

fn lifecycle(agent: &AgentLifecycleMachine) -> String {
    format!("{:?}", agent.lifecycle()).to_lowercase()
}

fn append_persistence_phases(
    runtime: LifecycleRuntime,
    record_path: &Path,
    phases: &mut Vec<Value>,
) -> Result<(), Box<dyn Error>> {
    let session_id = runtime.record.provider_session_id().to_owned();
    let mut runtime = Some(runtime);
    runtime
        .as_mut()
        .expect("runtime exists")
        .record
        .machine_mut()
        .close_for_restart()?;
    runtime
        .as_ref()
        .expect("runtime exists")
        .record
        .save(record_path)?;
    drop(runtime.take());
    let closed = AgentLifecycleRecord::load(record_path)?;
    phases.push(json!({
        "phase": "restart",
        "live": runtime.is_some(),
        "storedStatus": lifecycle(closed.machine()),
        "archived": closed.machine().is_archived(),
    }));

    let same_session = closed.provider_session_id() == session_id;
    runtime = Some(LifecycleRuntime::new(closed));
    let resumed = runtime.as_mut().expect("runtime reconstructed");
    resumed.record.machine_mut().resume()?;
    resumed.record.machine_mut().initialization_succeeded()?;
    phases.push(json!({
        "phase": "resume",
        "lifecycle": lifecycle(runtime.as_ref().expect("runtime exists").record.machine()),
        "live": runtime.is_some(),
        "sameSession": same_session,
    }));

    let active = runtime.as_mut().expect("runtime exists");
    active.record.machine_mut().archive()?;
    active.record.save(record_path)?;
    drop(runtime.take());
    let archived = AgentLifecycleRecord::load(record_path)?;
    phases.push(json!({
        "phase": "archive",
        "live": runtime.is_some(),
        "storedStatus": lifecycle(archived.machine()),
        "archived": archived.machine().is_archived(),
    }));

    runtime = Some(LifecycleRuntime::new(archived));
    runtime
        .as_mut()
        .expect("runtime reconstructed")
        .record
        .machine_mut()
        .recover_archived_history()?;
    let recovered = runtime.as_ref().expect("runtime exists");
    phases.push(json!({
        "phase": "recovery",
        "lifecycle": lifecycle(recovered.record.machine()),
        "live": runtime.is_some(),
        "archived": recovered.record.machine().is_archived(),
        "assistantMessages": recovered.record.assistant_message_count(),
    }));
    Ok(())
}

fn run_scenario(state_root: &Path) -> Result<Vec<Value>, Box<dyn Error>> {
    let agent_id = "00000000-0000-4000-8000-000000000901";
    let record_path = state_root.join(format!("agents/{agent_id}.json"));
    let workspace = state_root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let machine = AgentLifecycleMachine::create(AgentId::new(agent_id)?);
    let record = AgentLifecycleRecord::new(machine, "provider-session-1");
    let mut runtime = Some(LifecycleRuntime::new(record));
    let mut phases: Vec<Value> = Vec::new();

    runtime
        .as_mut()
        .expect("runtime exists")
        .record
        .machine_mut()
        .initialization_succeeded()?;
    phases.push(json!({
        "phase": "create",
        "lifecycle": lifecycle(runtime.as_ref().expect("runtime exists").record.machine()),
        "live": runtime.is_some(),
    }));

    let mut streamed = runtime.as_mut().expect("runtime exists").start_turn()?;
    runtime
        .as_mut()
        .expect("runtime exists")
        .complete_turn(&mut streamed, "STREAM_OK")?;
    phases.push(json!({
        "phase": "stream",
        "lifecycle": lifecycle(runtime.as_ref().expect("runtime exists").record.machine()),
        "eventTypes": streamed.event_types(),
        "assistantText": streamed.assistant_text(),
    }));

    let mut permission = runtime.as_mut().expect("runtime exists").start_turn()?;
    let permission_file = workspace.join("permission.txt");
    let permission_id = PermissionRequestId::new("permission-1")?;
    runtime.as_mut().expect("runtime exists").allow_permission(
        &mut permission,
        &permission_id,
        &permission_file,
    )?;
    runtime
        .as_mut()
        .expect("runtime exists")
        .complete_turn(&mut permission, "PERMISSION_OK")?;
    phases.push(json!({
        "phase": "permission",
        "lifecycle": lifecycle(runtime.as_ref().expect("runtime exists").record.machine()),
        "eventTypes": permission.event_types(),
        "permissionRequests": permission.permission_requests,
        "permissionResolutions": permission.permission_resolutions,
        "fileCreated": permission_file.is_file(),
    }));

    let mut cancellation = runtime.as_mut().expect("runtime exists").start_turn()?;
    let cancelled = runtime
        .as_mut()
        .expect("runtime exists")
        .cancel_turn(&mut cancellation)?;
    phases.push(json!({
        "phase": "cancel",
        "lifecycle": lifecycle(runtime.as_ref().expect("runtime exists").record.machine()),
        "eventTypes": cancellation.event_types(),
        "cancelled": cancelled,
    }));

    append_persistence_phases(
        runtime.take().expect("runtime exists"),
        &record_path,
        &mut phases,
    )?;

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
