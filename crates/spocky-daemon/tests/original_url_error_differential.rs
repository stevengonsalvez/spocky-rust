//! A listening handler that throws a `TypeError` from `new URL(...)`: the fatal
//! record the daemon writes against the pinned original's pino output
//! (`tests/fixtures/original-url-error-vectors.json`, from
//! `gen-original-url-error-vectors.cjs`). The error is built from the URL
//! arguments the way a session backend builds it: name, message, then the own
//! properties `code`, `input` and, when a base was given, `base`, in the order
//! Node defines them.
//!
//! Masked: time, pid and host name. The stack is compared by its first line; the
//! frames below it are Node's and the caller's.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use spocky_daemon::daemon::{DaemonEnv, NoSessionBackend, start};
use spocky_daemon::listen::ListenTarget;
use spocky_daemon::log::Logger;
use spocky_daemon::process::daemon_logger;
use spocky_daemon::session_api::{SessionBackend, SessionError, SessionHandle, SessionOpen};

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

/// A backend whose listening handler throws `error`.
struct Throws(SessionError);

impl SessionBackend for Throws {
    fn open(&self, open: SessionOpen) -> Arc<dyn SessionHandle> {
        NoSessionBackend.open(open)
    }
    fn validate_inbound(&self, message: &Value) -> Result<(), String> {
        NoSessionBackend.validate_inbound(message)
    }
    fn listening_error(&self, _bound: &ListenTarget) -> Result<(), SessionError> {
        Err(self.0.clone())
    }
}

/// `new URL(...args)` failing, as Node builds the error.
fn invalid_url(args: &[&str]) -> SessionError {
    let mut error = SessionError::new("TypeError", "Invalid URL")
        .with_property("code", "ERR_INVALID_URL")
        .with_property("input", args[0]);
    if let Some(base) = args.get(1) {
        error = error.with_property("base", *base);
    }
    error
}

fn keys(value: &Value) -> Vec<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

/// The key order of the record and of `err`, and the record with the values the
/// run decides replaced and the stack cut to its first line.
fn normalized(line: &str) -> (Vec<String>, Vec<String>, Value) {
    let mut record: Value = serde_json::from_str(line).unwrap();
    let (record_keys, err_keys) = (keys(&record), keys(&record["err"]));
    for key in ["time", "pid", "hostname"] {
        record[key] = json!("N");
    }
    let stack = record["err"]["stack"].as_str().unwrap();
    record["err"]["stack"] = json!(stack.lines().next().unwrap());
    (record_keys, err_keys, record)
}

#[test]
fn an_invalid_url_thrown_while_listening_is_logged_as_the_original_does() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/original-url-error-vectors.json")).unwrap();
    common::assert_fixture_provenance(&fixture);
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 4);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let args: Vec<&str> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();

        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.json"),
            json!({"daemon": {"listen": "127.0.0.1:0"}}).to_string(),
        )
        .unwrap();
        let env = DaemonEnv::new(
            HashMap::from([("PASEO_HOME".to_owned(), home.display().to_string())]),
            PathBuf::from("/"),
            Some(PathBuf::from("/Users/example")),
        );
        let sink = Sink::default();
        let logger: Arc<dyn Logger> = Arc::new(daemon_logger(sink.clone()));
        let backend = Arc::new(Throws(invalid_url(&args)));
        let error = start(&env, backend, &logger)
            .err()
            .unwrap_or_else(|| panic!("{name}: the start must fail"));
        assert_eq!(error.0, "Invalid URL", "{name}");

        let output = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let fatal: Vec<&str> = output
            .lines()
            .filter(|line| line.starts_with("{\"level\":60"))
            .collect();
        assert_eq!(fatal.len(), 1, "{name}: {output}");
        assert_eq!(
            normalized(fatal[0]),
            normalized(case["line"].as_str().unwrap()),
            "{name}"
        );
    }
}
