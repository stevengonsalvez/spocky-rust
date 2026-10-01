use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use sha2::{Digest, Sha256};
use spocky_hub_pilot::{
    IpcValue, RetainedHostError, RetainedPgliteConfig, RetainedPgliteHost, SqlStatement,
};

fn main() {
    let mode = std::env::args().nth(1).expect("mode");
    let data_directory = PathBuf::from(std::env::args_os().nth(2).expect("data directory"));
    if mode == "capture" {
        capture(&data_directory);
        return;
    }
    assert_eq!(mode, "hold", "unsupported mode");
    let host = RetainedPgliteHost::open(&config(data_directory)).expect("open retained host");
    println!("{}", host.process_id().expect("retained host pid"));
    loop {
        std::thread::park();
    }
}

fn config(data_directory: PathBuf) -> RetainedPgliteConfig {
    RetainedPgliteConfig {
        node_executable: env_path("SPOCKY_NODE"),
        adapter_path: env_path("SPOCKY_PGLITE_ADAPTER"),
        package_root: env_path("SPOCKY_PGLITE_PACKAGE"),
        migrations_root: env_path("SPOCKY_HUB_MIGRATIONS"),
        data_directory,
        max_frame_bytes: 1_048_576,
        startup_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(20),
    }
}

#[allow(clippy::too_many_lines)]
fn capture(root: &std::path::Path) {
    let main = root.join("main");
    let old = root.join("old-state");
    let first = RetainedPgliteHost::open(&config(main.clone())).expect("open candidate");
    let identity = first.identity().clone();
    let migration = first.migrate().expect("migrate candidate");
    first
        .execute("create table differential_probe (value text not null unique)")
        .expect("create probe");
    first
        .query(
            "insert into differential_probe (value) values ('kept')",
            &[],
        )
        .expect("insert probe");
    let second_owner = RetainedPgliteHost::open(&config(main.clone()));
    let second_owner_error = match second_owner {
        Err(RetainedHostError::DirectoryInUse) => "DIRECTORY_IN_USE",
        Err(error) => panic!("unexpected owner error: {error}"),
        Ok(_) => panic!("second owner unexpectedly opened"),
    };
    let rollback = first.transaction(&[
        SqlStatement::new(
            "insert into differential_probe (value) values ($1)",
            vec![IpcValue::String("rolled-back".into())],
        ),
        SqlStatement::new(
            "insert into differential_probe (value) values ($1)",
            vec![IpcValue::String("kept".into())],
        ),
    ]);
    let rollback_error = match rollback {
        Err(RetainedHostError::Remote {
            code,
            message,
            details,
        }) => {
            serde_json::json!({ "code": code, "message": message, "details": details })
        }
        other => panic!("unexpected rollback outcome: {other:?}"),
    };
    let rolled_back = first
        .query(
            "select value from differential_probe where value = 'rolled-back'",
            &[],
        )
        .expect("query rollback");
    drop(first);

    let reopened = RetainedPgliteHost::open(&config(main.clone())).expect("reopen candidate");
    let restart = reopened
        .query("select value from differential_probe", &[])
        .expect("query restart");
    let catalog_tables = reopened
        .query(
            "select table_schema || '.' || table_name as name \
             from information_schema.tables \
             where table_schema in ('public', 'drizzle') \
             order by table_schema, table_name",
            &[],
        )
        .expect("query tables");
    let constraints = reopened
        .query(
            "select constraint_name as name \
             from information_schema.table_constraints \
             where table_schema = 'public' and constraint_type <> 'PRIMARY KEY' \
             order by constraint_name",
            &[],
        )
        .expect("query constraints");
    let indexes = reopened
        .query(
            "select indexname as name from pg_indexes \
             where schemaname = 'public' and indexname not like '%_pkey' \
             order by indexname",
            &[],
        )
        .expect("query indexes");
    let journal = reopened
        .query(
            "select hash, created_at from drizzle.__drizzle_migrations order by created_at, id",
            &[],
        )
        .expect("query journal");
    let crash_error = format!("{:?}", reopened.crash_for_test().expect_err("crash reply"));
    drop(reopened);
    let recovered = RetainedPgliteHost::open(&config(main)).expect("recover crashed candidate");
    let recovered_migration = recovered.migrate().expect("migrate recovered candidate");
    drop(recovered);

    let historical = RetainedPgliteHost::open(&config(old.clone())).expect("open old state");
    install_first_historical_migration(&historical);
    drop(historical);
    let historical = RetainedPgliteHost::open(&config(old)).expect("reopen old state");
    let suffix = historical.migrate().expect("migrate historical suffix");

    let schema_tables = strings(&catalog_tables)
        .into_iter()
        .filter(|name| name.starts_with("public.") && name != "public.differential_probe")
        .collect::<Vec<_>>();
    let mut schema_constraints = strings(&constraints);
    schema_constraints.extend(strings(&indexes));
    schema_constraints.sort();
    schema_constraints.dedup();
    let journal_rows = journal
        .rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "hash": string(&row[0]),
                "createdAt": numeric(&row[1]),
            })
        })
        .collect::<Vec<_>>();
    let output = serde_json::json!({
        "identity": identity,
        "operations": {
            "restart": restart.rows == [vec![IpcValue::String("kept".into())]],
            "crossProcessRejection": second_owner_error == "DIRECTORY_IN_USE",
            "transactionRollback": rolled_back.rows.is_empty(),
            "staleOwnerRecovery": recovered_migration.journal_rows == 49,
            "historicalResume": { "prefixJournalRows": 1, "suffix": suffix },
        },
        "observations": {
            "schemaTables": schema_tables,
            "schemaConstraints": schema_constraints,
            "migrationJournal": journal_rows,
            "migration": migration,
            "catalogTablesRaw": catalog_tables,
            "constraintsRaw": constraints,
            "indexesRaw": indexes,
        },
        "failures": {
            "secondOwner": second_owner_error,
            "rollback": rollback_error,
            "lostReply": crash_error,
        },
        "boundary": {
            "engine": "PGlite",
            "schema": "baseline relational",
            "dialect": "PostgreSQL",
            "migrations": "baseline journal via framed Rust-to-Node IPC",
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("serialize evidence")
    );
}

fn install_first_historical_migration(host: &RetainedPgliteHost) {
    let migrations = env_path("SPOCKY_HUB_MIGRATIONS");
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(migrations.join("meta/_journal.json")).expect("read migration journal"),
    )
    .expect("parse migration journal");
    let entry = &journal["entries"][0];
    let tag = entry["tag"].as_str().expect("migration tag");
    let timestamp = entry["when"].as_i64().expect("migration timestamp");
    let sql =
        fs::read_to_string(migrations.join(format!("{tag}.sql"))).expect("read first migration");
    host.execute(
        "create schema if not exists drizzle; \
         create table if not exists drizzle.__drizzle_migrations (\
           id serial primary key, hash text not null, created_at bigint)",
    )
    .expect("create migration infrastructure");
    for statement in sql.split("--> statement-breakpoint") {
        if !statement.trim().is_empty() {
            host.execute(statement)
                .expect("execute historical statement");
        }
    }
    let hash = format!("{:x}", Sha256::digest(sql.as_bytes()));
    host.query(
        "insert into drizzle.__drizzle_migrations (hash, created_at) values ($1, $2)",
        &[
            IpcValue::String(hash),
            IpcValue::Numeric(timestamp.to_string()),
        ],
    )
    .expect("record first historical migration");
}

fn strings(result: &spocky_hub_pilot::QueryResult) -> Vec<String> {
    result.rows.iter().map(|row| string(&row[0])).collect()
}

fn string(value: &IpcValue) -> String {
    match value {
        IpcValue::String(value) => value.clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn numeric(value: &IpcValue) -> i64 {
    match value {
        IpcValue::Numeric(value) => value.parse().expect("numeric i64"),
        other => panic!("expected numeric, got {other:?}"),
    }
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")))
}
