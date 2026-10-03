//! `parse_iso` (the daemon's `Date.parse(startedAt)` of `pid-lock.ts:59`)
//! against what Node 22.20.0 printed for the same strings
//! (`tests/fixtures/date-parse-vectors.json`, from `gen-date-parse-vectors.cjs`).
//! The strings include non-ISO forms V8 accepts and ones it rejects.

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
