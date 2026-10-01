//! Runtime tests of the Rust `PGlite` host against the pinned package.
//! `SPOCKY_PGLITE_PACKAGE` (the installed `@electric-sql/pglite` directory)
//! and `SPOCKY_HUB_MIGRATIONS` (the Hub `drizzle` directory) are required.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use spocky_pglite_host::host::{PgliteHost, PgliteHostConfig, PgliteHostError};
use spocky_pglite_host::pglite::EngineOptions;
use spocky_pglite_host::values::IpcValue;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-pglite-host-{}-{nonce}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn required(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} must be set")))
}

fn config(data_directory: &Path, request_timeout: Option<Duration>) -> PgliteHostConfig {
    PgliteHostConfig {
        package_root: required("SPOCKY_PGLITE_PACKAGE"),
        migrations_root: required("SPOCKY_HUB_MIGRATIONS"),
        data_directory: data_directory.join("data"),
        engine: EngineOptions::default(),
        request_timeout,
    }
}

#[test]
fn request_deadline_stops_runaway_wasm_and_closes_the_host() {
    let root = TestDir::new();
    let host = PgliteHost::open(&config(&root.0, Some(Duration::from_secs(2)))).expect("open host");
    let started = Instant::now();
    let outcome = host.query("select pg_sleep(30)", &[]);
    let elapsed = started.elapsed();
    assert_eq!(outcome, Err(PgliteHostError::Timeout));
    assert!(
        elapsed < Duration::from_secs(20),
        "deadline took {elapsed:?}"
    );
    assert_eq!(host.query("select 1", &[]), Err(PgliteHostError::Closed));
}

#[test]
fn typed_query_round_trips_basic_values() {
    let root = TestDir::new();
    let host = PgliteHost::open(&config(&root.0, None)).expect("open host");
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
    host.close().expect("close");
}
