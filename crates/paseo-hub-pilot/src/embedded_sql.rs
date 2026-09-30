//! Executable embedded SQL pilot for durable Hub state.

use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{DurableHubStore, StoreError, StoreSemantics};

const DATABASE_FILE: &str = "hub.sqlite3";
const LOCK_FILE: &str = ".paseo-hub.lock";
const OWNER_READ_ATTEMPTS: usize = 10;
const OWNER_READ_DELAY: Duration = Duration::from_millis(10);
const SNAPSHOT_SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS hub_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    state_bytes BLOB NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0)
)";
const RELATIONAL_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "user" (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL UNIQUE,
    email_verified INTEGER NOT NULL DEFAULT 0 CHECK (email_verified IN (0, 1)),
    must_change_password INTEGER NOT NULL DEFAULT 0 CHECK (must_change_password IN (0, 1)),
    is_instance_operator INTEGER NOT NULL DEFAULT 0 CHECK (is_instance_operator IN (0, 1))
);
CREATE TABLE IF NOT EXISTS account (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    password TEXT
);
CREATE TABLE IF NOT EXISTS organization (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS member (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    CONSTRAINT members_organization_user_unique UNIQUE (organization_id, user_id),
    CONSTRAINT members_role_check CHECK (role IN ('owner', 'admin', 'member'))
);
CREATE INDEX IF NOT EXISTS members_user_id_idx ON member (user_id);
CREATE INDEX IF NOT EXISTS members_organization_id_idx ON member (organization_id);
CREATE TABLE IF NOT EXISTS session (
    id TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL,
    token TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    active_organization_id TEXT REFERENCES organization(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS sessions_active_organization_id_idx
    ON session (active_organization_id);
CREATE TABLE IF NOT EXISTS invitation (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    role TEXT NOT NULL,
    status TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    inviter_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    CONSTRAINT invitations_role_check CHECK (role IN ('admin', 'member')),
    CONSTRAINT invitations_status_check
        CHECK (status IN ('pending', 'accepted', 'rejected', 'canceled'))
);
CREATE INDEX IF NOT EXISTS invitations_organization_status_idx
    ON invitation (organization_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS invitations_pending_organization_email_unique
    ON invitation (organization_id, lower(email)) WHERE status = 'pending';
CREATE TABLE IF NOT EXISTS instance_bootstrap (
    id TEXT PRIMARY KEY,
    organization_id TEXT REFERENCES organization(id) ON DELETE RESTRICT,
    owner_user_id TEXT REFERENCES "user"(id) ON DELETE RESTRICT,
    completed_at INTEGER,
    app_onboarding_completed_at INTEGER,
    CONSTRAINT instance_bootstrap_completion_check
        CHECK (completed_at IS NULL OR (organization_id IS NOT NULL AND owner_user_id IS NOT NULL))
);
CREATE TABLE IF NOT EXISTS runtime_configuration (
    singleton INTEGER PRIMARY KEY DEFAULT 1,
    auth_secret TEXT NOT NULL,
    CONSTRAINT runtime_configuration_singleton_check CHECK (singleton = 1)
);
CREATE TABLE IF NOT EXISTS organization_api_keys (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    prefix TEXT NOT NULL,
    verifier TEXT NOT NULL,
    scopes TEXT NOT NULL,
    created_by_user_id TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_used_at INTEGER,
    revoked_at INTEGER,
    CONSTRAINT organization_api_keys_scopes_check
        CHECK (json_valid(scopes) AND json_array_length(scopes) > 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS organization_api_keys_prefix_unique
    ON organization_api_keys (prefix);
CREATE INDEX IF NOT EXISTS organization_api_keys_organization_created_idx
    ON organization_api_keys (organization_id, created_at DESC);
"#;
const MIGRATIONS: &[(u32, &str, &str)] = &[
    (1, "0001_snapshot_state", SNAPSHOT_SCHEMA_SQL),
    (2, "0002_modeled_relational_state", RELATIONAL_SCHEMA_SQL),
];
const MODELED_CONSTRAINTS: &[&str] = &[
    "instance_bootstrap_completion_check",
    "invitations_pending_organization_email_unique",
    "invitations_role_check",
    "invitations_status_check",
    "members_organization_user_unique",
    "members_role_check",
    "organization_api_keys_prefix_unique",
    "organization_api_keys_scopes_check",
    "runtime_configuration_singleton_check",
];

/// Single-owner `SQLite` runtime with transactional whole-state persistence.
pub struct EmbeddedSqlStore {
    connection: Mutex<Connection>,
    data_directory: PathBuf,
    keyed_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    _directory_lock: DataDirectoryLock,
}

impl EmbeddedSqlStore {
    pub const SEMANTICS: StoreSemantics = StoreSemantics::EmbeddedSqlTransactionalSnapshot;
    pub const LIMITATIONS: &str = "SQLite, not PGlite or PostgreSQL; modeled relational tables coexist with whole-state snapshot persistence; no PostgreSQL dialect or advisory-lock parity";

    pub fn open(data_directory: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_directory = data_directory.as_ref().to_path_buf();
        fs::create_dir_all(&data_directory)?;
        let directory_lock = DataDirectoryLock::acquire(&data_directory)?;
        let connection = Connection::open(data_directory.join(DATABASE_FILE))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(Duration::ZERO)?;
        connection
            .pragma_update(None, "locking_mode", "EXCLUSIVE")
            .map_err(|error| map_lock_error(error, &data_directory))?;
        connection
            .execute_batch("BEGIN EXCLUSIVE")
            .map_err(|error| map_lock_error(error, &data_directory))?;
        if let Err(error) = apply_migrations(&connection) {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
        connection.execute_batch("COMMIT")?;
        Ok(Self {
            connection: Mutex::new(connection),
            data_directory,
            keyed_locks: Mutex::new(HashMap::new()),
            _directory_lock: directory_lock,
        })
    }

    #[must_use]
    pub const fn schema_sql(&self) -> &'static str {
        SNAPSHOT_SCHEMA_SQL
    }

    pub fn relational_tables(&self) -> Result<Vec<String>, StoreError> {
        self.query_text_column(
            "SELECT name FROM sqlite_master
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name != 'paseo_hub_migrations'
             ORDER BY name",
        )
    }

    pub fn schema_observation(&self) -> Result<String, StoreError> {
        self.query_text_column(
            "SELECT sql FROM sqlite_master
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .map(|statements| statements.join("\n"))
    }

    pub fn schema_constraints(&self) -> Result<Vec<String>, StoreError> {
        let schema = self.schema_observation()?;
        Ok(MODELED_CONSTRAINTS
            .iter()
            .filter(|name| schema.contains(*name))
            .map(ToString::to_string)
            .collect())
    }

    pub fn migration_journal(&self) -> Result<Vec<(u32, String)>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection
            .prepare("SELECT version, name FROM paseo_hub_migrations ORDER BY version")?;
        let rows = statement.query_map([], |row| {
            let version = row.get::<_, i64>(0)?;
            let version = u32::try_from(version)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, i64::from(u32::MAX)))?;
            Ok((version, row.get(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn revision(&self) -> Result<Option<u64>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement =
            connection.prepare("SELECT revision FROM hub_state WHERE singleton = 1")?;
        let mut rows = statement.query([])?;
        let revision = rows
            .next()?
            .map(|row| {
                let value = row.get::<_, i64>(0)?;
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
            })
            .transpose()?;
        Ok(revision)
    }

    pub fn execute_batch(&self, sql: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute_batch(sql)?;
        Ok(())
    }

    pub fn query_text_column(&self, sql: &str) -> Result<Vec<String>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn transaction<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    /// Runs one operation at a time for a shared in-process lock key.
    pub fn with_lock<T>(
        &self,
        key: &str,
        operation: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let lock = {
            let mut keyed_locks = self.keyed_locks.lock().map_err(|_| StoreError::Poisoned)?;
            Arc::clone(
                keyed_locks
                    .entry(key.to_owned())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = lock.lock().map_err(|_| StoreError::Poisoned)?;
        operation()
    }

    #[must_use]
    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }
}

fn apply_migrations(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS paseo_hub_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        )",
    )?;
    let current = connection.query_row(
        "SELECT coalesce(max(version), 0) FROM paseo_hub_migrations",
        [],
        |row| row.get::<_, u32>(0),
    )?;
    for &(version, name, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }
        connection.execute_batch(sql)?;
        connection.execute(
            "INSERT INTO paseo_hub_migrations (version, name) VALUES (?1, ?2)",
            (version, name),
        )?;
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct LockOwner {
    pid: u32,
    token: String,
}

struct DataDirectoryLock {
    path: PathBuf,
    owner: LockOwner,
}

impl DataDirectoryLock {
    fn acquire(data_directory: &Path) -> Result<Self, StoreError> {
        let path = data_directory.join(LOCK_FILE);
        let owner = LockOwner {
            pid: std::process::id(),
            token: Uuid::new_v4().to_string(),
        };
        loop {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;

                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    serde_json::to_writer(&mut file, &owner).map_err(std::io::Error::other)?;
                    file.flush()?;
                    return Ok(Self { path, owner });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            if read_lock_owner(&path)?.is_some_and(|existing| process_is_running(existing.pid)) {
                return Err(StoreError::EmbeddedDirectoryInUse(
                    data_directory.to_path_buf(),
                ));
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for DataDirectoryLock {
    fn drop(&mut self) {
        if read_lock_owner(&self.path)
            .ok()
            .flatten()
            .is_some_and(|owner| owner.token == self.owner.token)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn read_lock_owner(path: &Path) -> Result<Option<LockOwner>, StoreError> {
    for _ in 0..OWNER_READ_ATTEMPTS {
        match fs::read(path) {
            Ok(bytes) => {
                if let Ok(owner) = serde_json::from_slice(&bytes) {
                    return Ok(Some(owner));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        thread::sleep(OWNER_READ_DELAY);
    }
    Ok(None)
}

fn process_is_running(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    let Ok(output) = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    else {
        return false;
    };
    output.status.success()
        || String::from_utf8_lossy(&output.stderr).contains("Operation not permitted")
}

impl DurableHubStore for EmbeddedSqlStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement =
            connection.prepare("SELECT state_bytes FROM hub_state WHERE singleton = 1")?;
        let mut rows = statement.query([])?;
        rows.next()?
            .map(|row| row.get::<_, Vec<u8>>(0))
            .transpose()
            .map_err(Into::into)
    }

    fn save(&self, bytes: &[u8]) -> Result<(), StoreError> {
        self.transaction(|transaction| {
            transaction.execute(
                "INSERT INTO hub_state (singleton, state_bytes, revision)
                 VALUES (1, ?1, 1)
                 ON CONFLICT (singleton) DO UPDATE
                 SET state_bytes = excluded.state_bytes,
                     revision = hub_state.revision + 1",
                [bytes],
            )?;
            Ok(())
        })
    }
}

fn map_lock_error(error: rusqlite::Error, data_directory: &Path) -> StoreError {
    if matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    ) {
        StoreError::EmbeddedDirectoryInUse(data_directory.to_path_buf())
    } else {
        error.into()
    }
}
