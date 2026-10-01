use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use spocky_hub_pilot::{
    IpcValue, RetainedHostError, RetainedPgliteConfig, RetainedPgliteHost, SqlStatement,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-retained-pglite-{}-{nonce}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
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

fn config(data_directory: PathBuf) -> RetainedPgliteConfig {
    config_with_migrations(
        data_directory,
        PathBuf::from(std::env::var_os("SPOCKY_HUB_MIGRATIONS").expect("SPOCKY_HUB_MIGRATIONS")),
    )
}

fn config_with_migrations(
    data_directory: PathBuf,
    migrations_root: PathBuf,
) -> RetainedPgliteConfig {
    RetainedPgliteConfig {
        node_executable: PathBuf::from(std::env::var_os("SPOCKY_NODE").expect("SPOCKY_NODE")),
        adapter_path: PathBuf::from(
            std::env::var_os("SPOCKY_PGLITE_ADAPTER").expect("SPOCKY_PGLITE_ADAPTER"),
        ),
        package_root: PathBuf::from(
            std::env::var_os("SPOCKY_PGLITE_PACKAGE").expect("SPOCKY_PGLITE_PACKAGE"),
        ),
        migrations_root,
        data_directory,
        max_frame_bytes: 1_048_576,
        startup_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(20),
    }
}

#[test]
fn retained_host_replays_history_and_preserves_typed_values() {
    let root = TestDir::new();
    let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open retained host");
    let identity = host.identity();
    assert_eq!(identity.package, "@electric-sql/pglite");
    assert_eq!(identity.package_version, "0.5.4");
    assert!(!identity.node_executable_sha256.is_empty());
    assert!(!identity.node_version.is_empty());
    assert!(!identity.os.is_empty());
    assert!(!identity.arch.is_empty());

    let migration = host.migrate().expect("replay migrations");
    assert_eq!(migration.applied, 49);
    assert_eq!(migration.journal_rows, 49);
    let second = host.migrate().expect("rerun migrations");
    assert_eq!(second.applied, 0);
    assert_eq!(second.journal_rows, 49);
    let catalog = host
        .query(
            "select \
               (select count(*)::bigint from information_schema.tables \
                where table_schema = 'public') as table_count, \
               (select count(*)::bigint from information_schema.table_constraints \
                where table_schema = 'public') as constraint_count",
            &[],
        )
        .expect("query installed catalog");
    assert_eq!(
        catalog.rows,
        [vec![
            IpcValue::Numeric("50".into()),
            IpcValue::Numeric("506".into()),
        ]]
    );

    let result = host
        .query(
            "select $1::text as text_value, $2::bytea as binary_value, \
                    $3::timestamptz as timestamp_value, $4::numeric as numeric_value, \
                    null::text as null_value",
            &[
                IpcValue::String("hello".into()),
                IpcValue::Binary(vec![0, 1, 2, 255]),
                IpcValue::Timestamp("2026-10-01T00:00:00.123Z".into()),
                IpcValue::Numeric("1234567890.123456789".into()),
            ],
        )
        .expect("typed query");
    assert_eq!(
        result.columns,
        [
            "text_value",
            "binary_value",
            "timestamp_value",
            "numeric_value",
            "null_value"
        ]
    );
    assert_eq!(
        result.rows,
        [vec![
            IpcValue::String("hello".into()),
            IpcValue::Binary(vec![0, 1, 2, 255]),
            IpcValue::Timestamp("2026-10-01T00:00:00.123Z".into()),
            IpcValue::Numeric("1234567890.123456789".into()),
            IpcValue::Null,
        ]]
    );
}

#[test]
fn retained_host_reopens_real_historical_state_and_applies_remaining_migrations() {
    let root = TestDir::new();
    {
        let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open old host");
        install_first_historical_migration(&host);
        host.query(
            "insert into \"user\" (id, name, email) values ($1, $2, $3)",
            &[
                IpcValue::String("historical-user".into()),
                IpcValue::String("Historical User".into()),
                IpcValue::String("historical@example.com".into()),
            ],
        )
        .expect("insert historical user row");
    }
    let reopened = RetainedPgliteHost::open(&config(root.0.clone())).expect("reopen old state");
    let remaining = reopened.migrate().expect("apply remaining migrations");
    assert_eq!(remaining.applied, 48);
    assert_eq!(remaining.journal_rows, 49);
    let row = reopened
        .query(
            "select id, email from \"user\" where id = $1",
            &[IpcValue::String("historical-user".into())],
        )
        .expect("query historical user row");
    assert_eq!(
        row.rows,
        [vec![
            IpcValue::String("historical-user".into()),
            IpcValue::String("historical@example.com".into()),
        ]]
    );
}

fn install_first_historical_migration(host: &RetainedPgliteHost) {
    let migrations =
        PathBuf::from(std::env::var_os("SPOCKY_HUB_MIGRATIONS").expect("SPOCKY_HUB_MIGRATIONS"));
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

#[test]
fn retained_host_rolls_back_transaction() {
    let root = TestDir::new();
    let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open retained host");
    host.migrate().expect("migrate");
    host.query(
        "create table retained_probe (id integer primary key, value text not null unique)",
        &[],
    )
    .expect("create probe");
    let failed = host.transaction(&[
        SqlStatement::new(
            "insert into retained_probe (id, value) values ($1, $2)",
            vec![
                IpcValue::Numeric("1".into()),
                IpcValue::String("first".into()),
            ],
        ),
        SqlStatement::new(
            "insert into retained_probe (id, value) values ($1, $2)",
            vec![
                IpcValue::Numeric("1".into()),
                IpcValue::String("duplicate".into()),
            ],
        ),
    ]);
    match failed {
        Err(RetainedHostError::Remote {
            code,
            message,
            details,
        }) => {
            assert_eq!(code, "23505");
            assert!(message.contains("duplicate key"));
            assert_eq!(details["constraint"], "retained_probe_pkey");
        }
        other => panic!("unexpected transaction outcome: {other:?}"),
    }
    let rows = host
        .query("select value from retained_probe order by id", &[])
        .expect("query rolled back rows");
    assert!(rows.rows.is_empty());
}

#[test]
fn retained_host_serializes_concurrent_callers() {
    let root = TestDir::new();
    let host = Arc::new(RetainedPgliteHost::open(&config(root.0.clone())).expect("open host"));
    host.execute("create table concurrent_probe (value integer primary key)")
        .expect("create probe");
    let workers = (0..8)
        .map(|value| {
            let host = Arc::clone(&host);
            thread::spawn(move || {
                host.query(
                    "insert into concurrent_probe (value) values ($1)",
                    &[IpcValue::Numeric(value.to_string())],
                )
                .expect("concurrent insert");
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().expect("join worker");
    }
    let rows = host
        .query("select value from concurrent_probe order by value", &[])
        .expect("query ordered values");
    assert_eq!(rows.rows.len(), 8);
    assert_eq!(rows.rows[0], [IpcValue::Numeric("0".into())]);
    assert_eq!(rows.rows[7], [IpcValue::Numeric("7".into())]);
}

#[test]
fn retained_host_rejects_second_owner_and_recovers_after_child_crash() {
    let root = TestDir::new();
    let first = RetainedPgliteHost::open(&config(root.0.clone())).expect("open first host");
    let second = RetainedPgliteHost::open(&config(root.0.clone()));
    assert!(matches!(second, Err(RetainedHostError::DirectoryInUse)));

    let error = first.crash_for_test().expect_err("crash loses reply");
    assert!(matches!(
        error,
        RetainedHostError::ReplyLost {
            write_may_have_committed: true,
            ..
        }
    ));
    drop(first);

    let recovered = RetainedPgliteHost::open(&config(root.0.clone())).expect("recover stale owner");
    let migration = recovered.migrate().expect("migrate after crash");
    assert_eq!(migration.journal_rows, 49);
}

#[test]
fn retained_host_recovers_committed_write_after_lost_crash_reply() {
    let root = TestDir::new();
    let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open host");
    host.execute("create table crash_probe (value text primary key)")
        .expect("create crash probe");
    let lost = host.execute_then_crash_for_test(
        "insert into crash_probe (value) values ('committed-before-crash')",
    );
    assert!(matches!(
        lost,
        Err(RetainedHostError::ReplyLost {
            write_may_have_committed: true,
            ..
        })
    ));
    drop(host);

    let recovered = RetainedPgliteHost::open(&config(root.0.clone())).expect("recover host");
    let rows = recovered
        .query("select value from crash_probe", &[])
        .expect("query committed crash row");
    assert_eq!(
        rows.rows,
        [vec![IpcValue::String("committed-before-crash".into())]]
    );
}

#[test]
fn retained_host_gives_partial_live_owner_bounded_grace() {
    let root = TestDir::new();
    let lock_path = root.0.join(".paseo-hub.lock");
    fs::write(&lock_path, "{\"pid\":").expect("write partial live owner");
    let writer = thread::spawn({
        let lock_path = lock_path.clone();
        move || {
            thread::sleep(Duration::from_millis(50));
            fs::write(
                lock_path,
                format!(
                    "{{\"pid\":{},\"token\":\"live-owner\"}}",
                    std::process::id()
                ),
            )
            .expect("complete live owner");
        }
    });

    let opened = RetainedPgliteHost::open(&config(root.0.clone()));
    writer.join().expect("join lock writer");
    assert!(matches!(opened, Err(RetainedHostError::DirectoryInUse)));
    assert!(
        fs::read_to_string(lock_path)
            .expect("read preserved live owner")
            .contains("live-owner")
    );
}

#[test]
fn retained_host_reclaims_stale_owner_with_one_concurrent_winner() {
    let root = TestDir::new();
    fs::write(
        root.0.join(".paseo-hub.lock"),
        r#"{"pid":2147483647,"token":"stale"}"#,
    )
    .expect("write stale owner");
    let barrier = Arc::new(Barrier::new(4));
    let contenders = (0..4)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let contender_config = config(root.0.clone());
            thread::spawn(move || {
                barrier.wait();
                RetainedPgliteHost::open(&contender_config)
            })
        })
        .collect::<Vec<_>>();
    let outcomes = contenders
        .into_iter()
        .map(|contender| contender.join().expect("join contender"))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .all(|error| matches!(error, RetainedHostError::DirectoryInUse))
    );
}

#[test]
#[cfg(unix)]
fn retained_host_never_replaces_live_lock_inode() {
    use std::os::unix::fs::MetadataExt as _;

    let root = TestDir::new();
    let lock_path = root.0.join(".paseo-hub.lock");
    let owner = RetainedPgliteHost::open(&config(root.0.clone())).expect("open owner");
    let inode = fs::metadata(&lock_path).expect("lock metadata").ino();
    let record = fs::read(&lock_path).expect("lock record");

    for _ in 0..8 {
        assert!(matches!(
            RetainedPgliteHost::open(&config(root.0.clone())),
            Err(RetainedHostError::DirectoryInUse)
        ));
        assert_eq!(
            fs::metadata(&lock_path).expect("lock metadata").ino(),
            inode
        );
        assert_eq!(fs::read(&lock_path).expect("lock record"), record);
    }

    owner
        .query("select 1::bigint as owned", &[])
        .expect("original owner remains usable");
}

#[test]
fn retained_host_keeps_baseline_partial_journal_semantics() {
    let root = TestDir::new();
    let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open retained host");
    host.execute(
        "create schema if not exists drizzle; \
         create table drizzle.__drizzle_migrations (\
           id serial primary key, hash text not null, created_at bigint); \
         insert into drizzle.__drizzle_migrations (hash, created_at) \
         values ('malformed-future', 9223372036854775807)",
    )
    .expect("install malformed journal");
    let migration = host
        .migrate()
        .expect("baseline skips after future timestamp");
    assert_eq!(migration.applied, 0);
    assert_eq!(migration.journal_rows, 1);
    let tables = host
        .query(
            "select count(*)::bigint as count from information_schema.tables \
             where table_schema = 'public'",
            &[],
        )
        .expect("query tables");
    assert_eq!(tables.rows, [vec![IpcValue::Numeric("0".into())]]);
}

#[test]
fn retained_host_rolls_back_pending_migrations_after_partial_journal() {
    let root = TestDir::new();
    let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open retained host");
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(
            PathBuf::from(std::env::var_os("SPOCKY_HUB_MIGRATIONS").expect("migrations"))
                .join("meta/_journal.json"),
        )
        .expect("read journal"),
    )
    .expect("parse journal");
    let first_timestamp = journal["entries"][0]["when"].as_i64().expect("timestamp");
    host.execute(&format!(
        "create schema drizzle; \
         create table drizzle.__drizzle_migrations (\
           id serial primary key, hash text not null, created_at bigint); \
         insert into drizzle.__drizzle_migrations (hash, created_at) \
         values ('missing-schema', {first_timestamp})"
    ))
    .expect("install partial journal");
    assert!(matches!(
        host.migrate(),
        Err(RetainedHostError::Remote { .. })
    ));
    let result = host
        .query(
            "select count(*)::bigint as count from drizzle.__drizzle_migrations",
            &[],
        )
        .expect("query journal after rollback");
    assert_eq!(result.rows, [vec![IpcValue::Numeric("1".into())]]);
    let tables = host
        .query(
            "select count(*)::bigint as count from information_schema.tables \
             where table_schema = 'public'",
            &[],
        )
        .expect("query public catalog after rollback");
    assert_eq!(tables.rows, [vec![IpcValue::Numeric("0".into())]]);
}

#[test]
fn retained_host_rolls_back_applied_steps_when_later_migration_fails() {
    let root = TestDir::new();
    let migrations = root.0.join("migrations");
    fs::create_dir_all(migrations.join("meta")).expect("create migration metadata directory");
    fs::write(
        migrations.join("meta/_journal.json"),
        r#"{"entries":[{"idx":0,"version":"7","when":1,"tag":"0000_first","breakpoints":true},{"idx":1,"version":"7","when":2,"tag":"0001_second","breakpoints":true}]}"#,
    )
    .expect("write migration journal");
    fs::write(
        migrations.join("0000_first.sql"),
        "create table partial_probe (value text primary key);\n\
         --> statement-breakpoint\n\
         insert into partial_probe values ('seed');",
    )
    .expect("write first migration");
    fs::write(
        migrations.join("0001_second.sql"),
        "create table rolled_back_probe (value text);\n\
         --> statement-breakpoint\n\
         definitely not valid sql;",
    )
    .expect("write failing migration");
    let migration_config = config_with_migrations(root.0.join("database"), migrations.clone());
    let host = RetainedPgliteHost::open(&migration_config).expect("open host");
    assert!(matches!(
        host.migrate(),
        Err(RetainedHostError::Remote { .. })
    ));
    let after_failure = host
        .query(
            "select \
               (select count(*)::bigint from drizzle.__drizzle_migrations) as journal_rows, \
               (select count(*)::bigint from information_schema.tables \
                where table_schema = 'public') as public_tables",
            &[],
        )
        .expect("query rollback state");
    assert_eq!(
        after_failure.rows,
        [vec![
            IpcValue::Numeric("0".into()),
            IpcValue::Numeric("0".into()),
        ]]
    );

    fs::write(
        migrations.join("0001_second.sql"),
        "alter table partial_probe add column extra text;",
    )
    .expect("repair second migration");
    let recovered = host.migrate().expect("replay repaired transaction");
    assert_eq!(recovered.applied, 2);
    assert_eq!(recovered.journal_rows, 2);
    let rows = host
        .query("select value from partial_probe", &[])
        .expect("query replayed seed");
    assert_eq!(rows.rows, [vec![IpcValue::String("seed".into())]]);
}

#[test]
fn retained_host_enforces_frame_bound_and_timeout_without_replay() {
    let root = TestDir::new();
    let mut bounded = config(root.0.clone());
    bounded.max_frame_bytes = 4096;
    bounded.request_timeout = Duration::from_millis(50);
    let host = RetainedPgliteHost::open(&bounded).expect("open bounded host");
    let oversized = host.query("select $1::text", &[IpcValue::String("x".repeat(8192))]);
    assert!(matches!(
        oversized,
        Err(RetainedHostError::FrameTooLarge { .. })
    ));
    let lost_response = host.query("select repeat('x', 8192) as value", &[]);
    assert!(matches!(
        lost_response,
        Err(RetainedHostError::ReplyLost {
            write_may_have_committed: true,
            ..
        })
    ));

    let timeout_root = TestDir::new();
    let mut timeout_config = config(timeout_root.0.clone());
    timeout_config.request_timeout = Duration::from_millis(50);
    let timeout_host = RetainedPgliteHost::open(&timeout_config).expect("open timeout host");
    let timeout = timeout_host.delay_for_test(Duration::from_secs(10));
    assert!(matches!(
        timeout,
        Err(RetainedHostError::Timeout {
            write_may_have_committed: true,
            ..
        })
    ));
}

#[test]
fn retained_host_bounds_delivery_when_child_stops_reading() {
    let root = TestDir::new();
    let mut bounded = config(root.0.clone());
    bounded.max_frame_bytes = 8 * 1024 * 1024;
    bounded.request_timeout = Duration::from_millis(100);
    let host = RetainedPgliteHost::open(&bounded).expect("open bounded host");
    host.stall_reads_for_test().expect("stall child reads");

    let started = std::time::Instant::now();
    let delivery = host.query(
        "select $1::text",
        &[IpcValue::String("x".repeat(4 * 1024 * 1024))],
    );
    assert!(
        matches!(
            &delivery,
            Err(RetainedHostError::DeliveryTimeout {
                write_may_have_committed: true,
                ..
            })
        ),
        "unexpected delivery result: {delivery:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));

    let drop_started = std::time::Instant::now();
    drop(host);
    assert!(drop_started.elapsed() < Duration::from_secs(2));
}

#[test]
fn retained_host_persists_normal_close_and_recovers_injected_post_close_failure() {
    let root = TestDir::new();
    let mut close_config = config(root.0.clone());
    close_config.request_timeout = Duration::from_secs(2);
    let host = RetainedPgliteHost::open(&close_config).expect("open host");
    host.execute("create table close_probe (value text primary key)")
        .expect("create close probe");
    host.query("insert into close_probe values ('normal')", &[])
        .expect("insert normal close row");
    host.close().expect("graceful close");

    let reopened = RetainedPgliteHost::open(&close_config).expect("reopen after normal close");
    let normal = reopened
        .query("select value from close_probe order by value", &[])
        .expect("query normal close row");
    assert_eq!(normal.rows, [vec![IpcValue::String("normal".into())]]);
    reopened
        .query("insert into close_probe values ('failed')", &[])
        .expect("insert injected post-close failure row");
    reopened
        .fail_close_for_test()
        .expect("arm injected post-close failure");
    let close_started = std::time::Instant::now();
    assert!(matches!(
        reopened.close(),
        Err(RetainedHostError::Remote { ref code, .. }) if code == "CLOSE_FAILED"
    ));
    assert!(close_started.elapsed() < Duration::from_secs(4));
    drop(reopened);

    let recovered = RetainedPgliteHost::open(&close_config).expect("reopen after failed close");
    let rows = recovered
        .query("select value from close_probe order by value", &[])
        .expect("query persisted close rows");
    assert_eq!(
        rows.rows,
        [
            vec![IpcValue::String("failed".into())],
            vec![IpcValue::String("normal".into())],
        ]
    );
}

#[test]
fn retained_host_preserves_json_tags_distinct_from_sql_scalars() {
    let root = TestDir::new();
    let host = RetainedPgliteHost::open(&config(root.0.clone())).expect("open host");
    let json_values = [
        serde_json::Value::Null,
        serde_json::json!(true),
        serde_json::json!(42),
        serde_json::json!("json-string"),
        serde_json::json!({ "key": "value" }),
        serde_json::json!([1, 2]),
    ];
    let result = host
        .query(
            "select null::text as sql_null, null::json as sql_json_null, \
                    null::jsonb as sql_jsonb_null, true::boolean as sql_boolean, \
                    42::numeric as sql_numeric, 'plain'::text as sql_string, \
                    $1::json as json_null, $2::jsonb as json_boolean, \
                    $3::jsonb as json_numeric, $4::jsonb as json_string, \
                    $5::jsonb as json_object, $6::json as json_array",
            &json_values
                .iter()
                .cloned()
                .map(IpcValue::Json)
                .collect::<Vec<_>>(),
        )
        .expect("round trip JSON and SQL scalars");
    assert_eq!(
        result.rows,
        [vec![
            IpcValue::Null,
            IpcValue::Null,
            IpcValue::Null,
            IpcValue::Boolean(true),
            IpcValue::Numeric("42".into()),
            IpcValue::String("plain".into()),
            IpcValue::Json(json_values[0].clone()),
            IpcValue::Json(json_values[1].clone()),
            IpcValue::Json(json_values[2].clone()),
            IpcValue::Json(json_values[3].clone()),
            IpcValue::Json(json_values[4].clone()),
            IpcValue::Json(json_values[5].clone()),
        ]]
    );
}

#[test]
fn retained_host_releases_owner_when_parent_process_dies() {
    let root = TestDir::new();
    let mut helper = Command::new(env!("CARGO_BIN_EXE_hub-retained-pglite-evidence"))
        .arg("hold")
        .arg(&root.0)
        .env(
            "SPOCKY_NODE",
            std::env::var_os("SPOCKY_NODE").expect("node"),
        )
        .env(
            "SPOCKY_PGLITE_ADAPTER",
            std::env::var_os("SPOCKY_PGLITE_ADAPTER").expect("adapter"),
        )
        .env(
            "SPOCKY_PGLITE_PACKAGE",
            std::env::var_os("SPOCKY_PGLITE_PACKAGE").expect("package"),
        )
        .env(
            "SPOCKY_HUB_MIGRATIONS",
            std::env::var_os("SPOCKY_HUB_MIGRATIONS").expect("migrations"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn parent helper");
    let mut line = String::new();
    BufReader::new(helper.stdout.take().expect("helper stdout"))
        .read_line(&mut line)
        .expect("read child pid");
    let node_pid: u32 = line.trim().parse().expect("parse child pid");
    let stopped = Command::new("kill")
        .args(["-STOP", &node_pid.to_string()])
        .status()
        .expect("stop retained child");
    assert!(stopped.success());
    let status = Command::new("kill")
        .args(["-9", &helper.id().to_string()])
        .status()
        .expect("kill helper");
    assert!(status.success());
    helper.wait().expect("reap helper");

    assert!(matches!(
        RetainedPgliteHost::open(&config(root.0.clone())),
        Err(RetainedHostError::DirectoryInUse)
    ));
    let resumed = Command::new("kill")
        .args(["-CONT", &node_pid.to_string()])
        .status()
        .expect("resume retained child");
    assert!(resumed.success());

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_is_running(node_pid) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !process_is_running(node_pid),
        "retained child survived parent death"
    );
    RetainedPgliteHost::open(&config(root.0.clone())).expect("reopen after parent death");
}

fn process_is_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
