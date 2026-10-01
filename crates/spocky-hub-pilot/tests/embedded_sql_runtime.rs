use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use spocky_hub_pilot::{
    AccountId, Bootstrap, DurableHubStore, EmbeddedSqlStore, HubPilot, OrganizationId,
    PasswordChange, StoreError, StoreSemantics,
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
            "spocky-hub-embedded-sql-{}-{nonce}-{}",
            std::process::id(),
            TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
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

#[test]
fn embedded_sql_schema_and_hub_state_survive_restart() {
    let root = TestDir::new();
    let owner = AccountId::from("owner@example.test");
    let organization = OrganizationId::from("organization-1");
    let store = EmbeddedSqlStore::open(&root.0).expect("open embedded SQL");
    assert_eq!(
        EmbeddedSqlStore::SEMANTICS,
        StoreSemantics::EmbeddedSqlTransactionalSnapshot
    );
    assert!(
        store
            .schema_sql()
            .contains("CREATE TABLE IF NOT EXISTS hub_state")
    );
    assert!(store.schema_sql().contains("CHECK (singleton = 1)"));
    assert_eq!(store.revision().expect("initial revision"), None);

    let mut hub = HubPilot::open(store).expect("open Hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "embedded-sql-secret-at-least-32-characters".into(),
        owner: owner.clone(),
        organization: organization.clone(),
        temporary_password: "temporary-password".into(),
    })
    .expect("bootstrap");
    hub.replace_password(&PasswordChange {
        account: owner.clone(),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .expect("replace password");
    drop(hub);

    let store = EmbeddedSqlStore::open(&root.0).expect("reopen embedded SQL");
    assert_eq!(store.revision().expect("persisted revision"), Some(2));
    let restarted = HubPilot::open(store).expect("restart Hub");
    assert!(restarted.authorize(&owner, &organization).is_ok());
}

#[test]
fn embedded_sql_transactions_commit_and_roll_back() {
    let root = TestDir::new();
    let store = EmbeddedSqlStore::open(&root.0).expect("open embedded SQL");
    store
        .execute_batch("CREATE TABLE probe (value TEXT NOT NULL UNIQUE)")
        .expect("create probe table");
    store
        .transaction(|transaction| {
            transaction.execute("INSERT INTO probe (value) VALUES ('kept')", [])?;
            Ok(())
        })
        .expect("commit insert");
    let rejected: Result<(), StoreError> = store.transaction(|transaction| {
        transaction.execute("INSERT INTO probe (value) VALUES ('rolled-back')", [])?;
        Err(StoreError::TransactionAborted)
    });
    assert!(matches!(rejected, Err(StoreError::TransactionAborted)));
    assert_eq!(
        store
            .query_text_column("SELECT value FROM probe ORDER BY value")
            .expect("query probe"),
        vec!["kept"]
    );
}

#[test]
fn embedded_sql_rejects_a_second_live_directory_owner() {
    let root = TestDir::new();
    let first = EmbeddedSqlStore::open(&root.0).expect("open first owner");

    let second = EmbeddedSqlStore::open(&root.0);
    assert!(matches!(second, Err(StoreError::EmbeddedDirectoryInUse(_))));

    drop(first);
    EmbeddedSqlStore::open(&root.0).expect("lock releases on close");
}

#[test]
fn embedded_sql_serializes_concurrent_state_transactions() {
    let root = TestDir::new();
    let store = Arc::new(EmbeddedSqlStore::open(&root.0).expect("open embedded SQL"));
    let writers = [b"writer-a".to_vec(), b"writer-b".to_vec()].map(|bytes| {
        let store = Arc::clone(&store);
        thread::spawn(move || store.save(&bytes).expect("save state"))
    });
    for writer in writers {
        writer.join().expect("join writer");
    }

    assert_eq!(store.revision().expect("revision"), Some(2));
    let bytes = store.load().expect("load state").expect("state exists");
    assert!(bytes == b"writer-a" || bytes == b"writer-b");
    assert!(EmbeddedSqlStore::LIMITATIONS.contains("not PGlite"));
    assert!(EmbeddedSqlStore::LIMITATIONS.contains("whole-state snapshot"));
}

#[test]
fn embedded_sql_serializes_callers_holding_the_same_key() {
    let root = TestDir::new();
    let store = Arc::new(EmbeddedSqlStore::open(&root.0).expect("open embedded SQL"));
    let first_entered = Arc::new(Barrier::new(2));
    let release_first = Arc::new(Barrier::new(2));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));

    let first = {
        let store = Arc::clone(&store);
        let first_entered = Arc::clone(&first_entered);
        let release_first = Arc::clone(&release_first);
        let events = Arc::clone(&events);
        thread::spawn(move || {
            store
                .with_lock("shared", || {
                    events.lock().expect("events").push("first:start");
                    first_entered.wait();
                    release_first.wait();
                    events.lock().expect("events").push("first:end");
                    Ok(())
                })
                .expect("first lock");
        })
    };
    first_entered.wait();
    let second = {
        let store = Arc::clone(&store);
        let events = Arc::clone(&events);
        thread::spawn(move || {
            store
                .with_lock("shared", || {
                    events.lock().expect("events").push("second:start");
                    events.lock().expect("events").push("second:end");
                    Ok(())
                })
                .expect("second lock");
        })
    };
    thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(*events.lock().expect("events"), ["first:start"]);
    release_first.wait();
    first.join().expect("join first");
    second.join().expect("join second");
    assert_eq!(
        *events.lock().expect("events"),
        ["first:start", "first:end", "second:start", "second:end"]
    );
}

#[test]
fn embedded_sql_installs_relational_hub_tables_and_constraints() {
    let root = TestDir::new();
    let store = EmbeddedSqlStore::open(&root.0).expect("open embedded SQL");

    let tables = store.relational_tables().expect("relational tables");
    assert_eq!(tables.len(), 50);
    for expected in [
        "account",
        "agent_executions",
        "attachment_capabilities",
        "billing_plan_prices",
        "cli_authorizations",
        "configuration_sync_attempts",
        "daemons",
        "execution_authorities",
        "github_connections",
        "organization_trigger_revisions",
        "project_configuration_revisions",
        "provider_event_receipts",
        "trigger_runs",
        "workflow_step_runs",
        "workflow_wakeups",
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "missing {expected}"
        );
    }
    assert!(tables.iter().any(|table| table == "hub_state"));
    let schema = store.schema_observation().expect("schema observation");
    assert!(schema.contains("members_role_check"));
    assert!(schema.contains("invitations_role_check"));
    assert!(schema.contains("invitations_status_check"));
    assert!(schema.contains("invitations_pending_organization_email_unique"));
    assert!(schema.contains("organization_api_keys_prefix_unique"));
    assert!(schema.contains("runtime_configuration_singleton_check"));
    for expected in [
        "agent_executions_daemon_organization_fk",
        "agent_executions_hub_action_check",
        "agent_executions_project_started_at_idx",
        "cli_authorizations_user_code_verifier_unique",
        "organization_connection_attempts_shape_check",
        "project_configuration_revisions_project_organization_fk",
        "provider_event_receipts_organization_delivery_unique",
        "trigger_runs_status_check",
        "workflow_step_runs_trigger_step_unique",
    ] {
        assert!(schema.contains(expected), "missing {expected}");
    }
    assert_eq!(
        store
            .schema_constraints()
            .expect("schema constraints")
            .len(),
        209
    );
}

#[test]
fn embedded_sql_schema_inventory_comes_from_installed_objects() {
    let root = TestDir::new();
    let store = EmbeddedSqlStore::open(&root.0).expect("open embedded SQL");
    store
        .execute_batch(
            "CREATE TABLE installed_probe (
                id INTEGER PRIMARY KEY,
                value TEXT NOT NULL,
                CONSTRAINT installed_probe_value_check CHECK (length(value) > 0)
            );
            CREATE UNIQUE INDEX installed_probe_value_unique ON installed_probe (value);",
        )
        .expect("install probe schema");

    assert!(
        store
            .baseline_tables()
            .expect("installed tables")
            .contains(&"public.installed_probe".to_owned())
    );
    let constraints = store.schema_constraints().expect("installed constraints");
    assert!(constraints.contains(&"installed_probe_value_check".to_owned()));
    assert!(constraints.contains(&"installed_probe_value_unique".to_owned()));
}

#[test]
fn embedded_sql_migrates_old_database_once_and_reopens_at_latest_version() {
    let root = TestDir::new();
    let database = root.0.join("hub.sqlite3");
    let connection = Connection::open(&database).expect("open old database");
    connection
        .execute_batch(
            "CREATE TABLE hub_state (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                state_bytes BLOB NOT NULL,
                revision INTEGER NOT NULL CHECK (revision > 0)
            );
            INSERT INTO hub_state (singleton, state_bytes, revision)
            VALUES (1, X'6f6c642d7374617465', 7);",
        )
        .expect("create old database");
    drop(connection);

    let store = EmbeddedSqlStore::open(&root.0).expect("upgrade old database");
    assert_eq!(
        store.load().expect("load old state"),
        Some(b"old-state".to_vec())
    );
    let first_journal = store.migration_journal().expect("migration journal");
    assert_eq!(first_journal.len(), 49);
    assert_eq!(first_journal[0], (0, "0000_phase_0_spine".into()));
    assert_eq!(first_journal[48], (48, "0048_execution_authority".into()));
    drop(store);

    let reopened = EmbeddedSqlStore::open(&root.0).expect("reopen upgraded database");
    assert_eq!(
        reopened.migration_journal().expect("reopened journal"),
        first_journal
    );
    assert_eq!(reopened.revision().expect("old revision"), Some(7));
}

#[test]
fn embedded_sql_resumes_an_interrupted_schema_install() {
    let root = TestDir::new();
    let database = root.0.join("hub.sqlite3");
    let connection = Connection::open(&database).expect("open interrupted database");
    connection
        .execute_batch(
            "CREATE TABLE paseo_hub_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at INTEGER NOT NULL
            );
            INSERT INTO paseo_hub_migrations (version, name, applied_at)
            VALUES (0, '0000_phase_0_spine', 1784319580564);",
        )
        .expect("create interrupted database");
    drop(connection);

    let store = EmbeddedSqlStore::open(&root.0).expect("resume interrupted migration");
    assert_eq!(
        store.migration_journal().expect("migration journal").len(),
        49
    );
    assert_eq!(store.relational_tables().expect("tables").len(), 50);
    let schema = store.schema_observation().expect("schema");
    assert!(schema.contains("workflow_step_runs_trigger_step_unique"));
}

#[test]
fn embedded_sql_rejects_non_prefix_migration_history_without_rewriting_it() {
    let root = TestDir::new();
    let database = root.0.join("hub.sqlite3");
    let connection = Connection::open(&database).expect("open malformed migration database");
    connection
        .execute_batch(
            "CREATE TABLE paseo_hub_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at INTEGER NOT NULL
            );
            INSERT INTO paseo_hub_migrations (version, name, applied_at)
            VALUES (1, '0001_charming_sabretooth', 1784401968975);",
        )
        .expect("create malformed migration history");
    drop(connection);

    let Err(error) = EmbeddedSqlStore::open(&root.0) else {
        panic!("accepted non-prefix journal");
    };
    assert!(error.to_string().contains("migration journal mismatch"));

    let connection = Connection::open(&database).expect("reopen malformed migration database");
    let row = connection
        .query_row(
            "SELECT version, name, applied_at FROM paseo_hub_migrations",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("preserved malformed row");
    assert_eq!(
        row,
        (1, "0001_charming_sabretooth".into(), 1_784_401_968_975)
    );
}

#[test]
fn embedded_sql_rolls_back_an_interrupted_migration_transaction() {
    let root = TestDir::new();
    let database = root.0.join("hub.sqlite3");
    let connection = Connection::open(&database).expect("open conflicting database");
    connection
        .execute_batch(
            "CREATE TABLE paseo_hub_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at INTEGER NOT NULL
            );
            CREATE TRIGGER interrupt_migration
            BEFORE INSERT ON paseo_hub_migrations
            WHEN NEW.version = 2
            BEGIN
                SELECT RAISE(ABORT, 'simulated migration interruption');
            END;",
        )
        .expect("create migration interruption trigger");
    drop(connection);

    assert!(EmbeddedSqlStore::open(&root.0).is_err());

    let connection = Connection::open(&database).expect("reopen conflicting database");
    let objects = connection
        .prepare("SELECT type || ':' || name FROM sqlite_master ORDER BY type, name")
        .expect("prepare object inventory")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query object inventory")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect object inventory");
    assert_eq!(
        objects,
        [
            "index:sqlite_autoindex_paseo_hub_migrations_1",
            "table:paseo_hub_migrations",
            "trigger:interrupt_migration"
        ]
    );
    let journal_rows = connection
        .query_row("SELECT count(*) FROM paseo_hub_migrations", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count rolled-back journal");
    assert_eq!(journal_rows, 0);
}

#[test]
fn embedded_sql_recovers_stale_and_incomplete_owner_records() {
    for owner in [
        r#"{"pid":2147483647,"token":"dead-owner"}"#,
        r#"{"pid":123"#,
    ] {
        let root = TestDir::new();
        let lock_path = root.0.join(".paseo-hub.lock");
        let mut lock = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
            .expect("create stale lock");
        lock.write_all(owner.as_bytes()).expect("write stale owner");
        drop(lock);

        let store = EmbeddedSqlStore::open(&root.0).expect("recover stale lock");
        let current = fs::read_to_string(&lock_path).expect("current owner record");
        assert!(current.contains(&format!(r#""pid":{}"#, std::process::id())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            assert_eq!(
                fs::metadata(&lock_path)
                    .expect("lock metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(store);
        assert!(lock_path.exists());
        EmbeddedSqlStore::open(&root.0).expect("reopen released OS lock");
    }
}

#[test]
fn embedded_sql_rejects_live_owner_record_before_opening_database() {
    let root = TestDir::new();
    let lock_path = root.0.join(".paseo-hub.lock");
    fs::write(
        &lock_path,
        format!(
            r#"{{"pid":{},"token":"other-live-owner"}}"#,
            std::process::id()
        ),
    )
    .expect("write live lock");

    assert!(matches!(
        EmbeddedSqlStore::open(&root.0),
        Err(StoreError::EmbeddedDirectoryInUse(path)) if path == root.0
    ));
    assert!(!root.0.join("hub.sqlite3").exists());
}
