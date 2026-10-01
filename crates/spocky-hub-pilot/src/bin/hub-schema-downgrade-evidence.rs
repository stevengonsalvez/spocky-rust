use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use spocky_hub_pilot::{IpcValue, RetainedPgliteConfig, RetainedPgliteHost};

fn main() {
    let mode = std::env::args().nth(1).expect("mode");
    let data_directory = PathBuf::from(std::env::args_os().nth(2).expect("data directory"));
    let host = RetainedPgliteHost::open(&config(data_directory)).expect("open data directory");
    let before = journal_count(&host).unwrap_or(0);
    let migration = host.migrate();
    let after = journal_count(&host).unwrap_or(0);

    if mode == "failure" {
        let error = migration.expect_err("migration must fail").to_string();
        let table_exists = scalar_string(
            &host,
            "select coalesce(to_regclass('public.schema_downgrade_partial')::text, '')",
        );
        host.close().expect("close candidate");
        println!(
            "{}",
            json!({"mode": mode, "before": before, "after": after, "error": error, "partialTable": table_exists})
        );
        return;
    }

    let migration = migration.expect("migrate data directory");
    host.query(
        "create table if not exists schema_downgrade_probe (producer text primary key, payload text not null)",
        &[],
    ).expect("create probe");
    let producer = if mode == "produce" {
        "candidate-newer"
    } else {
        "candidate-older"
    };
    host.query(
        "insert into schema_downgrade_probe (producer, payload) values ($1, $2) on conflict (producer) do nothing",
        &[IpcValue::String(producer.into()), IpcValue::String("preserved".into())],
    ).expect("insert probe");
    let rows = probe_rows(&host);
    host.close().expect("close candidate");
    println!(
        "{}",
        json!({"mode": mode, "before": before, "after": after, "migration": migration, "rows": rows})
    );
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
        request_timeout: Duration::from_secs(30),
    }
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")))
}

fn journal_count(host: &RetainedPgliteHost) -> Result<usize, spocky_hub_pilot::RetainedHostError> {
    let result = host.query(
        "select count(*)::bigint from drizzle.__drizzle_migrations",
        &[],
    )?;
    Ok(match &result.rows[0][0] {
        IpcValue::Numeric(value) => value.parse().expect("count"),
        value => panic!("unexpected count: {value:?}"),
    })
}

fn scalar_string(host: &RetainedPgliteHost, sql: &str) -> String {
    match &host.query(sql, &[]).expect("scalar query").rows[0][0] {
        IpcValue::String(value) => value.clone(),
        value => panic!("unexpected scalar: {value:?}"),
    }
}

fn probe_rows(host: &RetainedPgliteHost) -> Vec<[String; 2]> {
    host.query(
        "select producer, payload from schema_downgrade_probe order by producer",
        &[],
    )
    .expect("query probes")
    .rows
    .into_iter()
    .map(|row| {
        let [IpcValue::String(producer), IpcValue::String(payload)] = row.as_slice() else {
            panic!("unexpected probe row: {row:?}");
        };
        [producer.clone(), payload.clone()]
    })
    .collect()
}
