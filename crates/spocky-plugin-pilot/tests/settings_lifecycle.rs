use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_plugin_pilot::{
    PluginSettingsStore, SettingsDefinition, SettingsField, SettingsState, SettingsWriteState,
};

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-plugin-settings-{}-{nonce}-{}",
            std::process::id(),
            TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove test directory: {error}"),
        }
    }
}

fn display(version: u64) -> SettingsDefinition {
    SettingsDefinition::new(
        "display",
        version,
        BTreeMap::from([
            ("enabled".into(), SettingsField::Boolean { default: true }),
            (
                "count".into(),
                SettingsField::Integer {
                    default: 5,
                    minimum: Some(1),
                },
            ),
        ]),
    )
    .expect("valid definition")
}

fn ready(state: SettingsState) -> (Value, String) {
    match state {
        SettingsState::Ready { values, revision } => (values, revision),
        SettingsState::Invalid { error, .. } => panic!("expected ready settings: {error}"),
    }
}

#[test]
fn defaults_atomic_saves_conflicts_restart_and_file_permissions() {
    let root = TestDir::new();
    let mut store = PluginSettingsStore::open(root.path().join("first")).expect("open store");
    store.register(display(1)).expect("register settings");
    assert_eq!(
        store.read("display").expect("read defaults"),
        SettingsState::Ready {
            revision: "missing".into(),
            values: json!({"enabled": true, "count": 5}),
        }
    );

    let saved = store
        .write(
            "display",
            "missing",
            &json!({"enabled": false, "count": 10}),
        )
        .expect("write settings");
    let SettingsWriteState::Saved { revision, values } = saved else {
        panic!("first write must save");
    };
    assert_eq!(values, json!({"enabled": false, "count": 10}));
    assert_eq!(
        store
            .write("display", "missing", &json!({"count": 20}))
            .expect("stale write result"),
        SettingsWriteState::Conflict {
            error: "Settings changed on another client. Reload before saving again.".into(),
        }
    );
    assert_eq!(store.changed_ids(), &["display"]);

    let file = root.path().join("first/display.json");
    assert_eq!(
        fs::read_to_string(&file).expect("read exact envelope"),
        r#"{"version":1,"values":{"count":10,"enabled":false}}"#
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&file)
            .expect("settings metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        fs::read_dir(root.path().join("first"))
            .expect("list settings directory")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
    );

    let mut restarted = PluginSettingsStore::open(root.path().join("first")).expect("restart");
    restarted
        .register(display(1))
        .expect("register after restart");
    let (values, reopened_revision) = ready(restarted.read("display").expect("read restart"));
    assert_eq!(values, json!({"enabled": false, "count": 10}));
    assert_eq!(reopened_revision, revision);
}

#[test]
fn validation_failure_preserves_disk_and_writer_recovers() {
    let root = TestDir::new();
    let mut store = PluginSettingsStore::open(root.path()).expect("open store");
    store.register(display(1)).expect("register settings");
    assert!(matches!(
        store
            .write("display", "missing", &json!({"count": -1}))
            .expect("invalid write result"),
        SettingsWriteState::Invalid { .. }
    ));
    assert!(!root.path().join("display.json").exists());
    assert!(store.changed_ids().is_empty());
    assert!(matches!(
        store
            .write("display", "missing", &json!({"count": 2}))
            .expect("valid write"),
        SettingsWriteState::Saved { .. }
    ));
}

#[test]
fn migration_persists_once_and_old_definition_rejects_newer_schema() {
    let root = TestDir::new();
    let mut old = PluginSettingsStore::open(root.path()).expect("open old store");
    old.register(display(1)).expect("register old definition");
    let SettingsWriteState::Saved {
        revision: old_revision,
        ..
    } = old
        .write("display", "missing", &json!({"count": 7}))
        .expect("save old settings")
    else {
        panic!("old settings must save");
    };

    let mut upgraded = PluginSettingsStore::open(root.path()).expect("open upgraded store");
    upgraded
        .register(display(2).with_migration(|values, version| {
            assert_eq!(version, 1);
            Ok(json!({"enabled": values["enabled"], "count": values["count"]}))
        }))
        .expect("register upgraded definition");
    let (_, upgraded_revision) = ready(upgraded.read("display").expect("migrate"));
    assert_ne!(upgraded_revision, old_revision);
    assert_eq!(upgraded.migration_count("display"), Some(1));
    ready(upgraded.read("display").expect("second read"));
    assert_eq!(upgraded.migration_count("display"), Some(1));
    assert_eq!(upgraded.changed_ids(), &["display"]);

    assert_eq!(
        old.write("display", &old_revision, &json!({"count": 9}))
            .expect("old conflict"),
        SettingsWriteState::Conflict {
            error: "Settings changed on another client. Reload before saving again.".into(),
        }
    );
    let invalid = old
        .read("display")
        .expect("old reader rejects newer schema");
    assert!(matches!(
        invalid,
        SettingsState::Invalid { ref error, .. }
            if error == "Settings were saved by a newer plugin version"
    ));
}

#[test]
fn failed_migration_and_corrupt_data_survive_until_explicit_reset() {
    let root = TestDir::new();
    let mut store = PluginSettingsStore::open(root.path()).expect("open store");
    store.register(display(1)).expect("register settings");
    store
        .write("display", "missing", &json!({"count": 7}))
        .expect("save settings");
    let file = root.path().join("display.json");
    let before = fs::read(&file).expect("read stored settings");

    let mut upgraded = PluginSettingsStore::open(root.path()).expect("open upgrade");
    upgraded
        .register(display(2).with_migration(|_, _| Err("migration failed".into())))
        .expect("register failing migration");
    assert!(matches!(
        upgraded.read("display").expect("failed migration state"),
        SettingsState::Invalid { ref error, .. } if error == "migration failed"
    ));
    assert_eq!(fs::read(&file).expect("migration preserves file"), before);

    fs::write(&file, b"broken JSON").expect("corrupt fixture");
    let invalid = store.read("display").expect("invalid state");
    let SettingsState::Invalid { revision, .. } = invalid else {
        panic!("corrupt settings must be invalid");
    };
    assert_eq!(
        fs::read(&file).expect("corruption preserved"),
        b"broken JSON"
    );
    assert!(matches!(
        store.reset("display", &revision).expect("explicit reset"),
        SettingsWriteState::Saved {
            values,
            ..
        } if values == json!({"enabled": true, "count": 5})
    ));
}

#[test]
fn notifications_are_isolated_and_namespaces_and_definition_ids_stay_separate() {
    let root = TestDir::new();
    let mut first = PluginSettingsStore::open(root.path().join("first")).expect("first store");
    first.register(display(1)).expect("register first");
    first
        .subscribe("display", |state| {
            if let SettingsState::Ready { mut values, .. } = state {
                values["count"] = json!(99);
            }
        })
        .expect("subscribe");
    first
        .write(
            "display",
            "missing",
            &json!({"enabled": false, "count": 10}),
        )
        .expect("save first");
    let (values, _) = ready(first.read("display").expect("read after notification"));
    assert_eq!(values, json!({"enabled": false, "count": 10}));

    let mut second = PluginSettingsStore::open(root.path().join("second")).expect("second store");
    second.register(display(1)).expect("register second");
    assert_eq!(
        ready(second.read("display").expect("read second namespace")).0,
        json!({"enabled": true, "count": 5})
    );
    assert!(second.register(display(1)).is_err());
    assert!(SettingsDefinition::new("../escape", 1, BTreeMap::new()).is_err());
}
