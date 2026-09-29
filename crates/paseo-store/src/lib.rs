use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum StoreError {
    InvalidJson(serde_json::Error),
    MissingString(&'static str),
    Io {
        operation: &'static str,
        source: io::Error,
    },
}

impl Display for StoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(formatter, "invalid stored agent JSON: {error}"),
            Self::MissingString(field) => {
                write!(formatter, "missing required string field '{field}'")
            }
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::MissingString(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAgentRecord {
    value: Value,
    id: String,
    provider: String,
    cwd: String,
    created_at: String,
    updated_at: String,
    last_status: String,
}

impl StoredAgentRecord {
    /// Parses the baseline JSON shape while retaining every recognized and unknown field.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid JSON or a missing required string field.
    pub fn from_json(source: &str) -> Result<Self, StoreError> {
        let value: Value = serde_json::from_str(source).map_err(StoreError::InvalidJson)?;
        let id = required_string(&value, "id")?;
        let provider = required_string(&value, "provider")?;
        let cwd = required_string(&value, "cwd")?;
        let created_at = required_string(&value, "createdAt")?;
        let updated_at = required_string(&value, "updatedAt")?;
        let last_status = value
            .get("lastStatus")
            .and_then(Value::as_str)
            .unwrap_or("closed")
            .to_owned();
        Ok(Self {
            value,
            id,
            provider,
            cwd,
            created_at,
            updated_at,
            last_status,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    #[must_use]
    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    #[must_use]
    pub fn updated_at(&self) -> &str {
        &self.updated_at
    }

    #[must_use]
    pub fn last_status(&self) -> &str {
        &self.last_status
    }

    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.value
    }
}

fn required_string(value: &Value, field: &'static str) -> Result<String, StoreError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(StoreError::MissingString(field))
}

#[derive(Debug, Clone)]
pub struct AgentStore {
    base: PathBuf,
}

impl AgentStore {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Atomically writes one record using the baseline working-directory key layout.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation, serialization, writing, or rename fails.
    pub fn write(&self, record: &StoredAgentRecord) -> Result<PathBuf, StoreError> {
        let directory = self.base.join(cwd_key(record.cwd()));
        fs::create_dir_all(&directory).map_err(|source| StoreError::Io {
            operation: "create agent directory",
            source,
        })?;
        let destination = directory.join(format!("{}.json", record.id()));
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(
            ".{}.json.{}.{}.tmp",
            record.id(),
            std::process::id(),
            sequence
        ));
        let bytes =
            serde_json::to_vec_pretty(record.as_value()).map_err(StoreError::InvalidJson)?;
        if let Err(source) = fs::write(&temporary, bytes) {
            return Err(StoreError::Io {
                operation: "write temporary agent record",
                source,
            });
        }
        if let Err(source) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(StoreError::Io {
                operation: "rename temporary agent record",
                source,
            });
        }
        Ok(destination)
    }

    /// Loads an agent by scanning baseline working-directory buckets.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be scanned or a matching record is invalid.
    pub fn load(&self, id: &str) -> Result<Option<StoredAgentRecord>, StoreError> {
        let directories = match fs::read_dir(&self.base) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    operation: "read agent store",
                    source,
                });
            }
        };
        for entry in directories {
            let entry = entry.map_err(|source| StoreError::Io {
                operation: "read agent store entry",
                source,
            })?;
            if !entry
                .file_type()
                .map_err(|source| StoreError::Io {
                    operation: "read agent store entry type",
                    source,
                })?
                .is_dir()
            {
                continue;
            }
            let path = entry.path().join(format!("{id}.json"));
            match fs::read_to_string(&path) {
                Ok(source) => return StoredAgentRecord::from_json(&source).map(Some),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(StoreError::Io {
                        operation: "read agent record",
                        source,
                    });
                }
            }
        }
        Ok(None)
    }
}

fn cwd_key(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    if trimmed.is_empty() {
        return "root".to_owned();
    }
    trimmed.replace(['/', '\\'], "-")
}

#[must_use]
pub fn is_record_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "json")
}
