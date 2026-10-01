use std::io::{self, BufRead as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use spocky_hub_pilot::{IpcValue, RetainedHostError, RetainedPgliteConfig, RetainedPgliteHost};

fn main() {
    let mode = std::env::args().nth(1).expect("mode");
    let data_directory = PathBuf::from(std::env::args_os().nth(2).expect("data directory"));
    match mode.as_str() {
        "try-open" => try_open(data_directory),
        "hold" => hold(data_directory),
        other => panic!("unknown mode: {other}"),
    }
}

fn try_open(data_directory: PathBuf) {
    match RetainedPgliteHost::open(&config(data_directory)) {
        Err(RetainedHostError::DirectoryInUse) => print_json(&json!({
            "operation": "candidate-try-open",
            "opened": false,
            "error": "directory-in-use"
        })),
        Err(error) => panic!("unexpected candidate open error: {error}"),
        Ok(host) => {
            host.close().expect("close unexpected candidate owner");
            print_json(&json!({
                "operation": "candidate-try-open",
                "opened": true
            }));
        }
    }
}

fn hold(data_directory: PathBuf) {
    let host = RetainedPgliteHost::open(&config(data_directory)).expect("open candidate owner");
    let migration = host.migrate().expect("migrate candidate owner");
    let baseline_marker_payload = marker_payload(&host, "pinned-baseline");
    print_json(&json!({
        "operation": "candidate-hold",
        "event": "ready",
        "journalRows": migration.journal_rows,
        "baselineMarkerPayload": baseline_marker_payload
    }));

    let mut command = String::new();
    io::stdin()
        .lock()
        .read_line(&mut command)
        .expect("read close command");
    assert_eq!(command.trim(), "close", "unexpected hold command");
    host.close().expect("close candidate owner");
    print_json(&json!({
        "operation": "candidate-hold",
        "event": "closed"
    }));
}

fn config(data_directory: PathBuf) -> RetainedPgliteConfig {
    RetainedPgliteConfig {
        node_executable: env_path("SPOCKY_NODE"),
        adapter_path: env_path("SPOCKY_PGLITE_ADAPTER"),
        package_root: env_path("SPOCKY_PGLITE_PACKAGE"),
        migrations_root: env_path("SPOCKY_HUB_MIGRATIONS"),
        data_directory,
        max_frame_bytes: 1_048_576,
        startup_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(20),
    }
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")))
}

fn marker_payload(host: &RetainedPgliteHost, producer: &str) -> String {
    let result = host
        .query(
            "select payload from mixed_ownership_probe where producer = $1",
            &[IpcValue::String(producer.into())],
        )
        .expect("query ownership marker");
    match result.rows.as_slice() {
        [row] => match row.as_slice() {
            [IpcValue::String(payload)] => payload.clone(),
            other => panic!("unexpected marker row: {other:?}"),
        },
        other => panic!("unexpected marker result: {other:?}"),
    }
}

fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string(&value).expect("serialize evidence")
    );
    io::stdout().flush().expect("flush evidence");
}
