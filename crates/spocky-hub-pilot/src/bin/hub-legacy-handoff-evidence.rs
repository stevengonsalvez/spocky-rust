use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use spocky_hub_pilot::{IpcValue, RetainedPgliteConfig, RetainedPgliteHost};

fn main() {
    let data_directory = PathBuf::from(std::env::args_os().nth(1).expect("data directory"));
    let host = RetainedPgliteHost::open(&config(data_directory)).expect("open baseline directory");
    let before_journal_rows = scalar_count(
        &host
            .query(
                "select count(*)::bigint from drizzle.__drizzle_migrations",
                &[],
            )
            .expect("query pre-migration journal"),
    );
    let baseline_marker = marker_present(&host, "pinned-baseline");
    let migration = host.migrate().expect("migrate baseline directory");
    host.query(
        "insert into legacy_handoff_probe (producer, payload) values ($1, $2)",
        &[
            IpcValue::String("retained-candidate".into()),
            IpcValue::String("candidate-data-preserved".into()),
        ],
    )
    .expect("insert candidate marker");
    let after_journal_rows = scalar_count(
        &host
            .query(
                "select count(*)::bigint from drizzle.__drizzle_migrations",
                &[],
            )
            .expect("query post-migration journal"),
    );
    let candidate_marker = marker_present(&host, "retained-candidate");
    let identity = host.identity().clone();
    host.close().expect("close retained candidate");

    println!(
        "{}",
        serde_json::to_string(&json!({
            "operation": "candidate-forward-handoff",
            "opened": true,
            "identity": identity,
            "beforeJournalRows": before_journal_rows,
            "migration": migration,
            "afterJournalRows": after_journal_rows,
            "baselineMarker": baseline_marker,
            "candidateMarker": candidate_marker,
        }))
        .expect("serialize handoff evidence")
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
        request_timeout: Duration::from_secs(20),
    }
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")))
}

fn scalar_count(result: &spocky_hub_pilot::QueryResult) -> usize {
    match result.rows.as_slice() {
        [row] => match row.as_slice() {
            [IpcValue::Numeric(value)] => value.parse().expect("numeric count"),
            other => panic!("unexpected count row: {other:?}"),
        },
        other => panic!("unexpected count result: {other:?}"),
    }
}

fn marker_present(host: &RetainedPgliteHost, producer: &str) -> bool {
    let result = host
        .query(
            "select payload from legacy_handoff_probe where producer = $1",
            &[IpcValue::String(producer.into())],
        )
        .expect("query handoff marker");
    result.rows.len() == 1
}
