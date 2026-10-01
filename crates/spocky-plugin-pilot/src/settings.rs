use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const CONFLICT_MESSAGE: &str = "Settings changed on another client. Reload before saving again.";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

type Migration = dyn Fn(&Value, u64) -> Result<Value, String>;
type CallbackFuture = Pin<Box<dyn Future<Output = Result<(), String>>>>;
type AsyncCallback = dyn Fn(Value) -> CallbackFuture;
type Listener = dyn FnMut(SettingsState) -> CallbackFuture;
type ErrorReporter = dyn FnMut(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsField {
    Boolean { default: bool },
    Integer { default: i64, minimum: Option<i64> },
}

pub struct SettingsSchema {
    kind: SettingsSchemaKind,
}

enum SettingsSchemaKind {
    Json,
    Boolean,
    Integer {
        minimum: Option<i64>,
    },
    Number {
        minimum: Option<f64>,
    },
    String {
        minimum_length: Option<usize>,
    },
    Enum {
        values: Vec<String>,
    },
    Object {
        fields: Vec<(String, SettingsSchema)>,
    },
    Array {
        item: Box<SettingsSchema>,
        minimum_length: Option<usize>,
    },
    Default {
        schema: Box<SettingsSchema>,
        value: Value,
    },
    Refine {
        schema: Box<SettingsSchema>,
        callback: Box<AsyncCallback>,
    },
}

impl SettingsSchema {
    #[must_use]
    pub const fn json() -> Self {
        Self {
            kind: SettingsSchemaKind::Json,
        }
    }

    #[must_use]
    pub const fn boolean() -> Self {
        Self {
            kind: SettingsSchemaKind::Boolean,
        }
    }

    #[must_use]
    pub const fn integer() -> Self {
        Self {
            kind: SettingsSchemaKind::Integer { minimum: None },
        }
    }

    #[must_use]
    pub fn number() -> Self {
        Self {
            kind: SettingsSchemaKind::Number { minimum: None },
        }
    }

    #[must_use]
    pub const fn string() -> Self {
        Self {
            kind: SettingsSchemaKind::String {
                minimum_length: None,
            },
        }
    }

    #[must_use]
    pub fn enumeration(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            kind: SettingsSchemaKind::Enum {
                values: values.into_iter().map(Into::into).collect(),
            },
        }
    }

    #[must_use]
    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, SettingsSchema)>) -> Self {
        Self {
            kind: SettingsSchemaKind::Object {
                fields: fields
                    .into_iter()
                    .map(|(name, schema)| (name.into(), schema))
                    .collect(),
            },
        }
    }

    #[must_use]
    pub fn array(item: SettingsSchema) -> Self {
        Self {
            kind: SettingsSchemaKind::Array {
                item: Box::new(item),
                minimum_length: None,
            },
        }
    }

    #[must_use]
    pub fn minimum(mut self, minimum: f64) -> Self {
        if let SettingsSchemaKind::Number { minimum: value } = &mut self.kind {
            *value = Some(minimum);
        }
        self
    }

    #[must_use]
    pub fn minimum_integer(mut self, minimum: i64) -> Self {
        if let SettingsSchemaKind::Integer { minimum: value } = &mut self.kind {
            *value = Some(minimum);
        }
        self
    }

    #[must_use]
    pub fn min_length(mut self, minimum: usize) -> Self {
        match &mut self.kind {
            SettingsSchemaKind::String { minimum_length }
            | SettingsSchemaKind::Array { minimum_length, .. } => {
                *minimum_length = Some(minimum);
            }
            _ => {}
        }
        self
    }

    #[must_use]
    pub fn default(self, value: Value) -> Self {
        Self {
            kind: SettingsSchemaKind::Default {
                schema: Box::new(self),
                value,
            },
        }
    }

    #[must_use]
    pub fn refine_async<F, Fut>(self, callback: F) -> Self
    where
        F: Fn(Value) -> Fut + 'static,
        Fut: Future<Output = Result<(), String>> + 'static,
    {
        Self {
            kind: SettingsSchemaKind::Refine {
                schema: Box::new(self),
                callback: Box::new(move |value| Box::pin(callback(value))),
            },
        }
    }

    fn validate(&self, value: Option<&Value>) -> Result<Value, String> {
        match &self.kind {
            SettingsSchemaKind::Default {
                schema,
                value: fallback,
            } if value.is_none() => schema.validate(Some(fallback)),
            SettingsSchemaKind::Default { schema, .. } => schema.validate(value),
            _ => self.validate_present(value.ok_or_else(|| {
                format!(
                    "Invalid input: expected {}, received undefined",
                    self.expected_type()
                )
            })?),
        }
    }

    fn validate_present(&self, value: &Value) -> Result<Value, String> {
        match &self.kind {
            SettingsSchemaKind::Json => Ok(value.clone()),
            SettingsSchemaKind::Boolean => value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| invalid_type("boolean", value)),
            SettingsSchemaKind::Integer { minimum } => {
                if !value.is_number() {
                    return Err(invalid_type("number", value));
                }
                let parsed = value.as_i64().ok_or_else(|| invalid_type("int", value))?;
                if minimum.is_some_and(|minimum| parsed < minimum) {
                    return Err(format!(
                        "Too small: expected number to be >={}",
                        minimum.unwrap()
                    ));
                }
                Ok(Value::from(parsed))
            }
            SettingsSchemaKind::Number { minimum } => {
                let parsed = value
                    .as_f64()
                    .ok_or_else(|| invalid_type("number", value))?;
                if minimum.is_some_and(|minimum| parsed < minimum) {
                    return Err(format!(
                        "Too small: expected number to be >={}",
                        minimum.unwrap()
                    ));
                }
                Ok(value.clone())
            }
            SettingsSchemaKind::String { minimum_length } => {
                let parsed = value
                    .as_str()
                    .ok_or_else(|| invalid_type("string", value))?;
                if minimum_length.is_some_and(|minimum| parsed.chars().count() < minimum) {
                    return Err(format!(
                        "Too small: expected string to have >={} characters",
                        minimum_length.unwrap()
                    ));
                }
                Ok(Value::String(parsed.to_owned()))
            }
            SettingsSchemaKind::Enum { values } => {
                let parsed = value
                    .as_str()
                    .filter(|parsed| values.iter().any(|candidate| candidate == parsed))
                    .ok_or_else(|| {
                        format!(
                            "Invalid option: expected one of {}",
                            values
                                .iter()
                                .map(|value| format!("\"{value}\""))
                                .collect::<Vec<_>>()
                                .join("|")
                        )
                    })?;
                Ok(Value::String(parsed.to_owned()))
            }
            SettingsSchemaKind::Object { fields } => validate_object(fields, value),
            SettingsSchemaKind::Array {
                item,
                minimum_length,
            } => validate_array(item, *minimum_length, value),
            SettingsSchemaKind::Default { schema, .. } => schema.validate(Some(value)),
            SettingsSchemaKind::Refine { schema, callback } => {
                let parsed = schema.validate(Some(value))?;
                block_on(callback(parsed.clone()))?;
                Ok(parsed)
            }
        }
    }

    fn expected_type(&self) -> &'static str {
        match &self.kind {
            SettingsSchemaKind::Json => "JSON value",
            SettingsSchemaKind::Boolean => "boolean",
            SettingsSchemaKind::Integer { .. } => "int",
            SettingsSchemaKind::Number { .. } => "number",
            SettingsSchemaKind::String { .. } | SettingsSchemaKind::Enum { .. } => "string",
            SettingsSchemaKind::Object { .. } => "object",
            SettingsSchemaKind::Array { .. } => "array",
            SettingsSchemaKind::Default { schema, .. }
            | SettingsSchemaKind::Refine { schema, .. } => schema.expected_type(),
        }
    }
}

pub struct SettingsDefinition {
    id: String,
    version: u64,
    schema: SettingsSchema,
    migration: Option<Box<Migration>>,
}

impl SettingsDefinition {
    pub fn new(
        id: impl Into<String>,
        version: u64,
        fields: BTreeMap<String, SettingsField>,
    ) -> Result<Self, SettingsError> {
        let id = id.into();
        if version == 0 || !valid_id(&id) {
            return Err(SettingsError::InvalidDefinition);
        }
        let fields = fields.into_iter().map(|(name, field)| {
            let schema = match field {
                SettingsField::Boolean { default } => {
                    SettingsSchema::boolean().default(default.into())
                }
                SettingsField::Integer { default, minimum } => {
                    let schema = minimum.map_or_else(SettingsSchema::integer, |minimum| {
                        SettingsSchema::integer().minimum_integer(minimum)
                    });
                    schema.default(default.into())
                }
            };
            (name, schema)
        });
        Ok(Self {
            id,
            version,
            schema: SettingsSchema::object(fields),
            migration: None,
        })
    }

    pub fn from_schema(
        id: impl Into<String>,
        version: u64,
        schema: SettingsSchema,
    ) -> Result<Self, SettingsError> {
        let id = id.into();
        if version == 0 || !valid_id(&id) {
            return Err(SettingsError::InvalidDefinition);
        }
        Ok(Self {
            id,
            version,
            schema,
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
    error_reporter: Box<ErrorReporter>,
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
            error_reporter: Box::new(|error| eprintln!("{error}")),
        })
    }

    pub fn open_with_error_reporter(
        directory: impl Into<PathBuf>,
        error_reporter: impl FnMut(String) + 'static,
    ) -> Result<Self, SettingsError> {
        let mut store = Self::open(directory)?;
        store.error_reporter = Box::new(error_reporter);
        Ok(store)
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
        let mut listener = listener;
        self.registered_mut(id)?
            .listeners
            .push(Box::new(move |state| {
                listener(state);
                Box::pin(async { Ok(()) })
            }));
        Ok(())
    }

    pub fn subscribe_async<F, Fut>(
        &mut self,
        id: &str,
        mut listener: F,
    ) -> Result<(), SettingsError>
    where
        F: FnMut(SettingsState) -> Fut + 'static,
        Fut: Future<Output = Result<(), String>> + 'static,
    {
        self.registered_mut(id)?
            .listeners
            .push(Box::new(move |state| Box::pin(listener(state))));
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
        let schema = &self
            .registered(id)
            .map_err(|_| "Settings are not registered".to_owned())?
            .definition
            .schema;
        schema.validate(Some(values))
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
        let mut listeners = std::mem::take(&mut self.registered_mut(id)?.listeners);
        for listener in &mut listeners {
            let state = state.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                block_on(listener(state))
            }));
            let failure = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(payload) => Some(panic_message(payload.as_ref())),
            };
            if let Some(error) = failure {
                (self.error_reporter)(format!(
                    "Plugin settings subscriber failed for {id}: {error}"
                ));
            }
        }
        self.registered_mut(id)?.listeners = listeners;
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

fn invalid_type(expected: &str, value: &Value) -> String {
    let received = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    format!("Invalid input: expected {expected}, received {received}")
}

fn validate_object(fields: &[(String, SettingsSchema)], value: &Value) -> Result<Value, String> {
    let input = value
        .as_object()
        .ok_or_else(|| invalid_type("object", value))?;
    let mut output = Map::new();
    let mut errors = Vec::new();
    for (name, schema) in fields {
        match schema.validate(input.get(name)) {
            Ok(value) => {
                output.insert(name.clone(), value);
            }
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(output))
    } else {
        Err(errors.join("\n"))
    }
}

fn validate_array(
    item: &SettingsSchema,
    minimum_length: Option<usize>,
    value: &Value,
) -> Result<Value, String> {
    let input = value
        .as_array()
        .ok_or_else(|| invalid_type("array", value))?;
    if minimum_length.is_some_and(|minimum| input.len() < minimum) {
        return Err(format!(
            "Too small: expected array to have >={} items",
            minimum_length.unwrap()
        ));
    }
    let mut output = Vec::with_capacity(input.len());
    let mut errors = Vec::new();
    for value in input {
        match item.validate(Some(value)) {
            Ok(value) => output.push(value),
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok(Value::Array(output))
    } else {
        Err(errors.join("\n"))
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .map_or_else(|| "callback panicked".to_owned(), Clone::clone)
        },
        |message| (*message).to_owned(),
    )
}

struct ThreadWake(thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
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
