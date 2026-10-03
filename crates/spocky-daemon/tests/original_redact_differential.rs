//! The daemon's record writer against the pinned original's own
//! `createRootLogger` (pino 10.3.1, `REDACT_PATHS`, `remove: true`), from
//! `tests/fixtures/original-redact-vectors.json`: each case is one record, compared
//! as a raw line, with the key order and duplicate keys pino writes. Only the
//! time, pid and host name, which the run decides, are masked.

mod common;

use std::sync::{Arc, Mutex};

use serde_json::Value;
use spocky_daemon::log::{JsonLineLogger, Level};
use spocky_daemon::process::daemon_logger;

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The record with the values the run decides replaced by fixed ones.
fn mask(line: &str, hostname: &str) -> String {
    let mut text = line.replace(
        &format!("\"hostname\":\"{hostname}\""),
        "\"hostname\":\"HOST\"",
    );
    for key in ["\"time\":", "\"pid\":"] {
        let start = text.find(key).expect(key) + key.len();
        let end = start + text[start..].find(',').unwrap();
        text.replace_range(start..end, "N");
    }
    text
}

fn level(name: &str) -> Level {
    match name {
        "trace" => Level::Trace,
        "info" => Level::Info,
        "warn" => Level::Warn,
        "error" => Level::Error,
        "fatal" => Level::Fatal,
        other => panic!("unknown level {other}"),
    }
}

#[test]
fn records_match_the_original_logger_including_redaction() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/original-redact-vectors.json")).unwrap();
    common::assert_fixture_provenance(&fixture);
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let sink = Sink::default();
        // createRootLogger(...).child({ daemonVersion }), then the case's children.
        let bindings: Vec<(String, Value)> = case["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|binding| binding.as_object().unwrap().clone())
            .collect();
        let logger: JsonLineLogger<Sink> = daemon_logger(sink.clone())
            .with_level(Level::Trace)
            .with_value_bindings(bindings);
        let fields: Vec<(String, Value)> = case["fields"]
            .as_object()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        logger.log_values(
            level(case["level"].as_str().unwrap()),
            &fields,
            case["msg"].as_str().unwrap(),
        );

        let written = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            mask(written.trim_end(), &hostname),
            mask(case["line"].as_str().unwrap(), &hostname),
            "{name}"
        );
    }
}
