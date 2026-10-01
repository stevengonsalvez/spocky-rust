//! Scripted loopback Responses API stub for the Phase 3 slice gates.
//!
//! Usage: `spocky-responses-stub <script.json> <record.jsonl> <port-file>`.
//! Binds 127.0.0.1 on an ephemeral port that is never 6767 or 6768, writes
//! the port to `<port-file>` only after the listener is bound, then serves
//! until stopped.

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::ExitCode;

use spocky_slice_harness::stub::{Script, bind_loopback, serve};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("spocky-responses-stub: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    const USAGE: &str = "usage: spocky-responses-stub <script.json> <record.jsonl> <port-file>";
    let arguments: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [script_path, record_path, port_path] = arguments.as_slice() else {
        return Err(USAGE.into());
    };
    let script: Script = serde_json::from_slice(&fs::read(script_path)?)?;
    let record = File::options()
        .create_new(true)
        .append(true)
        .open(record_path)?;
    let listener = bind_loopback()?;
    let port = listener.local_addr()?.port();
    let staging = port_path.with_extension("tmp");
    fs::write(&staging, format!("{port}\n"))?;
    fs::rename(&staging, port_path)?;
    serve(&listener, script, record)?;
    Ok(())
}
