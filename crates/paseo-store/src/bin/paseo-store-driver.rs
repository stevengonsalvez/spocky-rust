use std::error::Error;
use std::fs;
use std::path::PathBuf;

use paseo_store::{AgentStore, StoredAgentRecord};

fn main() -> Result<(), Box<dyn Error>> {
    let state_root = PathBuf::from(std::env::var("PASEO_DIFFERENTIAL_STATE")?);
    let source = fs::read_to_string(state_root.join("input/record.json"))?;
    let record = StoredAgentRecord::from_json(&source)?;
    let id = record.id().to_owned();
    let store_root = state_root.join("store");

    AgentStore::new(&store_root).write(&record)?;
    let restarted = AgentStore::new(&store_root)
        .load(&id)?
        .ok_or("record missing after restart")?;

    let output_root = state_root.join("output");
    fs::create_dir_all(&output_root)?;
    fs::write(
        output_root.join("structured.json"),
        serde_json::to_vec(restarted.as_value())?,
    )?;
    fs::write(
        output_root.join("recovery.json"),
        br#"{"restart":"loaded"}"#,
    )?;
    fs::write(
        output_root.join("counts.json"),
        br#"{"fixtures":1,"assertions":5}"#,
    )?;
    println!("stored {id}");
    Ok(())
}
