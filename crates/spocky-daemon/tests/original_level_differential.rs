//! The level the daemon's root logger gets from a config `log` section, and the
//! records it then writes, against the pinned original's `createRootLogger`
//! (pino 10.3.1) in `tests/fixtures/original-level-vectors.json`, from
//! `gen-original-level-vectors.cjs`: the level for the worker (file: false) and for
//! a caller that allows files, and which of one record per level is written.

mod common;

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use spocky_daemon::config_file::{configured_log_level, load_persisted_config};
use spocky_daemon::log::{JsonLineLogger, Level, Logger, NullLogger};

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

#[test]
fn log_levels_and_filtering_match_the_original_logger() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/original-level-vectors.json")).unwrap();
    common::assert_fixture_provenance(&fixture);
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 12);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut config = json!({"version": 1});
        if !case["log"].is_null() {
            config["log"] = case["log"].clone();
        }
        fs::write(home.path().join("config.json"), config.to_string()).unwrap();
        assert!(
            load_persisted_config(home.path(), &NullLogger).is_ok(),
            "{name}: the config is valid"
        );

        let worker = configured_log_level(home.path(), false);
        assert_eq!(
            Some(worker),
            Level::from_name(case["workerLevel"].as_str().unwrap()),
            "{name}: the worker's level"
        );
        assert_eq!(
            Some(configured_log_level(home.path(), true)),
            Level::from_name(case["fileAllowedLevel"].as_str().unwrap()),
            "{name}: the level when files are allowed"
        );

        let sink = Sink::default();
        let logger = JsonLineLogger::new(sink.clone(), Vec::new()).with_level(worker);
        logger.trace(&[], "trace");
        logger.debug(&[], "debug");
        logger.info(&[], "info");
        logger.warn(&[], "warn");
        logger.error(&[], "error");
        logger.fatal(&[], "fatal");
        let written: Vec<String> = String::from_utf8(sink.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["msg"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let expected: Vec<&str> = case["written"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level.as_str().unwrap())
            .collect();
        assert_eq!(written, expected, "{name}: the records written");
    }
}
