//! The fatal record and the stderr text of a daemon that cannot listen, against
//! the pinned original (`tests/fixtures/original-log-vectors.json`, from
//! `gen-original-log-vectors.cjs`): the worker's stdout line `Daemon failed to
//! start listening` byte for byte and the stack Node prints. Only the time, pid,
//! host name and the port are masked, as values the run decides.
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

/// The record with the values the run decides replaced by fixed ones.
fn mask(line: &str, hostname: &str, port: u16) -> String {
    let mut text = line
        .replace(hostname, "HOST")
        .replace(&port.to_string(), "PORT");
    for key in ["\"time\":", "\"pid\":"] {
        let start = text.find(key).expect(key) + key.len();
        let end = start + text[start..].find(',').unwrap();
        text.replace_range(start..end, "N");
    }
    text
}

#[test]
fn a_failed_listen_logs_and_prints_what_the_original_does() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/original-log-vectors.json")).unwrap();
    common::assert_fixture_provenance(&fixture);
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let host = case["host"].as_str().unwrap();
        let captured_port = u16::try_from(case["port"].as_u64().unwrap()).unwrap();
        // The port is taken by a holder here, as it was when captured; the
        // other case needs a free one.
        let holder = (name == "port_in_use").then(|| TcpListener::bind("127.0.0.1:0").unwrap());
        let port = holder.as_ref().map_or_else(
            || {
                TcpListener::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap()
                    .port()
            },
            |holder| holder.local_addr().unwrap().port(),
        );
        common::assert_disposable_listen(&format!("{host}:{port}"));

        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.json"),
            json!({"daemon": {"listen": format!("{host}:{port}")}}).to_string(),
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
            mask(fatal[0], &hostname, port),
            mask(
                case["fatalLine"].as_str().unwrap(),
                &hostname,
                captured_port
            ),
            "{name}: the fatal record"
        );
        let stderr = format!("{}\n", failure_text(&error));
        assert_eq!(
            stderr.replace(&port.to_string(), "PORT"),
            case["workerStderr"]
                .as_str()
                .unwrap()
                .replace(&captured_port.to_string(), "PORT"),
            "{name}: stderr"
        );
        drop(holder);
    }
}
