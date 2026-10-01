//! Agent record storage from pinned Paseo `agent/agent-storage.ts`.
//!
//! Records live at `<base>/<cwd-key>/<agentId>.json` and are written as
//! `JSON.stringify(record, null, 2)` in the key order the caller built.
//! Loading scans root-level `*.json` files, then one level of
//! subdirectories, parses each with `STORED_AGENT_SCHEMA` (zod output: schema
//! key order, unknown keys stripped, defaults applied), and skips any file
//! that fails. Free-form values keep JavaScript `JSON.parse` property order.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::atomic::write_json_atomic;
use crate::js_json::js_property_order;
use crate::{RecordError, StoreError, cwd_key};

const AGENT_STATUSES: [&str; 5] = ["initializing", "idle", "running", "error", "closed"];
const ATTENTION_REASONS: [&str; 3] = ["finished", "error", "permission"];

type Object = Map<String, Value>;

fn fail(field: &'static str, expected: &'static str) -> RecordError {
    RecordError { field, expected }
}

/// Parses one stored agent record as `STORED_AGENT_SCHEMA.parse(value)`.
///
/// # Errors
///
/// Returns the first field that fails the schema.
pub fn parse_stored_agent_record(value: &Value) -> Result<Value, RecordError> {
    let input = value.as_object().ok_or(fail("record", "object"))?;
    let mut out = Object::new();
    string(input, &mut out, "id")?;
    string(input, &mut out, "provider")?;
    string(input, &mut out, "cwd")?;
    optional_string(input, &mut out, "workspaceId")?;
    string(input, &mut out, "createdAt")?;
    string(input, &mut out, "updatedAt")?;
    optional_string(input, &mut out, "lastActivityAt")?;
    nullish_string(input, &mut out, "lastUserMessageAt")?;
    nullish_string(input, &mut out, "title")?;
    match input.get("labels") {
        None => {
            out.insert("labels".into(), Value::Object(Object::new()));
        }
        Some(labels) => {
            out.insert("labels".into(), string_record(labels, "labels")?);
        }
    }
    match input.get("lastStatus") {
        None => {
            out.insert("lastStatus".into(), "closed".into());
        }
        Some(status) => {
            out.insert(
                "lastStatus".into(),
                one_of(status, &AGENT_STATUSES, "lastStatus")?,
            );
        }
    }
    nullish_string(input, &mut out, "lastModeId")?;
    if let Some(config) = input.get("config") {
        out.insert("config".into(), parse_config(config)?);
    }
    if let Some(runtime) = input.get("runtimeInfo") {
        out.insert("runtimeInfo".into(), parse_runtime_info(runtime)?);
    }
    if let Some(features) = input.get("features") {
        let items = features
            .as_array()
            .ok_or(fail("features", "AgentFeature[]"))?;
        out.insert(
            "features".into(),
            Value::Array(items.iter().map(parse_feature).collect::<Result<_, _>>()?),
        );
    }
    if let Some(persistence) = input.get("persistence") {
        out.insert("persistence".into(), parse_persistence(persistence)?);
    }
    nullish_string(input, &mut out, "lastError")?;
    optional_bool(input, &mut out, "requiresAttention")?;
    if let Some(reason) = input.get("attentionReason") {
        let parsed = if reason.is_null() {
            Value::Null
        } else {
            one_of(reason, &ATTENTION_REASONS, "attentionReason")?
        };
        out.insert("attentionReason".into(), parsed);
    }
    nullish_string(input, &mut out, "attentionTimestamp")?;
    optional_bool(input, &mut out, "internal")?;
    nullish_string(input, &mut out, "archivedAt")?;
    if let Some(owner) = input.get("owner") {
        out.insert("owner".into(), parse_owner(owner)?);
    }
    Ok(Value::Object(out))
}

fn string(input: &Object, out: &mut Object, field: &'static str) -> Result<(), RecordError> {
    match input.get(field) {
        Some(Value::String(value)) => {
            out.insert(field.into(), Value::String(value.clone()));
            Ok(())
        }
        _ => Err(fail(field, "string")),
    }
}

/// `z.string().optional()`.
fn optional_string(
    input: &Object,
    out: &mut Object,
    field: &'static str,
) -> Result<(), RecordError> {
    match input.get(field) {
        None => Ok(()),
        Some(Value::String(value)) => {
            out.insert(field.into(), Value::String(value.clone()));
            Ok(())
        }
        Some(_) => Err(fail(field, "string | undefined")),
    }
}

/// `z.string().nullable().optional()`: missing stays missing.
fn nullish_string(
    input: &Object,
    out: &mut Object,
    field: &'static str,
) -> Result<(), RecordError> {
    match input.get(field) {
        None => Ok(()),
        Some(value @ (Value::String(_) | Value::Null)) => {
            out.insert(field.into(), value.clone());
            Ok(())
        }
        Some(_) => Err(fail(field, "string | null | undefined")),
    }
}

fn optional_bool(input: &Object, out: &mut Object, field: &'static str) -> Result<(), RecordError> {
    match input.get(field) {
        None => Ok(()),
        Some(Value::Bool(value)) => {
            out.insert(field.into(), Value::Bool(*value));
            Ok(())
        }
        Some(_) => Err(fail(field, "boolean | undefined")),
    }
}

fn one_of(value: &Value, allowed: &[&str], field: &'static str) -> Result<Value, RecordError> {
    match value {
        Value::String(text) if allowed.contains(&text.as_str()) => Ok(value.clone()),
        _ => Err(fail(field, "enum value")),
    }
}

/// `z.record(z.string(), z.string())`.
fn string_record(value: &Value, field: &'static str) -> Result<Value, RecordError> {
    let map = value
        .as_object()
        .ok_or(fail(field, "record<string, string>"))?;
    if map.values().all(Value::is_string) {
        Ok(js_property_order(value.clone()))
    } else {
        Err(fail(field, "record<string, string>"))
    }
}

/// `z.record(z.string(), z.unknown())` and `z.record(z.string(), z.json())`:
/// any JSON object passes unchanged.
fn any_record(value: &Value, field: &'static str) -> Result<Value, RecordError> {
    if value.is_object() {
        Ok(js_property_order(value.clone()))
    } else {
        Err(fail(field, "record"))
    }
}

/// `SERIALIZABLE_CONFIG_SCHEMA`, `.nullable().optional()`.
fn parse_config(value: &Value) -> Result<Value, RecordError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let input = value.as_object().ok_or(fail("config", "object | null"))?;
    let mut out = Object::new();
    nullish_string(input, &mut out, "modeId")?;
    nullish_string(input, &mut out, "model")?;
    nullish_string(input, &mut out, "thinkingOptionId")?;
    for field in ["featureValues", "providerOptions"] {
        nullish_record(input, &mut out, field)?;
    }
    if let Some(policy) = input.get("toolPolicy") {
        out.insert("toolPolicy".into(), parse_tool_policy(policy)?);
    }
    nullish_string(input, &mut out, "systemPrompt")?;
    nullish_record(input, &mut out, "mcpServers")?;
    Ok(Value::Object(out))
}

fn nullish_record(
    input: &Object,
    out: &mut Object,
    field: &'static str,
) -> Result<(), RecordError> {
    match input.get(field) {
        None => Ok(()),
        Some(Value::Null) => {
            out.insert(field.into(), Value::Null);
            Ok(())
        }
        Some(value) => {
            out.insert(field.into(), any_record(value, field)?);
            Ok(())
        }
    }
}

/// `{ preapproved: [{ kind: "mcp", server, tool }.strict()] }.strict()`, nullable.
fn parse_tool_policy(value: &Value) -> Result<Value, RecordError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let error = fail("config.toolPolicy", "strict tool policy");
    let input = value.as_object().ok_or(error.clone())?;
    if input.keys().any(|key| key != "preapproved") {
        return Err(error);
    }
    let entries = input
        .get("preapproved")
        .and_then(Value::as_array)
        .ok_or(error.clone())?;
    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let object = entry.as_object().ok_or(error.clone())?;
        let valid = object.len() == 3
            && object.get("kind").and_then(Value::as_str) == Some("mcp")
            && object.get("server").is_some_and(Value::is_string)
            && object.get("tool").is_some_and(Value::is_string);
        if !valid {
            return Err(error);
        }
        let mut out = Object::new();
        out.insert("kind".into(), "mcp".into());
        out.insert("server".into(), object["server"].clone());
        out.insert("tool".into(), object["tool"].clone());
        parsed.push(Value::Object(out));
    }
    let mut out = Object::new();
    out.insert("preapproved".into(), Value::Array(parsed));
    Ok(Value::Object(out))
}

fn parse_runtime_info(value: &Value) -> Result<Value, RecordError> {
    let input = value.as_object().ok_or(fail("runtimeInfo", "object"))?;
    let mut out = Object::new();
    string(input, &mut out, "provider")?;
    match input.get("sessionId") {
        Some(id @ (Value::String(_) | Value::Null)) => {
            out.insert("sessionId".into(), id.clone());
        }
        _ => return Err(fail("runtimeInfo.sessionId", "string | null")),
    }
    nullish_string(input, &mut out, "model")?;
    nullish_string(input, &mut out, "thinkingOptionId")?;
    nullish_string(input, &mut out, "modeId")?;
    if let Some(extra) = input.get("extra") {
        out.insert("extra".into(), any_record(extra, "runtimeInfo.extra")?);
    }
    Ok(Value::Object(out))
}

/// `AgentFeatureSchema`: toggle or select, discriminated by `type`.
fn parse_feature(value: &Value) -> Result<Value, RecordError> {
    let input = value.as_object().ok_or(fail("features[]", "object"))?;
    let kind = input.get("type").and_then(Value::as_str);
    let mut out = Object::new();
    match kind {
        Some("toggle") => {
            out.insert("type".into(), "toggle".into());
            feature_common(input, &mut out)?;
            match input.get("value") {
                Some(Value::Bool(flag)) => {
                    out.insert("value".into(), Value::Bool(*flag));
                }
                _ => return Err(fail("features[].value", "boolean")),
            }
        }
        Some("select") => {
            out.insert("type".into(), "select".into());
            feature_common(input, &mut out)?;
            match input.get("value") {
                Some(value @ (Value::String(_) | Value::Null)) => {
                    out.insert("value".into(), value.clone());
                }
                _ => return Err(fail("features[].value", "string | null")),
            }
            let options = input
                .get("options")
                .and_then(Value::as_array)
                .ok_or(fail("features[].options", "array"))?;
            out.insert(
                "options".into(),
                Value::Array(
                    options
                        .iter()
                        .map(parse_select_option)
                        .collect::<Result<_, _>>()?,
                ),
            );
        }
        _ => return Err(fail("features[].type", "\"toggle\" | \"select\"")),
    }
    Ok(Value::Object(out))
}

fn feature_common(input: &Object, out: &mut Object) -> Result<(), RecordError> {
    string(input, out, "id")?;
    string(input, out, "label")?;
    optional_string(input, out, "description")?;
    optional_string(input, out, "tooltip")?;
    optional_string(input, out, "icon")
}

fn parse_select_option(value: &Value) -> Result<Value, RecordError> {
    let input = value
        .as_object()
        .ok_or(fail("features[].options[]", "object"))?;
    let mut out = Object::new();
    string(input, &mut out, "id")?;
    string(input, &mut out, "label")?;
    optional_string(input, &mut out, "description")?;
    optional_bool(input, &mut out, "isDefault")?;
    if let Some(metadata) = input.get("metadata") {
        out.insert(
            "metadata".into(),
            any_record(metadata, "features[].options[].metadata")?,
        );
    }
    Ok(Value::Object(out))
}

/// `PERSISTENCE_HANDLE_SCHEMA`, `.nullable().optional()`.
fn parse_persistence(value: &Value) -> Result<Value, RecordError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let input = value
        .as_object()
        .ok_or(fail("persistence", "object | null"))?;
    let mut out = Object::new();
    string(input, &mut out, "provider")?;
    string(input, &mut out, "sessionId")?;
    if let Some(handle) = input.get("nativeHandle") {
        out.insert("nativeHandle".into(), js_property_order(handle.clone()));
    }
    if let Some(metadata) = input.get("metadata") {
        out.insert(
            "metadata".into(),
            any_record(metadata, "persistence.metadata")?,
        );
    }
    Ok(Value::Object(out))
}

/// `AgentOwnerSchema`: only `{ kind: "daemon", daemonId, executionId }`.
fn parse_owner(value: &Value) -> Result<Value, RecordError> {
    let input = value.as_object().ok_or(fail("owner", "object"))?;
    if input.get("kind").and_then(Value::as_str) != Some("daemon") {
        return Err(fail("owner.kind", "\"daemon\""));
    }
    let mut out = Object::new();
    out.insert("kind".into(), "daemon".into());
    string(input, &mut out, "daemonId")?;
    string(input, &mut out, "executionId")?;
    Ok(Value::Object(out))
}

/// One loaded agent record and the file it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAgentRecord {
    pub record: Value,
    pub path: PathBuf,
}

/// File-backed agent records with the baseline's id-keyed, insertion-ordered cache.
#[derive(Debug)]
pub struct AgentRecordStore {
    base: PathBuf,
    loaded: bool,
    order: Vec<String>,
    records: HashMap<String, LoadedAgentRecord>,
    paths: HashMap<String, Vec<PathBuf>>,
    skipped: Vec<(PathBuf, String)>,
}

impl AgentRecordStore {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            loaded: false,
            order: Vec::new(),
            records: HashMap::new(),
            paths: HashMap::new(),
            skipped: Vec::new(),
        }
    }

    /// Loads once. Unreadable or invalid record files are skipped and kept
    /// in [`Self::skipped`] for logging, as the baseline logs and continues.
    pub fn initialize(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        for path in self.scan() {
            match read_record(&path) {
                Ok(record) => {
                    let Some(id) = record.get("id").and_then(Value::as_str).map(str::to_owned)
                    else {
                        continue;
                    };
                    self.index(&id, record, path);
                }
                Err(reason) => self.skipped.push((path, reason)),
            }
        }
    }

    /// Files skipped during load, with the reason.
    #[must_use]
    pub fn skipped(&self) -> &[(PathBuf, String)] {
        &self.skipped
    }

    fn scan(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.base) else {
            return Vec::new();
        };
        let mut root_files = Vec::new();
        let mut directories = Vec::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_file() && is_json_name(&path) {
                root_files.push(path);
            } else if kind.is_dir() {
                directories.push(path);
            }
        }
        for directory in directories {
            let Ok(files) = fs::read_dir(&directory) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if file.file_type().is_ok_and(|kind| kind.is_file()) && is_json_name(&path) {
                    root_files.push(path);
                }
            }
        }
        root_files
    }

    fn index(&mut self, id: &str, record: Value, path: PathBuf) {
        if !self.records.contains_key(id) {
            self.order.push(id.to_owned());
        }
        let paths = self.paths.entry(id.to_owned()).or_default();
        if !paths.contains(&path) {
            paths.push(path.clone());
        }
        self.records
            .insert(id.to_owned(), LoadedAgentRecord { record, path });
    }

    /// All records in cache insertion order.
    pub fn list(&mut self) -> Vec<Value> {
        self.initialize();
        self.order
            .iter()
            .filter_map(|id| self.records.get(id))
            .map(|loaded| loaded.record.clone())
            .collect()
    }

    pub fn get(&mut self, id: &str) -> Option<Value> {
        self.initialize();
        self.records.get(id).map(|loaded| loaded.record.clone())
    }

    /// Writes `record` as built by the caller and removes the previous file
    /// when the working directory key changed.
    ///
    /// # Errors
    ///
    /// Returns an error when the record lacks `id` or `cwd` strings, or the write fails.
    pub fn write(&mut self, record: Value) -> Result<PathBuf, StoreError> {
        self.initialize();
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .ok_or(StoreError::MissingString("id"))?
            .to_owned();
        let cwd = record
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or(StoreError::MissingString("cwd"))?;
        let next = self.base.join(cwd_key(cwd)).join(format!("{id}.json"));
        let rendered = serde_json::to_string_pretty(&record).map_err(StoreError::InvalidJson)?;
        write_json_atomic(&next, &rendered)?;
        let previous = self.records.get(&id).map(|loaded| loaded.path.clone());
        self.index(&id, record, next.clone());
        if let Some(previous) = previous.filter(|previous| *previous != next) {
            let _ = fs::remove_file(&previous);
            if let Some(paths) = self.paths.get_mut(&id) {
                paths.retain(|path| *path != previous);
            }
        }
        Ok(next)
    }

    /// Deletes every file indexed for `id` and drops it from the cache.
    ///
    /// # Errors
    ///
    /// Returns the first unlink error other than not-found.
    pub fn remove(&mut self, id: &str) -> Result<(), StoreError> {
        self.initialize();
        let mut failure = None;
        for path in self.paths.remove(id).unwrap_or_default() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    failure.get_or_insert(StoreError::Io {
                        operation: "remove agent record",
                        source,
                    });
                }
            }
        }
        self.records.remove(id);
        self.order.retain(|existing| existing != id);
        failure.map_or(Ok(()), Err)
    }
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the baseline filters with a case-sensitive `name.endsWith(\".json\")`"
)]
fn is_json_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".json"))
}

fn read_record(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let parsed: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|error| error.to_string())?;
    parse_stored_agent_record(&js_property_order(parsed)).map_err(|error| error.to_string())
}
