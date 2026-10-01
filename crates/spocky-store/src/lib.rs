use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub mod agent_record;
mod atomic;
pub mod collate;
pub mod js_value;
pub mod path_compare;
pub mod registry;
pub mod time;

pub use registry::RecordError;

#[derive(Debug)]
pub enum StoreError {
    InvalidJson(serde_json::Error),
    MissingString(&'static str),
    InvalidRecord(RecordError),
    JsonSyntax(js_value::JsonSyntaxError),
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
            Self::InvalidRecord(error) => write!(formatter, "invalid registry record: {error}"),
            Self::JsonSyntax(error) => write!(formatter, "invalid JSON: {error}"),
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            Self::InvalidRecord(error) => Some(error),
            Self::JsonSyntax(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::MissingString(_) => None,
        }
    }
}

/// The P2 agent record view. Parsing and storage go through
/// [`agent_record`] (`JSON.parse` plus `STORED_AGENT_SCHEMA`), so the record
/// is exactly what the baseline loads. [`Self::as_value`] is a `serde_json`
/// view kept for the P2 differential driver: it writes a non-finite number as
/// `null` and a lone surrogate as U+FFFD. New code uses
/// [`agent_record::AgentRecordStore`] and [`Self::as_js_value`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAgentRecord {
    record: js_value::JsValue,
    value: Value,
    id: String,
    provider: String,
    cwd: String,
    created_at: String,
    updated_at: String,
    last_status: String,
}

impl StoredAgentRecord {
    /// Parses one record file as the baseline loads it.
    ///
    /// # Errors
    ///
    /// Returns an error for text `JSON.parse` rejects, a missing required
    /// string field, or any other `STORED_AGENT_SCHEMA` failure.
    pub fn from_json(source: &str) -> Result<Self, StoreError> {
        let parsed = js_value::parse(source).map_err(StoreError::JsonSyntax)?;
        let record = agent_record::parse_stored_agent_record(&parsed).map_err(|error| {
            if error.expected == "string" {
                StoreError::MissingString(error.field)
            } else {
                StoreError::InvalidRecord(error)
            }
        })?;
        Self::from_parsed(record)
    }

    fn from_parsed(record: js_value::JsValue) -> Result<Self, StoreError> {
        let text = |field: &'static str| {
            record
                .get(field)
                .and_then(js_value::JsValue::as_str)
                .map(str::to_owned)
                .ok_or(StoreError::MissingString(field))
        };
        let value = serde_json::from_str(&js_value::stringify(&record)).unwrap_or(Value::Null);
        Ok(Self {
            id: text("id")?,
            provider: text("provider")?,
            cwd: text("cwd")?,
            created_at: text("createdAt")?,
            updated_at: text("updatedAt")?,
            last_status: text("lastStatus")?,
            value,
            record,
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

    /// The `serde_json` view described on the type.
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// The exact parsed record.
    #[must_use]
    pub const fn as_js_value(&self) -> &js_value::JsValue {
        &self.record
    }
}

/// The P2 agent store, a stateless wrapper over [`agent_record`]. Each
/// [`Self::load`] rescans the whole store (the session uses one long-lived
/// [`agent_record::AgentRecordStore`] instead), and a record file the schema
/// rejects is skipped as at baseline startup, so `load` returns `Ok(None)`
/// for it rather than an error.
#[derive(Debug, Clone)]
pub struct AgentStore {
    base: PathBuf,
}

impl AgentStore {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Atomically writes one record in the baseline `<cwd-key>/<id>.json` layout.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation, writing, or rename fails.
    pub fn write(&self, record: &StoredAgentRecord) -> Result<PathBuf, StoreError> {
        agent_record::write_record_file(&self.base, record.as_js_value())
    }

    /// Loads an agent by scanning the store as the baseline does at startup.
    ///
    /// # Errors
    ///
    /// Returns an error if a loaded record lacks a required string field.
    pub fn load(&self, id: &str) -> Result<Option<StoredAgentRecord>, StoreError> {
        agent_record::AgentRecordStore::new(&self.base)
            .get(id)
            .map(StoredAgentRecord::from_parsed)
            .transpose()
    }
}

pub(crate) fn cwd_key(cwd: &str) -> String {
    let root_end = win32_root_end(cwd);
    let (root, remainder) = cwd.split_at(root_end);
    let remainder = remainder.trim_end_matches(['/', '\\']);
    let sanitized_root = collapse_path_delimiters(root, true)
        .trim_matches('-')
        .to_owned();
    let sanitized_remainder = collapse_path_delimiters(remainder, false);

    match (sanitized_root.is_empty(), sanitized_remainder.is_empty()) {
        (true, true) => "root".to_owned(),
        (true, false) => sanitized_remainder,
        (false, true) => sanitized_root,
        (false, false) => format!("{sanitized_root}-{sanitized_remainder}"),
    }
}

fn win32_root_end(path: &str) -> usize {
    let bytes = path.as_bytes();
    let is_separator = |byte: u8| matches!(byte, b'/' | b'\\');

    if bytes.len() >= 2 && is_separator(bytes[0]) && is_separator(bytes[1]) {
        let Some(server_end) = bytes[2..]
            .iter()
            .position(|byte| is_separator(*byte))
            .map(|offset| offset + 2)
        else {
            return path.len();
        };
        let share_start = server_end + 1;
        return bytes[share_start..]
            .iter()
            .position(|byte| is_separator(*byte))
            .map_or(path.len(), |offset| share_start + offset + 1);
    }

    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return if bytes.get(2).is_some_and(|byte| is_separator(*byte)) {
            3
        } else {
            2
        };
    }

    usize::from(bytes.first().is_some_and(|byte| is_separator(*byte)))
}

fn collapse_path_delimiters(value: &str, include_colon: bool) -> String {
    let mut result = String::with_capacity(value.len());
    let mut previous_was_delimiter = false;

    for character in value.chars() {
        let is_delimiter = matches!(character, '/' | '\\') || (include_colon && character == ':');
        if is_delimiter {
            if !previous_was_delimiter {
                result.push('-');
            }
        } else {
            result.push(character);
        }
        previous_was_delimiter = is_delimiter;
    }

    result
}

#[must_use]
pub fn is_record_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "json")
}
