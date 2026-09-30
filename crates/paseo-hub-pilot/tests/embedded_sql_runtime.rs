use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::{
    AccountId, Bootstrap, DurableHubStore, EmbeddedSqlStore, HubPilot, OrganizationId,
    PasswordChange, StoreError, StoreSemantics,
};
use rusqlite::Connection;

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-hub-embedded-sql-{}-{nonce}-{}",
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

    assert_eq!(
        store.relational_tables().expect("relational tables"),
        [
            "account",
            "hub_state",
            "instance_bootstrap",
            "invitation",
            "member",
            "organization",
            "organization_api_keys",
            "runtime_configuration",
            "session",
            "user",
        ]
    );
    let schema = store.schema_observation().expect("schema observation");
    assert!(schema.contains("members_role_check"));
    assert!(schema.contains("invitations_role_check"));
    assert!(schema.contains("invitations_status_check"));
    assert!(schema.contains("invitations_pending_organization_email_unique"));
    assert!(schema.contains("organization_api_keys_prefix_unique"));
    assert!(schema.contains("runtime_configuration_singleton_check"));
    assert_eq!(
        store.schema_constraints().expect("schema constraints"),
        [
            "instance_bootstrap_completion_check",
            "invitations_pending_organization_email_unique",
            "invitations_role_check",
            "invitations_status_check",
            "members_organization_user_unique",
            "members_role_check",
            "organization_api_keys_prefix_unique",
            "organization_api_keys_scopes_check",
            "runtime_configuration_singleton_check",
        ]
    );
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
    assert_eq!(first_journal.len(), 2);
    assert_eq!(first_journal[0].0, 1);
    assert_eq!(first_journal[1].0, 2);
    drop(store);

    let reopened = EmbeddedSqlStore::open(&root.0).expect("reopen upgraded database");
    assert_eq!(
        reopened.migration_journal().expect("reopened journal"),
        first_journal
    );
    assert_eq!(reopened.revision().expect("old revision"), Some(7));
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
        assert!(!lock_path.exists());
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
