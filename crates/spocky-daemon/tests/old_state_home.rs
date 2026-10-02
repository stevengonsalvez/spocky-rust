//! A home the pinned original daemon made and left behind (captured by
//! `gen-original-home-vectors.cjs` into `tests/fixtures/original-home-vectors.json`)
//! opened by this daemon: after a graceful stop and after a SIGKILL. The
//! identity, key pair and config files must be kept byte for byte, a stale
//! lock must be replaced, and a stop must leave the files the original leaves.

mod common;

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use spocky_daemon::daemon::{DaemonEnv, NoSessionBackend, start};
use spocky_daemon::log::{Logger, NullLogger};

fn fixture() -> Value {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/original-home-vectors.json")).unwrap();
    assert_eq!(fixture["node"], "v22.20.0");
    common::assert_fixture_provenance(&fixture);
    fixture
}

/// Writes the captured files (not directories) into a fresh home.
fn restore(home: &Path, files: &Value) {
    fs::create_dir_all(home).unwrap();
    fs::set_permissions(home, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, file) in files.as_object().unwrap() {
        let Some(text) = file["text"].as_str() else {
            continue;
        };
        let path = home.join(name);
        fs::write(&path, text).unwrap();
        let mode = u32::from_str_radix(file["mode"].as_str().unwrap(), 8).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn env(home: &Path) -> DaemonEnv {
    let listen = "127.0.0.1:0";
    common::assert_disposable_listen(listen);
    let vars: HashMap<String, String> = HashMap::from([
        ("PASEO_HOME".to_owned(), home.display().to_string()),
        ("PASEO_LISTEN".to_owned(), listen.to_owned()),
    ]);
    DaemonEnv::new(
        vars,
        PathBuf::from("/"),
        Some(PathBuf::from("/Users/example")),
    )
}

fn read(home: &Path, name: &str) -> String {
    fs::read_to_string(home.join(name)).unwrap()
}

fn open_and_stop(state: &Value) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let files = &state["after"];
    restore(&home, files);
    let kept = ["config.json", "daemon-keypair.json", "server-id"];
    let original_server_id = read(&home, "server-id");

    let logger: Arc<dyn Logger> = Arc::new(NullLogger);
    let daemon = start(&env(&home), Arc::new(NoSessionBackend), &logger).unwrap();
    // The identity and key pair of the original home are the daemon's own.
    assert_eq!(
        format!("{}\n", daemon.server_id()),
        original_server_id,
        "server id"
    );
    for name in kept {
        assert_eq!(
            read(&home, name),
            files[name]["text"].as_str().unwrap(),
            "{name}"
        );
    }
    // A stale lock is replaced by this process's.
    let lock: Value = serde_json::from_str(&read(&home, "paseo.pid")).unwrap();
    assert_eq!(lock["pid"], std::process::id());
    assert_eq!(lock["serverId"], daemon.server_id());
    assert_eq!(lock["listen"], daemon.listen());
    // The files a running daemon keeps look as the original's do: same modes,
    // and a lock with the same keys in the same order.
    let running = &state["listening"];
    for name in ["paseo.pid", "local-credential"] {
        let mode = fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            format!("{mode:o}"),
            running[name]["mode"].as_str().unwrap(),
            "{name} mode"
        );
    }
    let keys = |lock: &Value| {
        lock.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    let original_lock: Value =
        serde_json::from_str(running["paseo.pid"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(keys(&lock), keys(&original_lock));

    daemon.stop();
    assert!(!home.join("paseo.pid").exists(), "the lock is released");
    assert!(
        !home.join("local-credential").exists(),
        "the credential is removed"
    );
    for name in kept {
        assert_eq!(
            read(&home, name),
            files[name]["text"].as_str().unwrap(),
            "{name}"
        );
        let mode = fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            format!("{mode:o}"),
            files[name]["mode"].as_str().unwrap(),
            "{name} mode"
        );
    }
}

#[test]
fn a_home_the_original_stopped_gracefully_opens_and_keeps_its_identity() {
    open_and_stop(&fixture()["graceful"]);
}

#[test]
fn a_home_the_original_left_after_sigkill_opens_over_its_stale_lock() {
    let fixture = fixture();
    let killed = &fixture["killed"];
    // The captured lock names a process that is gone.
    assert!(killed["after"]["paseo.pid"]["text"].is_string());
    open_and_stop(killed);
}
