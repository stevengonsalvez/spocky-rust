//! Golden parity against the pinned Paseo validators.
//!
//! `tests/fixtures/g1-golden.json` is written by
//! `scripts/phase3/contracts-capture.mjs` from Paseo `5de45e2` with node
//! v22.20.0. For every case:
//!
//! - inbound: Rust accepts exactly when the daemon's zod parse accepts, and
//!   writes byte-for-byte what zod outputs (key order, defaults, stripping);
//! - outbound: Rust accepts exactly when the client's zod-aot validator
//!   accepts, the validator returns the daemon text unchanged, and Rust
//!   writes the same bytes back.

use std::fs;
use std::path::Path;

use serde_json::Value;
use spocky_contracts::frame::{WsInbound, WsOutbound, frame_text, parse_frame};

/// Raised only by recapturing; a lower count fails the run.
const EXPECTED_CASES: usize = 69;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/g1-golden.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string at {pointer}"))
}

fn flag(value: &Value, pointer: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("missing bool at {pointer}"))
}

fn check_inbound(case: &Value, id: &str, input: &str, failures: &mut Vec<String>) {
    let accepted = flag(case, "/zod/success");
    match (parse_frame::<WsInbound>(input), accepted) {
        (Ok(frame), true) => {
            let written = frame_text(&frame).unwrap();
            let expected = text(case, "/zod/output");
            if written != expected {
                failures.push(format!("{id}: wrote\n  {written}\nzod\n  {expected}"));
            }
        }
        (Err(_), false) => {}
        (Ok(frame), false) => failures.push(format!("{id}: zod rejects, Rust accepted {frame:?}")),
        (Err(error), true) => failures.push(format!("{id}: zod accepts, Rust rejected: {error}")),
    }
}

fn check_outbound(case: &Value, id: &str, input: &str, failures: &mut Vec<String>) {
    let accepted = flag(case, "/aot/success");
    if accepted && text(case, "/aot/output") != input {
        failures.push(format!("{id}: client validator changed the daemon text"));
    }
    match (parse_frame::<WsOutbound>(input), accepted) {
        (Ok(frame), true) => {
            let written = frame_text(&frame).unwrap();
            if written != input {
                failures.push(format!("{id}: wrote\n  {written}\ndaemon\n  {input}"));
            }
        }
        (Err(_), false) => {}
        (Ok(frame), false) => {
            failures.push(format!("{id}: client rejects, Rust accepted {frame:?}"));
        }
        (Err(error), true) => {
            failures.push(format!("{id}: client accepts, Rust rejected: {error}"));
        }
    }
}

#[test]
fn fixture_provenance_is_pinned() {
    let fixture = fixture();
    assert_eq!(
        text(&fixture, "/provenance/paseoCommit"),
        "5de45e208690b0efc51c59a585ae9729325a9204"
    );
    assert_eq!(text(&fixture, "/provenance/node"), "v22.20.0");
    assert_eq!(text(&fixture, "/provenance/zod"), "4.4.3");
    assert_eq!(text(&fixture, "/provenance/zodAot"), "0.20.4");
}

#[test]
fn every_case_matches_pinned_validators() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().expect("cases array");
    assert_eq!(cases.len(), EXPECTED_CASES, "fixture case count changed");
    let mut failures = Vec::new();
    for case in cases {
        let id = text(case, "/id");
        let input = text(case, "/input");
        match text(case, "/direction") {
            "inbound" => check_inbound(case, id, input, &mut failures),
            "outbound" => check_outbound(case, id, input, &mut failures),
            other => failures.push(format!("{id}: unknown direction {other}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
