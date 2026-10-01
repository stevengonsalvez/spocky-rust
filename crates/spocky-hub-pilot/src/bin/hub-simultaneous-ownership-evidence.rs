use std::fs;
use std::io::{self, BufRead as _, Write as _};
use std::path::PathBuf;

use serde_json::json;
use spocky_hub_pilot::{EmbeddedSqlStore, StoreError};

fn main() {
    let mode = std::env::args().nth(1).expect("mode");
    let data_directory = PathBuf::from(std::env::args_os().nth(2).expect("data directory"));
    if mode == "pause-before-open" {
        print_json(&json!({"event": "paused-before-owner-open"}));
        expect_command("open");
    } else if mode != "hold" {
        panic!("unknown mode: {mode}");
    }

    match EmbeddedSqlStore::open(&data_directory) {
        Ok(store) => {
            let tables = store.relational_tables().expect("inspect candidate store");
            print_json(&json!({
                "event": "owner-ready",
                "opened": true,
                "owner": serde_json::from_slice::<serde_json::Value>(
                    &fs::read(data_directory.join(".paseo-hub.lock")).expect("read owner")
                ).expect("parse owner"),
                "tableCount": tables.len()
            }));
            expect_command("close");
            drop(store);
            print_json(&json!({"event": "closed"}));
        }
        Err(StoreError::EmbeddedDirectoryInUse(_)) => {
            print_json(&json!({
                "event": "open-rejected",
                "opened": false,
                "error": "directory-in-use"
            }));
        }
        Err(error) => panic!("unexpected candidate open error: {error}"),
    }
}

fn expect_command(expected: &str) {
    let mut command = String::new();
    io::stdin()
        .lock()
        .read_line(&mut command)
        .expect("read command");
    assert_eq!(command.trim(), expected, "unexpected command");
}

fn print_json(value: &serde_json::Value) {
    println!("{}", serde_json::to_string(value).expect("serialize event"));
    io::stdout().flush().expect("flush event");
}
