//! The libuv names and descriptions the daemon puts into system error messages,
//! against what Node 22.20.0 reports on darwin (`tests/fixtures/uv-errors.json`,
//! from `gen-uv-errors.cjs`).
#![cfg(target_os = "macos")]

mod common;

use std::io;

use serde_json::Value;
use spocky_store::atomic::FsError;

#[test]
fn error_names_and_descriptions_match_nodes_table() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/uv-errors.json")).unwrap();
    assert_eq!(fixture["platform"], "darwin");
    common::assert_node_pin(&fixture);
    let mut mismatches = Vec::new();
    for entry in fixture["errors"].as_array().unwrap() {
        let errno = i32::try_from(-entry[0].as_i64().unwrap()).unwrap();
        // libuv's own codes (EOF, the EAI_* family) are below any errno.
        if errno > 200 {
            continue;
        }
        let error = FsError {
            syscall: "listen",
            path: None,
            dest: None,
            source: io::Error::from_raw_os_error(errno),
        };
        let (name, description) = (entry[1].as_str().unwrap(), entry[2].as_str().unwrap());
        if error.code() != name || error.description() != description {
            mismatches.push(format!(
                "{errno}: {} / {} (node: {name} / {description})",
                error.code(),
                error.description()
            ));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}
