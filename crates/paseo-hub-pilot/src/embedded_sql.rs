//! Executable embedded SQL pilot for durable Hub state.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::{DurableHubStore, StoreError, StoreSemantics};

const DATABASE_FILE: &str = "hub.sqlite3";
const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS hub_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    state_bytes BLOB NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0)
)";

/// Single-owner `SQLite` runtime with transactional whole-state persistence.
pub struct EmbeddedSqlStore {
    connection: Mutex<Connection>,
    data_directory: PathBuf,
}

impl EmbeddedSqlStore {
    pub const SEMANTICS: StoreSemantics = StoreSemantics::EmbeddedSqlTransactionalSnapshot;
    pub const LIMITATIONS: &str = "SQLite, not PGlite or PostgreSQL; whole-state snapshot rather than the baseline relational schema; no PostgreSQL dialect or advisory-lock parity";

    pub fn open(data_directory: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_directory = data_directory.as_ref().to_path_buf();
        fs::create_dir_all(&data_directory)?;
        let connection = Connection::open(data_directory.join(DATABASE_FILE))?;
        connection.busy_timeout(Duration::ZERO)?;
        connection
            .pragma_update(None, "locking_mode", "EXCLUSIVE")
            .map_err(|error| map_lock_error(error, &data_directory))?;
        connection
            .execute_batch("BEGIN EXCLUSIVE")
            .map_err(|error| map_lock_error(error, &data_directory))?;
        if let Err(error) = connection.execute_batch(SCHEMA_SQL) {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error.into());
        }
        connection.execute_batch("COMMIT")?;
        Ok(Self {
            connection: Mutex::new(connection),
            data_directory,
        })
    }

    #[must_use]
    pub const fn schema_sql(&self) -> &'static str {
        SCHEMA_SQL
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

    #[must_use]
    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }
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
