use std::error::Error;
use std::fs;
use std::path::PathBuf;

use paseo_domain::{AgentId, AgentLifecycleMachine, CancellationOutcome};
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn Error>> {
    let state_root = PathBuf::from(std::env::var("PASEO_DIFFERENTIAL_STATE")?);
    let results = vec![
        run_case("not_running", None)?,
        run_case("settled", Some(CancellationOutcome::Settled))?,
        run_case("race", Some(CancellationOutcome::AlreadySettled))?,
        run_case("refused", Some(CancellationOutcome::Refused))?,
    ];

    let output_root = state_root.join("output");
    fs::create_dir_all(&output_root)?;
    fs::write(
        output_root.join("structured.json"),
        serde_json::to_vec(&results)?,
    )?;
    fs::write(
        output_root.join("counts.json"),
        br#"{"fixtures":4,"assertions":16}"#,
    )?;
    println!("lifecycle cases {}", results.len());
    Ok(())
}

fn run_case(
    id: &'static str,
    outcome: Option<CancellationOutcome>,
) -> Result<Value, Box<dyn Error>> {
    let mut agent = AgentLifecycleMachine::create(AgentId::new("agent-1")?);
    agent.initialization_succeeded()?;
    if outcome.is_some() {
        agent.send()?;
    }

    let result = agent.cancel(outcome.unwrap_or(CancellationOutcome::Settled));
    let (ok, cancelled, error) = match result {
        Ok(cancelled) => (true, cancelled, Value::Null),
        Err(error) => (false, false, json!(error.to_string())),
    };
    Ok(json!({
        "id": id,
        "ok": ok,
        "cancelled": cancelled,
        "lifecycle": format!("{:?}", agent.lifecycle()).to_lowercase(),
        "error": error,
    }))
}
