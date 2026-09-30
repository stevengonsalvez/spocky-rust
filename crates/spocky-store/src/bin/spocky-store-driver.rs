use std::error::Error;
use std::fs;
use std::path::PathBuf;

use spocky_store::{AgentStore, StoredAgentRecord};

fn main() -> Result<(), Box<dyn Error>> {
    let state_root = PathBuf::from(std::env::var("PASEO_DIFFERENTIAL_STATE")?);
    let source = fs::read_to_string(state_root.join("input/record.json"))?;
    let record = StoredAgentRecord::from_json(&source)?;
    let id = record.id().to_owned();
    let store_root = state_root.join("store");
    let mut assertions = 0_u64;

    let persisted_path = AgentStore::new(&store_root).write(&record)?;
    if !persisted_path.is_file() {
        return Err("persisted record is missing".into());
    }
    assertions += 1;
    let restarted = AgentStore::new(&store_root)
        .load(&id)?
        .ok_or("record missing after restart")?;
    assertions += 1;
    if restarted.id() != id {
        return Err("restarted record id differs".into());
    }
    assertions += 1;
    if restarted.as_value() != record.as_value() {
        return Err("restarted record value differs".into());
    }
    assertions += 1;
    let persisted: serde_json::Value = serde_json::from_slice(&fs::read(&persisted_path)?)?;
    if &persisted != record.as_value() {
        return Err("persisted record value differs".into());
    }
    assertions += 1;

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
        serde_json::to_vec(&serde_json::json!({
            "fixtures": 1,
            "assertions": assertions,
        }))?,
    )?;
    println!("stored {id}");
    Ok(())
}
