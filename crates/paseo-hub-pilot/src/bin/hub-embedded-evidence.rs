use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use paseo_hub_pilot::{EmbeddedSqlStore, StoreError};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let operation = arguments.next().ok_or("operation is required")?;
    let data_directory = arguments.next().ok_or("data directory is required")?;
    if arguments.next().is_some() {
        return Err("unexpected argument".into());
    }
    if operation == "probe-open" {
        return match EmbeddedSqlStore::open(&data_directory) {
            Ok(_) => Ok(()),
            Err(StoreError::EmbeddedDirectoryInUse(_)) => std::process::exit(23),
            Err(error) => Err(error.into()),
        };
    }
    if operation != "capture" {
        return Err(format!("unknown operation: {operation}").into());
    }
    capture(Path::new(&data_directory))
}

fn capture(data_directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let store = EmbeddedSqlStore::open(data_directory)?;
    store.execute_batch(
        "CREATE TABLE differential_probe (value TEXT NOT NULL UNIQUE);
         INSERT INTO differential_probe (value) VALUES ('kept');",
    )?;
    drop(store);
    let store = Arc::new(EmbeddedSqlStore::open(data_directory)?);
    let restart = store.query_text_column("SELECT value FROM differential_probe")? == ["kept"];

    let child = Command::new(env::current_exe()?)
        .arg("probe-open")
        .arg(data_directory)
        .output()?;
    let cross_process_rejection = child.status.code() == Some(23);

    let rollback_result: Result<(), StoreError> = store.transaction(|transaction| {
        transaction.execute(
            "INSERT INTO differential_probe (value) VALUES ('rolled-back')",
            [],
        )?;
        Err(StoreError::TransactionAborted)
    });
    let transaction_rollback = matches!(rollback_result, Err(StoreError::TransactionAborted))
        && store
            .query_text_column("SELECT value FROM differential_probe WHERE value = 'rolled-back'")?
            .is_empty();

    let same_key_serialization = capture_same_key_serialization(&store)?;
    let tables = store.relational_tables()?;
    let constraints = store.schema_constraints()?;
    let canonical_tables = store.baseline_tables()?;
    let canonical_constraints = constraints.clone();
    let migration_journal = store
        .migration_journal()?
        .into_iter()
        .map(|(version, name)| json!({ "version": version, "name": name }))
        .collect::<Vec<_>>();
    let lock_owner = serde_json::from_slice::<serde_json::Value>(&fs::read(
        data_directory.join(".paseo-hub.lock"),
    )?)?;
    let mut lock_owner_keys = lock_owner
        .as_object()
        .ok_or("lock owner is not an object")?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    lock_owner_keys.sort();
    drop(store);

    let lock_path = data_directory.join(".paseo-hub.lock");
    fs::write(&lock_path, r#"{"pid":2147483647,"token":"stale"}"#)?;
    let stale_owner_recovery = EmbeddedSqlStore::open(data_directory).is_ok();
    fs::write(&lock_path, r#"{"pid":123"#)?;
    let incomplete_owner_recovery = EmbeddedSqlStore::open(data_directory).is_ok();
    let reopened = EmbeddedSqlStore::open(data_directory)?;
    let reopened_journal = reopened
        .migration_journal()?
        .into_iter()
        .map(|(version, name)| json!({ "version": version, "name": name }))
        .collect::<Vec<_>>();
    drop(reopened);
    let output = json!({
        "operations": {
            "restart": restart,
            "crossProcessRejection": cross_process_rejection,
            "transactionRollback": transaction_rollback,
            "sameKeySerialization": same_key_serialization,
            "staleOwnerRecovery": stale_owner_recovery && incomplete_owner_recovery,
        },
        "observations": {
            "tables": tables,
            "constraints": constraints,
            "canonicalTables": canonical_tables,
            "canonicalConstraints": canonical_constraints,
            "migrationJournal": migration_journal,
            "migrationReopenStable": migration_journal == reopened_journal,
            "lockOwnerKeys": lock_owner_keys,
        },
        "boundary": {
            "engine": "SQLite",
            "schema": "baseline-owned relational schema plus snapshot compatibility shim",
            "dialect": "SQLite",
            "migrations": "baseline journal representation over idempotent final schema",
        },
    });
    println!("{output}");
    Ok(())
}

fn capture_same_key_serialization(
    store: &Arc<EmbeddedSqlStore>,
) -> Result<Vec<&'static str>, StoreError> {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let events = Arc::new(Mutex::new(Vec::new()));
    let first = {
        let store = Arc::clone(store);
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let events = Arc::clone(&events);
        thread::spawn(move || {
            store.with_lock("shared", || {
                events
                    .lock()
                    .map_err(|_| StoreError::Poisoned)?
                    .push("first:start");
                entered.wait();
                release.wait();
                events
                    .lock()
                    .map_err(|_| StoreError::Poisoned)?
                    .push("first:end");
                Ok(())
            })
        })
    };
    entered.wait();
    let second = {
        let store = Arc::clone(store);
        let events = Arc::clone(&events);
        thread::spawn(move || {
            store.with_lock("shared", || {
                events
                    .lock()
                    .map_err(|_| StoreError::Poisoned)?
                    .push("second:start");
                events
                    .lock()
                    .map_err(|_| StoreError::Poisoned)?
                    .push("second:end");
                Ok(())
            })
        })
    };
    thread::sleep(Duration::from_millis(25));
    release.wait();
    first.join().map_err(|_| StoreError::Poisoned)??;
    second.join().map_err(|_| StoreError::Poisoned)??;
    Arc::try_unwrap(events)
        .map_err(|_| StoreError::Poisoned)?
        .into_inner()
        .map_err(|_| StoreError::Poisoned)
}
