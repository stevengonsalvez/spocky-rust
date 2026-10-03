//! The fatal record and the stderr text of a daemon that cannot listen, against
//! the pinned original (`tests/fixtures/original-log-vectors.json`, from
//! `gen-original-log-vectors.cjs`): the worker's stdout line `Daemon failed to
//! start listening` and the stack Node prints, for a taken port, an address that is
//! not the host's, a port out of range, a host that does not resolve and a unix
//! socket in a missing directory.
//!
//! Masked, because the run decides them: the time, pid, host name and the port
//! of a listen string with a "PORT" placeholder. The stack lines that name the
//! original's own build (`file:///.../bootstrap.js`) and everything below them are
//! cut from both sides; Spocky has no such files.
//!
//! The captured errno and stack lines are macOS ones, so the comparison runs
//! only there.
#![cfg(target_os = "macos")]

mod common;

use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use spocky_daemon::daemon::{DaemonEnv, NoSessionBackend, start};
use spocky_daemon::log::Logger;
use spocky_daemon::process::{daemon_logger, failure_text};

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

/// Everything before the first stack line that names the original's own files.
fn without_build_frames(text: &str) -> String {
    let cut = text
        .find("\n    at file:///")
        .or_else(|| text.find("    at file:///"));
    cut.map_or_else(|| text.to_owned(), |at| text[..at].to_owned())
}

/// The record's key order, the `err` object's key order, and the record with
/// the values the run decides replaced by fixed ones.
fn normalized(line: &str, port: Option<u16>) -> (Vec<String>, Vec<String>, Value) {
    let mut record: Value = serde_json::from_str(line).unwrap();
    let keys = |value: &Value| value.as_object().unwrap().keys().cloned().collect();
    let record_keys = keys(&record);
    let err_keys = keys(&record["err"]);
    for key in ["time", "pid", "hostname"] {
        record[key] = json!("N");
    }
    let err = record["err"].as_object_mut().unwrap();
    for key in ["message", "stack"] {
        let mut text = err[key].as_str().unwrap().to_owned();
        if let Some(port) = port {
            text = text.replace(&port.to_string(), "PORT");
        }
        err[key] = json!(without_build_frames(&text));
    }
    if let Some(port) = port
        && err.get("port") == Some(&json!(port))
    {
        err["port"] = json!("PORT");
    }
    (record_keys, err_keys, record)
}

#[test]
fn a_failed_listen_logs_and_prints_what_the_original_does() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/original-log-vectors.json")).unwrap();
    common::assert_fixture_provenance(&fixture);
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 6);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let template = case["listen"].as_str().unwrap();
        let captured_port = case["port"]
            .as_u64()
            .filter(|_| template.contains("PORT"))
            .map(|port| u16::try_from(port).unwrap());
        // The taken-port case holds a port here, as it was held when captured.
        let holder = (name == "port_in_use").then(|| TcpListener::bind("127.0.0.1:0").unwrap());
        let port = captured_port.map(|_| {
            holder.as_ref().map_or_else(
                || {
                    TcpListener::bind("127.0.0.1:0")
                        .unwrap()
                        .local_addr()
                        .unwrap()
                        .port()
                },
                |holder| holder.local_addr().unwrap().port(),
            )
        });
        let listen = port.map_or_else(
            || template.to_owned(),
            |port| template.replace("PORT", &port.to_string()),
        );
        common::assert_disposable_listen(&listen);

        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.json"),
            json!({"daemon": {"listen": listen}}).to_string(),
        )
        .unwrap();
        let env = DaemonEnv::new(
            HashMap::from([("PASEO_HOME".to_owned(), home.display().to_string())]),
            PathBuf::from("/"),
            Some(PathBuf::from("/Users/example")),
        );
        let sink = Sink::default();
        let logger: Arc<dyn Logger> = Arc::new(daemon_logger(sink.clone()));
        let error = start(&env, Arc::new(NoSessionBackend), &logger)
            .err()
            .unwrap_or_else(|| panic!("{name}: the start must fail"));

        let output = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let fatal: Vec<&str> = output
            .lines()
            .filter(|line| line.starts_with("{\"level\":60"))
            .collect();
        assert_eq!(fatal.len(), 1, "{name}: {output}");
        assert_eq!(
            normalized(fatal[0], port),
            normalized(case["fatalLine"].as_str().unwrap(), captured_port),
            "{name}: the fatal record"
        );
        let mask = |text: &str, port: Option<u16>| {
            let text = port.map_or_else(
                || text.to_owned(),
                |port| text.replace(&port.to_string(), "PORT"),
            );
            without_build_frames(&text)
        };
        let stderr = format!("{}\n", failure_text(&error));
        assert_eq!(
            mask(&stderr, port).trim_end(),
            mask(case["workerStderr"].as_str().unwrap(), captured_port).trim_end(),
            "{name}: stderr"
        );
        drop(holder);
    }
}
