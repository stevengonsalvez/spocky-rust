//! `parse_iso` (the daemon's `Date.parse(startedAt)` of `pid-lock.ts:59`)
//! against what Node 22.20.0 printed for the same strings
//! (`tests/fixtures/date-parse-vectors.json`, from `gen-date-parse-vectors.cjs`).
//! The strings include non-ISO forms V8 accepts and ones it rejects.

use std::process::Command;

use serde_json::Value;
use spocky_daemon::iso_time::parse_iso;

/// The value of `NAME=value` in `scripts/phase3/pins.sh`.
fn pin(name: &str) -> String {
    include_str!("../../../scripts/phase3/pins.sh")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{name} is not in pins.sh"))
        .trim_matches('"')
        .to_owned()
}

/// The strings of `const <name> = [ ... ];` in the generator, one JSON string
/// per line, so a fixture that was edited by hand or left behind by an edited
/// generator is caught.
fn generator_inputs(name: &str) -> Vec<String> {
    include_str!("fixtures/gen-date-parse-vectors.cjs")
        .lines()
        .skip_while(|line| !line.starts_with(&format!("const {name} = [")))
        .skip(1)
        .take_while(|line| *line != "];")
        .map(|line| {
            let literal = line.trim().trim_end_matches(',');
            serde_json::from_str::<String>(literal)
                .unwrap_or_else(|error| panic!("{name}: {literal}: {error}"))
        })
        .collect()
}

#[test]
fn the_fixture_texts_are_the_generator_inputs() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/date-parse-vectors.json")).unwrap();
    let texts: Vec<&str> = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| case["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, generator_inputs("inputs"));
}

/// The zones of the local-time cases, in fixture order.
fn local_zones(fixture: &Value) -> Vec<&str> {
    let mut zones: Vec<&str> = Vec::new();
    for case in fixture["localCases"].as_array().unwrap() {
        let zone = case["zone"].as_str().unwrap();
        if !zones.contains(&zone) {
            zones.push(zone);
        }
    }
    zones
}

#[test]
fn the_local_time_fixture_texts_are_the_generator_inputs() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/date-parse-vectors.json")).unwrap();
    let inputs = generator_inputs("localInputs");
    let zones = local_zones(&fixture);
    assert!(zones.contains(&"UTC"), "the harness zone is covered");
    assert!(zones.len() >= 2, "a zone with an offset is covered");
    for zone in zones {
        let texts: Vec<&str> = fixture["localCases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["zone"] == zone)
            .map(|case| case["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, inputs, "{zone}");
    }
}

const ZONE_VARIABLE: &str = "SPOCKY_DATE_PARSE_ZONE";

/// V8 reads local time from the zone set when the process starts, and so does
/// `parse_iso`, so each zone runs in a child of this test binary with `TZ` set.
#[test]
fn local_time_strings_match_node_in_each_zone() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/date-parse-vectors.json")).unwrap();
    for zone in local_zones(&fixture) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_time_strings_in_the_zone_of_the_environment",
                "--nocapture",
            ])
            .env("TZ", zone)
            .env(ZONE_VARIABLE, zone)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{zone}: {stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!("checked {zone} ")),
            "{zone}: the child did not run the check: {stdout}"
        );
    }
}

/// The child of [`local_time_strings_match_node_in_each_zone`]; a no-op when
/// run on its own.
#[test]
fn local_time_strings_in_the_zone_of_the_environment() {
    let Ok(zone) = std::env::var(ZONE_VARIABLE) else {
        return;
    };
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/date-parse-vectors.json")).unwrap();
    let mut checked = 0;
    for case in fixture["localCases"].as_array().unwrap() {
        if case["zone"] != zone.as_str() {
            continue;
        }
        let text = case["text"].as_str().unwrap();
        assert_eq!(
            parse_iso(text),
            case["ms"].as_i64(),
            "Date.parse({text:?}) in {zone}"
        );
        checked += 1;
    }
    assert!(checked >= 19, "{checked} cases in {zone}");
    println!("checked {zone} {checked}");
}

#[test]
fn parse_iso_matches_node_date_parse() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/date-parse-vectors.json")).unwrap();
    assert_eq!(
        fixture["node"].as_str(),
        Some(format!("v{}", pin("P3_NODE_VERSION")).as_str())
    );
    assert_eq!(
        fixture["nodeSha256"].as_str(),
        Some(pin("P3_NODE_BINARY_SHA256").as_str())
    );
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 30);
    // At least one string no ISO-only parser reads, so a regression to one fails.
    assert!(cases.iter().any(|case| {
        case["text"] == "Thu, 01 Oct 2026 15:17:04 GMT" && case["ms"].as_i64().is_some()
    }));
    for case in cases {
        let text = case["text"].as_str().unwrap();
        assert_eq!(parse_iso(text), case["ms"].as_i64(), "Date.parse({text:?})");
    }
}
