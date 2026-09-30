use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const CONFLICT_MESSAGE: &str = "Settings changed on another client. Reload before saving again.";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

type Migration = dyn Fn(&Value, u64) -> Result<Value, String>;
type Listener = dyn FnMut(SettingsState);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsField {
    Boolean { default: bool },
    Integer { default: i64, minimum: Option<i64> },
}

pub struct SettingsDefinition {
    id: String,
    version: u64,
    fields: BTreeMap<String, SettingsField>,
    migration: Option<Box<Migration>>,
}

impl SettingsDefinition {
    pub fn new(
        id: impl Into<String>,
        version: u64,
        fields: BTreeMap<String, SettingsField>,
    ) -> Result<Self, SettingsError> {
        let id = id.into();
        if version == 0 || !valid_id(&id) || fields.keys().any(|field| !valid_id(field)) {
            return Err(SettingsError::InvalidDefinition);
        }
        Ok(Self {
            id,
            version,
            fields,
            migration: None,
        })
    }

    #[must_use]
    pub fn with_migration(
        mut self,
        migration: impl Fn(&Value, u64) -> Result<Value, String> + 'static,
    ) -> Self {
        self.migration = Some(Box::new(migration));
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsState {
    Ready { values: Value, revision: String },
    Invalid { revision: String, error: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsWriteState {
    Saved { values: Value, revision: String },
    Conflict { error: String },
    Invalid { error: String },
}

#[derive(Debug)]
pub enum SettingsError {
    InvalidDefinition,
    DuplicateSettings,
    SettingsNotRegistered,
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl From<std::io::Error> for SettingsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SettingsError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsEnvelope {
    version: u64,
    values: Value,
}

struct RegisteredDefinition {
    definition: SettingsDefinition,
    migration_count: usize,
    listeners: Vec<Box<Listener>>,
}

struct StoredSettings {
    raw: Option<Vec<u8>>,
    revision: String,
}

pub struct PluginSettingsStore {
    directory: PathBuf,
    definitions: BTreeMap<String, RegisteredDefinition>,
    changed_ids: Vec<String>,
}

impl PluginSettingsStore {
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self, SettingsError> {
        let directory = directory.into();
        if directory.exists() && !directory.is_dir() {
            return Err(SettingsError::InvalidDefinition);
        }
        Ok(Self {
            directory,
            definitions: BTreeMap::new(),
            changed_ids: Vec::new(),
        })
    }

    pub fn register(&mut self, definition: SettingsDefinition) -> Result<(), SettingsError> {
        if self.definitions.contains_key(&definition.id) {
            return Err(SettingsError::DuplicateSettings);
        }
        self.definitions.insert(
            definition.id.clone(),
            RegisteredDefinition {
                definition,
                migration_count: 0,
                listeners: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn subscribe(
        &mut self,
        id: &str,
        listener: impl FnMut(SettingsState) + 'static,
    ) -> Result<(), SettingsError> {
        self.registered_mut(id)?.listeners.push(Box::new(listener));
        Ok(())
    }

    pub fn read(&mut self, id: &str) -> Result<SettingsState, SettingsError> {
        let stored = self.stored(id)?;
        let result = self.read_inner(id, &stored);
        match result {
            Ok((values, migrated)) => {
                let revision = if migrated {
                    let revision = self.persist(id, &values)?;
                    let registered = self.registered_mut(id)?;
                    registered.migration_count += 1;
                    let state = SettingsState::Ready {
                        values: values.clone(),
                        revision: revision.clone(),
                    };
                    self.notify(id, &state)?;
                    self.changed_ids.push(id.to_owned());
                    revision
                } else {
                    stored.revision
                };
                Ok(SettingsState::Ready { values, revision })
            }
            Err(error) => Ok(SettingsState::Invalid {
                revision: stored.revision,
                error,
            }),
        }
    }

    pub fn write(
        &mut self,
        id: &str,
        revision: &str,
        values: &Value,
    ) -> Result<SettingsWriteState, SettingsError> {
        self.write_inner(id, revision, values, false)
    }

    pub fn reset(&mut self, id: &str, revision: &str) -> Result<SettingsWriteState, SettingsError> {
        self.write_inner(id, revision, &Value::Object(Map::new()), true)
    }

    #[must_use]
    pub fn changed_ids(&self) -> &[String] {
        &self.changed_ids
    }

    #[must_use]
    pub fn migration_count(&self, id: &str) -> Option<usize> {
        self.definitions.get(id).map(|entry| entry.migration_count)
    }

    fn write_inner(
        &mut self,
        id: &str,
        revision: &str,
        values: &Value,
        reset: bool,
    ) -> Result<SettingsWriteState, SettingsError> {
        let stored = self.stored(id)?;
        if stored.revision != revision {
            return Ok(SettingsWriteState::Conflict {
                error: CONFLICT_MESSAGE.into(),
            });
        }
        if !reset && let Some(raw) = &stored.raw {
            let envelope: SettingsEnvelope = match serde_json::from_slice(raw) {
                Ok(envelope) => envelope,
                Err(error) => {
                    return Ok(SettingsWriteState::Invalid {
                        error: error.to_string(),
                    });
                }
            };
            if envelope.version != self.registered(id)?.definition.version {
                return Ok(SettingsWriteState::Invalid {
                    error: "Reload or reset settings before saving a different schema version"
                        .into(),
                });
            }
        }
        let parsed = match self.validate(id, values) {
            Ok(parsed) => parsed,
            Err(error) => return Ok(SettingsWriteState::Invalid { error }),
        };
        let next_revision = self.persist(id, &parsed)?;
        let state = SettingsState::Ready {
            values: parsed.clone(),
            revision: next_revision.clone(),
        };
        self.notify(id, &state)?;
        self.changed_ids.push(id.to_owned());
        Ok(SettingsWriteState::Saved {
            values: parsed,
            revision: next_revision,
        })
    }

    fn read_inner(&self, id: &str, stored: &StoredSettings) -> Result<(Value, bool), String> {
        let registered = self
            .registered(id)
            .map_err(|_| "Settings are not registered".to_owned())?;
        let Some(raw) = &stored.raw else {
            return self
                .validate(id, &Value::Object(Map::new()))
                .map(|value| (value, false));
        };
        let envelope: SettingsEnvelope =
            serde_json::from_slice(raw).map_err(|error| error.to_string())?;
        if envelope.version > registered.definition.version {
            return Err("Settings were saved by a newer plugin version".into());
        }
        let migrated = envelope.version != registered.definition.version;
        let values = if migrated {
            let migration = registered.definition.migration.as_ref().ok_or_else(|| {
                format!("Settings version {} requires a migration", envelope.version)
            })?;
            migration(&envelope.values, envelope.version)?
        } else {
            envelope.values
        };
        self.validate(id, &values).map(|value| (value, migrated))
    }

    fn validate(&self, id: &str, values: &Value) -> Result<Value, String> {
        let fields = &self
            .registered(id)
            .map_err(|_| "Settings are not registered".to_owned())?
            .definition
            .fields;
        let input = values
            .as_object()
            .ok_or_else(|| "Expected settings object".to_owned())?;
        let mut parsed = Map::new();
        for (name, field) in fields {
            match field {
                SettingsField::Boolean { default } => {
                    let value = input.get(name).map_or(Ok(*default), |value| {
                        value
                            .as_bool()
                            .ok_or_else(|| format!("{name}: expected boolean"))
                    })?;
                    parsed.insert(name.clone(), Value::Bool(value));
                }
                SettingsField::Integer { default, minimum } => {
                    let value = input.get(name).map_or(Ok(*default), |value| {
                        value
                            .as_i64()
                            .ok_or_else(|| format!("{name}: expected integer"))
                    })?;
                    if minimum.is_some_and(|minimum| value < minimum) {
                        return Err(format!(
                            "{name}: value must be at least {}",
                            minimum.unwrap()
                        ));
                    }
                    parsed.insert(name.clone(), Value::from(value));
                }
            }
        }
        Ok(Value::Object(parsed))
    }

    fn stored(&self, id: &str) -> Result<StoredSettings, SettingsError> {
        self.registered(id)?;
        match fs::read(self.path(id)) {
            Ok(raw) => Ok(StoredSettings {
                revision: revision_of(&raw),
                raw: Some(raw),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoredSettings {
                raw: None,
                revision: "missing".into(),
            }),
            Err(error) => Err(error.into()),
        }
    }

    fn persist(&self, id: &str, values: &Value) -> Result<String, SettingsError> {
        let envelope = SettingsEnvelope {
            version: self.registered(id)?.definition.version,
            values: values.clone(),
        };
        let raw = serde_json::to_vec(&envelope)?;
        atomic_private_write(&self.path(id), &raw)?;
        Ok(revision_of(&raw))
    }

    fn notify(&mut self, id: &str, state: &SettingsState) -> Result<(), SettingsError> {
        let listeners = &mut self.registered_mut(id)?.listeners;
        for listener in listeners {
            let state = state.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(state)));
        }
        Ok(())
    }

    fn registered(&self, id: &str) -> Result<&RegisteredDefinition, SettingsError> {
        self.definitions
            .get(id)
            .ok_or(SettingsError::SettingsNotRegistered)
    }

    fn registered_mut(&mut self, id: &str) -> Result<&mut RegisteredDefinition, SettingsError> {
        self.definitions
            .get_mut(id)
            .ok_or(SettingsError::SettingsNotRegistered)
    }

    fn path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().enumerate().all(|(index, character)| {
            character.is_ascii_lowercase()
                || character == '-'
                || (index > 0 && character.is_ascii_digit())
        })
        && value.as_bytes()[0].is_ascii_lowercase()
}

fn revision_of(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}
