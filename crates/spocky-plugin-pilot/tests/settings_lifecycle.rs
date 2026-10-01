use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_plugin_pilot::{
    PluginSettingsStore, SettingsDefinition, SettingsField, SettingsSchema, SettingsState,
    SettingsWriteState,
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

fn rich_settings() -> SettingsDefinition {
    SettingsDefinition::from_schema(
        "rich",
        1,
        SettingsSchema::object([
            (
                "items",
                SettingsSchema::array(SettingsSchema::object([
                    ("name", SettingsSchema::string().min_length(3)),
                    ("weight", SettingsSchema::number().minimum(0.0)),
                ]))
                .min_length(1),
            ),
            ("label", SettingsSchema::string().default(json!("ready"))),
            (
                "mode",
                SettingsSchema::enumeration(["fast", "safe"]).default(json!("safe")),
            ),
        ])
        .refine_async(|value| async move {
            if value["items"][0]["name"] == "bad" {
                Err("name rejected asynchronously".into())
            } else {
                Ok(())
            }
        }),
    )
    .expect("valid rich definition")
}

#[test]
fn arbitrary_json_schema_defaults_validation_and_async_refinement_match_baseline() {
    let root = TestDir::new();
    let mut store = PluginSettingsStore::open(root.path()).expect("open store");
    store.register(rich_settings()).expect("register settings");

    assert_eq!(
        store
            .write(
                "rich",
                "missing",
                &json!({"items": [{"name": "valid", "weight": 1.5, "ignored": true}], "extra": true}),
            )
            .expect("write rich settings"),
        SettingsWriteState::Saved {
            revision: "5b0e432217a76e07160ccf72afdf410b1e4172c8d0ba3e9bd52830fc6669cade".into(),
            values: json!({
                "items": [{"name": "valid", "weight": 1.5}],
                "label": "ready",
                "mode": "safe",
            }),
        }
    );

    let (_, revision) = ready(store.read("rich").expect("read rich settings"));
    assert_eq!(
        store
            .write(
                "rich",
                &revision,
                &json!({"items": [{"name": "x", "weight": 1.5}]}),
            )
            .expect("invalid string"),
        SettingsWriteState::Invalid {
            error: "Too small: expected string to have >=3 characters".into(),
        }
    );
    assert_eq!(
        store
            .write(
                "rich",
                &revision,
                &json!({"items": [{"name": "valid", "weight": -1}]}),
            )
            .expect("invalid number"),
        SettingsWriteState::Invalid {
            error: "Too small: expected number to be >=0".into(),
        }
    );
    assert_eq!(
        store
            .write(
                "rich",
                &revision,
                &json!({"items": [{"name": "bad", "weight": 1}]}),
            )
            .expect("failed async refinement"),
        SettingsWriteState::Invalid {
            error: "name rejected asynchronously".into(),
        }
    );
}

#[test]
fn enum_array_and_required_object_errors_match_baseline() {
    let root = TestDir::new();
    let mut store = PluginSettingsStore::open(root.path()).expect("open store");
    store.register(rich_settings()).expect("register settings");

    for (values, expected) in [
        (
            json!({"items": []}),
            "Too small: expected array to have >=1 items",
        ),
        (
            json!({"items": [{"name": "valid", "weight": 1}], "mode": "other"}),
            "Invalid option: expected one of \"fast\"|\"safe\"",
        ),
        (
            json!({"items": [{"weight": 1}]}),
            "Invalid input: expected string, received undefined",
        ),
        (
            json!({"items": [{"name": "x", "weight": -1}], "mode": "other"}),
            "Too small: expected string to have >=3 characters\nToo small: expected number to be >=0\nInvalid option: expected one of \"fast\"|\"safe\"",
        ),
    ] {
        assert_eq!(
            store
                .write("rich", "missing", &values)
                .expect("validation result"),
            SettingsWriteState::Invalid {
                error: expected.into(),
            }
        );
    }
}

#[test]
fn integer_errors_match_baseline() {
    let root = TestDir::new();
    let mut store = PluginSettingsStore::open(root.path()).expect("open store");
    store.register(display(1)).expect("register settings");

    for (values, expected) in [
        (json!({"count": -1}), "Too small: expected number to be >=1"),
        (
            json!({"count": 1.5}),
            "Invalid input: expected int, received number",
        ),
        (
            json!({"count": "1"}),
            "Invalid input: expected number, received string",
        ),
    ] {
        assert_eq!(
            store
                .write("display", "missing", &values)
                .expect("integer validation result"),
            SettingsWriteState::Invalid {
                error: expected.into(),
            }
        );
    }
}

#[test]
fn listener_failures_are_reported_without_failing_saved_write() {
    let root = TestDir::new();
    let reported = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&reported);
    let mut store = PluginSettingsStore::open_with_error_reporter(root.path(), move |error| {
        captured.lock().expect("lock reports").push(error);
    })
    .expect("open store");
    store.register(rich_settings()).expect("register settings");
    store
        .subscribe_async("rich", |_| async { Err("subscriber rejected".into()) })
        .expect("subscribe async listener");

    assert!(matches!(
        store
            .write(
                "rich",
                "missing",
                &json!({"items": [{"name": "valid", "weight": 1}]})
            )
            .expect("write despite listener failure"),
        SettingsWriteState::Saved { .. }
    ));
    assert_eq!(
        *reported.lock().expect("lock reports"),
        ["Plugin settings subscriber failed for rich: subscriber rejected"]
    );
}

fn write_result_json(result: SettingsWriteState) -> Value {
    match result {
        SettingsWriteState::Saved { values, revision } => {
            json!({"status": "saved", "values": values, "revision": revision})
        }
        SettingsWriteState::Conflict { error } => {
            json!({"status": "conflict", "error": error})
        }
        SettingsWriteState::Invalid { error } => json!({"status": "invalid", "error": error}),
    }
}

#[test]
fn differential_capture_matches_pinned_settings_cases() {
    let root = TestDir::new();
    let mut cases = Vec::new();
    for (name, values) in [
        ("array", json!({"items": []})),
        (
            "enum",
            json!({"items": [{"name": "valid", "weight": 1}], "mode": "other"}),
        ),
        ("required", json!({"items": [{"weight": 1}]})),
        ("string", json!({"items": [{"name": "x", "weight": 1.5}]})),
        (
            "number",
            json!({"items": [{"name": "valid", "weight": -1}]}),
        ),
        (
            "refinement",
            json!({"items": [{"name": "bad", "weight": 1}]}),
        ),
        (
            "multiple",
            json!({"items": [{"name": "x", "weight": -1}], "mode": "other"}),
        ),
    ] {
        let mut store = PluginSettingsStore::open(root.path().join(name)).expect("open store");
        store.register(rich_settings()).expect("register settings");
        let result = store
            .write("rich", "missing", &values)
            .expect("capture write result");
        cases.push(json!({"name": name, "result": write_result_json(result)}));
    }
    for (name, value) in [
        ("integer-minimum", json!(-1)),
        ("integer-fraction", json!(1.5)),
        ("integer-type", json!("1")),
    ] {
        let mut store = PluginSettingsStore::open(root.path().join(name)).expect("open store");
        store.register(display(1)).expect("register settings");
        let result = store
            .write("display", "missing", &json!({"count": value}))
            .expect("capture integer result");
        cases.push(json!({"name": name, "result": write_result_json(result)}));
    }

    let reports = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&reports);
    let mut store =
        PluginSettingsStore::open_with_error_reporter(root.path().join("saved"), move |error| {
            captured.lock().expect("lock reports").push(error);
        })
        .expect("open callback store");
    store.register(rich_settings()).expect("register settings");
    store
        .subscribe_async("rich", |_| async { Err("subscriber rejected".into()) })
        .expect("subscribe callback");
    let result = store
        .write(
            "rich",
            "missing",
            &json!({"items": [{"name": "valid", "weight": 1.5, "ignored": true}], "extra": true}),
        )
        .expect("capture saved result");
    cases.push(json!({
        "name": "saved",
        "reports": reports.lock().expect("lock reports").clone(),
        "result": write_result_json(result),
    }));

    println!(
        "PLUGIN_SETTINGS_RUST {}",
        serde_json::to_string(&cases).expect("serialize capture")
    );
}
