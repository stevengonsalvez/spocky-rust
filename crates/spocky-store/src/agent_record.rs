//! Agent record storage from pinned Paseo `agent/agent-storage.ts`.
//!
//! Records live at `<base>/<cwd-key>/<agentId>.json` and are written as
//! `JSON.stringify(record, null, 2)` in the key order the caller built.
//! Loading scans root-level `*.json` files, then one level of
//! subdirectories, parses each with `JSON.parse` semantics
//! ([`crate::js_value`]) and `STORED_AGENT_SCHEMA` (zod output: schema
//! key order, unknown keys stripped, defaults applied), and skips any file
//! that fails. Free-form values are kept as parsed and written with
//! `JSON.stringify` property order.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic::write_json_atomic;
use crate::js_value::{JsObject, JsValue, js_text_to_utf8, parse, stringify_pretty};
use crate::{RecordError, StoreError, cwd_key};

const AGENT_STATUSES: [&str; 5] = ["initializing", "idle", "running", "error", "closed"];
const ATTENTION_REASONS: [&str; 3] = ["finished", "error", "permission"];

type Object = JsObject;

fn fail(field: &'static str, expected: &'static str) -> RecordError {
    RecordError { field, expected }
}

/// Parses one stored agent record as `STORED_AGENT_SCHEMA.parse(value)`.
///
/// # Errors
///
/// Returns the first field that fails the schema.
pub fn parse_stored_agent_record(value: &JsValue) -> Result<JsValue, RecordError> {
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
            out.insert("labels", JsValue::Object(Object::new()));
        }
        Some(labels) => {
            out.insert("labels".to_owned(), string_record(labels, "labels")?);
        }
    }
    match input.get("lastStatus") {
        None => {
            out.insert("lastStatus", JsValue::String("closed".to_owned()));
        }
        Some(status) => {
            out.insert(
                "lastStatus".to_owned(),
                one_of(status, &AGENT_STATUSES, "lastStatus")?,
            );
        }
    }
    nullish_string(input, &mut out, "lastModeId")?;
    if let Some(config) = input.get("config") {
        out.insert("config".to_owned(), parse_config(config)?);
    }
    if let Some(runtime) = input.get("runtimeInfo") {
        out.insert("runtimeInfo".to_owned(), parse_runtime_info(runtime)?);
    }
    if let Some(features) = input.get("features") {
        let items = features
            .as_array()
            .ok_or(fail("features", "AgentFeature[]"))?;
        out.insert(
            "features".to_owned(),
            JsValue::Array(items.iter().map(parse_feature).collect::<Result<_, _>>()?),
        );
    }
    if let Some(persistence) = input.get("persistence") {
        out.insert("persistence".to_owned(), parse_persistence(persistence)?);
    }
    nullish_string(input, &mut out, "lastError")?;
    optional_bool(input, &mut out, "requiresAttention")?;
    if let Some(reason) = input.get("attentionReason") {
        let parsed = if reason.is_null() {
            JsValue::Null
        } else {
            one_of(reason, &ATTENTION_REASONS, "attentionReason")?
        };
        out.insert("attentionReason".to_owned(), parsed);
    }
    nullish_string(input, &mut out, "attentionTimestamp")?;
    optional_bool(input, &mut out, "internal")?;
    nullish_string(input, &mut out, "archivedAt")?;
    if let Some(owner) = input.get("owner") {
        out.insert("owner".to_owned(), parse_owner(owner)?);
    }
    Ok(JsValue::Object(out))
}

fn string(input: &Object, out: &mut Object, field: &'static str) -> Result<(), RecordError> {
    match input.get(field) {
        Some(JsValue::String(value)) => {
            out.insert(field.to_owned(), JsValue::String(value.clone()));
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
        Some(JsValue::String(value)) => {
            out.insert(field.to_owned(), JsValue::String(value.clone()));
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
        Some(value @ (JsValue::String(_) | JsValue::Null)) => {
            out.insert(field.to_owned(), value.clone());
            Ok(())
        }
        Some(_) => Err(fail(field, "string | null | undefined")),
    }
}

fn optional_bool(input: &Object, out: &mut Object, field: &'static str) -> Result<(), RecordError> {
    match input.get(field) {
        None => Ok(()),
        Some(JsValue::Bool(value)) => {
            out.insert(field.to_owned(), JsValue::Bool(*value));
            Ok(())
        }
        Some(_) => Err(fail(field, "boolean | undefined")),
    }
}

fn one_of(value: &JsValue, allowed: &[&str], field: &'static str) -> Result<JsValue, RecordError> {
    match value {
        JsValue::String(text) if allowed.contains(&text.as_str()) => Ok(value.clone()),
        _ => Err(fail(field, "enum value")),
    }
}

/// zod v4 `z.record` (and `z.json()` records) skip an own `__proto__` key.
const PROTO_KEY: &str = "__proto__";

/// `z.record(z.string(), z.string())`.
fn string_record(value: &JsValue, field: &'static str) -> Result<JsValue, RecordError> {
    let map = value
        .as_object()
        .ok_or(fail(field, "record<string, string>"))?;
    let mut out = Object::new();
    for (key, entry) in map.iter().filter(|(key, _)| *key != PROTO_KEY) {
        if !entry.is_string() {
            return Err(fail(field, "record<string, string>"));
        }
        out.insert(key, entry.clone());
    }
    Ok(JsValue::Object(out))
}

/// `z.record(z.string(), z.unknown())` and `z.record(z.string(), z.any())`:
/// values pass unchanged; only the record's own `__proto__` key is dropped.
fn any_record(value: &JsValue, field: &'static str) -> Result<JsValue, RecordError> {
    let map = value.as_object().ok_or(fail(field, "record"))?;
    let mut out = Object::new();
    for (key, entry) in map.iter().filter(|(key, _)| *key != PROTO_KEY) {
        out.insert(key, entry.clone());
    }
    Ok(JsValue::Object(out))
}

enum JsonWork<'a> {
    Visit(&'a JsValue),
    Array(usize),
    Object(Vec<String>),
}

/// `z.json()`: rejects non-finite numbers and drops `__proto__` keys at every
/// object level. Walks with an explicit stack.
///
/// Divergence (DIV-001 family): zod parses `z.json()` recursively and throws
/// a `RangeError` near 10,000 nesting levels (3,000 passes on node 22.20.0), so
/// the baseline skips such a record at load. This walk has no depth limit
/// and loads it.
fn json_value(root: &JsValue) -> Option<JsValue> {
    let mut work = vec![JsonWork::Visit(root)];
    let mut built: Vec<JsValue> = Vec::new();
    while let Some(step) = work.pop() {
        match step {
            JsonWork::Visit(value) => match value {
                JsValue::Number(number) if !number.is_finite() => return None,
                JsValue::Array(items) => {
                    work.push(JsonWork::Array(items.len()));
                    work.extend(items.iter().rev().map(JsonWork::Visit));
                }
                JsValue::Object(object) => {
                    let kept: Vec<(&str, &JsValue)> =
                        object.iter().filter(|(key, _)| *key != PROTO_KEY).collect();
                    work.push(JsonWork::Object(
                        kept.iter().map(|(key, _)| (*key).to_owned()).collect(),
                    ));
                    work.extend(kept.iter().rev().map(|(_, item)| JsonWork::Visit(item)));
                }
                scalar => built.push(scalar.clone()),
            },
            JsonWork::Array(length) => {
                let items = built.split_off(built.len() - length);
                built.push(JsValue::Array(items));
            }
            JsonWork::Object(keys) => {
                let values = built.split_off(built.len() - keys.len());
                let mut object = Object::new();
                for (key, item) in keys.into_iter().zip(values) {
                    object.insert(key, item);
                }
                built.push(JsValue::Object(object));
            }
        }
    }
    built.pop()
}

/// `z.record(z.string(), z.json())`.
fn json_record(value: &JsValue, field: &'static str) -> Result<JsValue, RecordError> {
    let invalid = || fail(field, "record<string, JSON>");
    let map = value.as_object().ok_or_else(invalid)?;
    let mut out = Object::new();
    for (key, entry) in map.iter().filter(|(key, _)| *key != PROTO_KEY) {
        out.insert(key, json_value(entry).ok_or_else(invalid)?);
    }
    Ok(JsValue::Object(out))
}

/// `SERIALIZABLE_CONFIG_SCHEMA`, `.nullable().optional()`.
fn parse_config(value: &JsValue) -> Result<JsValue, RecordError> {
    if value.is_null() {
        return Ok(JsValue::Null);
    }
    let input = value.as_object().ok_or(fail("config", "object | null"))?;
    let mut out = Object::new();
    nullish_string(input, &mut out, "modeId")?;
    nullish_string(input, &mut out, "model")?;
    nullish_string(input, &mut out, "thinkingOptionId")?;
    nullish_record(input, &mut out, "featureValues", any_record)?;
    nullish_record(input, &mut out, "providerOptions", json_record)?;
    if let Some(policy) = input.get("toolPolicy") {
        out.insert("toolPolicy".to_owned(), parse_tool_policy(policy)?);
    }
    nullish_string(input, &mut out, "systemPrompt")?;
    nullish_record(input, &mut out, "mcpServers", any_record)?;
    Ok(JsValue::Object(out))
}

fn nullish_record(
    input: &Object,
    out: &mut Object,
    field: &'static str,
    parse_record: fn(&JsValue, &'static str) -> Result<JsValue, RecordError>,
) -> Result<(), RecordError> {
    match input.get(field) {
        None => Ok(()),
        Some(JsValue::Null) => {
            out.insert(field.to_owned(), JsValue::Null);
            Ok(())
        }
        Some(value) => {
            out.insert(field.to_owned(), parse_record(value, field)?);
            Ok(())
        }
    }
}

/// `{ preapproved: [{ kind: "mcp", server, tool }.strict()] }.strict()`, nullable.
fn parse_tool_policy(value: &JsValue) -> Result<JsValue, RecordError> {
    if value.is_null() {
        return Ok(JsValue::Null);
    }
    let error = fail("config.toolPolicy", "strict tool policy");
    let input = value.as_object().ok_or(error.clone())?;
    // `.strict()` checks unknown keys with `in`-style enumeration that never
    // sees an own `__proto__` key, so that key is ignored, not rejected.
    if input
        .iter()
        .any(|(key, _)| key != "preapproved" && key != PROTO_KEY)
    {
        return Err(error);
    }
    let entries = input
        .get("preapproved")
        .and_then(JsValue::as_array)
        .ok_or(error.clone())?;
    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let object = entry.as_object().ok_or(error.clone())?;
        let own_keys = object.iter().filter(|(key, _)| *key != PROTO_KEY).count();
        let valid = own_keys == 3
            && object.get("kind").and_then(JsValue::as_str) == Some("mcp")
            && object.get("server").is_some_and(JsValue::is_string)
            && object.get("tool").is_some_and(JsValue::is_string);
        if !valid {
            return Err(error);
        }
        let mut out = Object::new();
        out.insert("kind", JsValue::String("mcp".to_owned()));
        for key in ["server", "tool"] {
            out.insert(key, object.get(key).cloned().unwrap_or(JsValue::Null));
        }
        parsed.push(JsValue::Object(out));
    }
    let mut out = Object::new();
    out.insert("preapproved".to_owned(), JsValue::Array(parsed));
    Ok(JsValue::Object(out))
}

fn parse_runtime_info(value: &JsValue) -> Result<JsValue, RecordError> {
    let input = value.as_object().ok_or(fail("runtimeInfo", "object"))?;
    let mut out = Object::new();
    string(input, &mut out, "provider")?;
    match input.get("sessionId") {
        Some(id @ (JsValue::String(_) | JsValue::Null)) => {
            out.insert("sessionId".to_owned(), id.clone());
        }
        _ => return Err(fail("runtimeInfo.sessionId", "string | null")),
    }
    nullish_string(input, &mut out, "model")?;
    nullish_string(input, &mut out, "thinkingOptionId")?;
    nullish_string(input, &mut out, "modeId")?;
    if let Some(extra) = input.get("extra") {
        out.insert("extra".to_owned(), any_record(extra, "runtimeInfo.extra")?);
    }
    Ok(JsValue::Object(out))
}

/// `AgentFeatureSchema`: toggle or select, discriminated by `type`.
fn parse_feature(value: &JsValue) -> Result<JsValue, RecordError> {
    let input = value.as_object().ok_or(fail("features[]", "object"))?;
    let kind = input.get("type").and_then(JsValue::as_str);
    let mut out = Object::new();
    match kind {
        Some("toggle") => {
            out.insert("type".to_owned(), JsValue::String("toggle".to_owned()));
            feature_common(input, &mut out)?;
            match input.get("value") {
                Some(JsValue::Bool(flag)) => {
                    out.insert("value".to_owned(), JsValue::Bool(*flag));
                }
                _ => return Err(fail("features[].value", "boolean")),
            }
        }
        Some("select") => {
            out.insert("type".to_owned(), JsValue::String("select".to_owned()));
            feature_common(input, &mut out)?;
            match input.get("value") {
                Some(value @ (JsValue::String(_) | JsValue::Null)) => {
                    out.insert("value".to_owned(), value.clone());
                }
                _ => return Err(fail("features[].value", "string | null")),
            }
            let options = input
                .get("options")
                .and_then(JsValue::as_array)
                .ok_or(fail("features[].options", "array"))?;
            out.insert(
                "options".to_owned(),
                JsValue::Array(
                    options
                        .iter()
                        .map(parse_select_option)
                        .collect::<Result<_, _>>()?,
                ),
            );
        }
        _ => return Err(fail("features[].type", "\"toggle\" | \"select\"")),
    }
    Ok(JsValue::Object(out))
}

fn feature_common(input: &Object, out: &mut Object) -> Result<(), RecordError> {
    string(input, out, "id")?;
    string(input, out, "label")?;
    optional_string(input, out, "description")?;
    optional_string(input, out, "tooltip")?;
    optional_string(input, out, "icon")
}

fn parse_select_option(value: &JsValue) -> Result<JsValue, RecordError> {
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
            "metadata".to_owned(),
            any_record(metadata, "features[].options[].metadata")?,
        );
    }
    Ok(JsValue::Object(out))
}

/// `PERSISTENCE_HANDLE_SCHEMA`, `.nullable().optional()`.
fn parse_persistence(value: &JsValue) -> Result<JsValue, RecordError> {
    if value.is_null() {
        return Ok(JsValue::Null);
    }
    let input = value
        .as_object()
        .ok_or(fail("persistence", "object | null"))?;
    let mut out = Object::new();
    string(input, &mut out, "provider")?;
    string(input, &mut out, "sessionId")?;
    if let Some(handle) = input.get("nativeHandle") {
        out.insert("nativeHandle".to_owned(), handle.clone());
    }
    if let Some(metadata) = input.get("metadata") {
        out.insert(
            "metadata".to_owned(),
            any_record(metadata, "persistence.metadata")?,
        );
    }
    Ok(JsValue::Object(out))
}

/// `AgentOwnerSchema`: only `{ kind: "daemon", daemonId, executionId }`.
fn parse_owner(value: &JsValue) -> Result<JsValue, RecordError> {
    let input = value.as_object().ok_or(fail("owner", "object"))?;
    if input.get("kind").and_then(JsValue::as_str) != Some("daemon") {
        return Err(fail("owner.kind", "\"daemon\""));
    }
    let mut out = Object::new();
    out.insert("kind", JsValue::String("daemon".to_owned()));
    string(input, &mut out, "daemonId")?;
    string(input, &mut out, "executionId")?;
    Ok(JsValue::Object(out))
}

/// One loaded agent record and the file it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAgentRecord {
    pub record: JsValue,
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
    deleting: HashSet<String>,
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
            deleting: HashSet::new(),
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
                    let Some(id) = record
                        .get("id")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned)
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

    /// Node `fs.readdir` (libuv `scandir`) lists names sorted by bytes.
    fn scan(&self) -> Vec<PathBuf> {
        let mut root_files = Vec::new();
        let mut directories = Vec::new();
        for (path, kind) in sorted_entries(&self.base) {
            if kind.is_file() && is_json_name(&path) {
                root_files.push(path);
            } else if kind.is_dir() {
                directories.push(path);
            }
        }
        for directory in directories {
            for (path, kind) in sorted_entries(&directory) {
                if kind.is_file() && is_json_name(&path) {
                    root_files.push(path);
                }
            }
        }
        root_files
    }

    fn index(&mut self, id: &str, record: JsValue, path: PathBuf) {
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
    pub fn list(&mut self) -> Vec<JsValue> {
        self.initialize();
        self.order
            .iter()
            .filter_map(|id| self.records.get(id))
            .map(|loaded| loaded.record.clone())
            .collect()
    }

    pub fn get(&mut self, id: &str) -> Option<JsValue> {
        self.initialize();
        self.records.get(id).map(|loaded| loaded.record.clone())
    }

    /// Writes `record` as built by the caller and removes the previous file
    /// when the working directory key changed. Returns `None` without
    /// writing once a delete has begun for the id, as the baseline write
    /// queue does (`agent-storage.ts:168`).
    ///
    /// # Errors
    ///
    /// Returns an error when the record lacks `id` or `cwd` strings, or the write fails.
    pub fn write(&mut self, record: JsValue) -> Result<Option<PathBuf>, StoreError> {
        self.initialize();
        let id = record
            .get("id")
            .and_then(JsValue::as_str)
            .ok_or(StoreError::MissingString("id"))?;
        if self.deleting.contains(id) {
            return Ok(None);
        }
        self.write_record(record).map(Some)
    }

    /// `writeRecord`: writes, re-indexes, and unlinks the previous file of
    /// the same id when its path changed. Ignores the delete tombstone.
    ///
    /// # Errors
    ///
    /// Returns an error when the record lacks `id` or `cwd` strings, or the write fails.
    pub fn write_record(&mut self, record: JsValue) -> Result<PathBuf, StoreError> {
        self.initialize();
        let id = record
            .get("id")
            .and_then(JsValue::as_str)
            .ok_or(StoreError::MissingString("id"))?
            .to_owned();
        let next = write_record_file(&self.base, &record)?;
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

    /// `beginDelete`: every later write for `id` is skipped for the life of
    /// this store, even after the delete finishes.
    pub fn begin_delete(&mut self, id: &str) {
        self.deleting.insert(id.to_owned());
    }

    /// `deleting.has(id)`: a delete has begun, so queued writes for `id` are
    /// skipped before their record is built.
    #[must_use]
    pub fn is_deleting(&self, id: &str) -> bool {
        self.deleting.contains(id)
    }

    /// `remove`: marks the id deleting, unlinks every file indexed for it,
    /// and drops it from the cache. Unlink failures other than not-found do
    /// not fail the call; they are returned for the caller to log, as the
    /// baseline logs `"Failed to remove agent record file"`.
    #[must_use = "unlink failures are returned for the caller to log"]
    pub fn remove(&mut self, id: &str) -> Vec<(PathBuf, io::Error)> {
        self.initialize();
        self.begin_delete(id);
        let mut failures = Vec::new();
        for path in self.paths.remove(id).unwrap_or_default() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => failures.push((path, error)),
            }
        }
        self.records.remove(id);
        self.order.retain(|existing| existing != id);
        failures
    }
}

/// Writes one record at `<base>/<cwd-key>/<id>.json` as
/// `JSON.stringify(record, null, 2)`, atomically.
///
/// # Errors
///
/// Returns an error when the record lacks `id` or `cwd` strings, or the write fails.
pub fn write_record_file(base: &Path, record: &JsValue) -> Result<PathBuf, StoreError> {
    let id = record
        .get("id")
        .and_then(JsValue::as_str)
        .ok_or(StoreError::MissingString("id"))?;
    let cwd = record
        .get("cwd")
        .and_then(JsValue::as_str)
        .ok_or(StoreError::MissingString("cwd"))?;
    // `cwd` and `id` are JavaScript text; node encodes the path to UTF-8.
    let path = base
        .join(js_text_to_utf8(&cwd_key(cwd)))
        .join(js_text_to_utf8(&format!("{id}.json")));
    write_json_atomic(&path, &stringify_pretty(record))?;
    Ok(path)
}

/// The entries of `directory` in `fs.readdir` order: libuv sorts them with
/// `strcmp` on the raw name bytes (`uv__fs_scandir_sort` in
/// `src/unix/fs.c`, libuv 1.51.0 as shipped in node v22.20.0), on every
/// Unix file system and in every locale. Names compare as unsigned bytes,
/// valid UTF-8 or not.
// ponytail: libuv's Windows `fs__scandir` does not sort, so on Windows node's
// order is the file system's; this port keeps the Unix order there too.
fn sorted_entries(directory: &Path) -> Vec<(PathBuf, fs::FileType)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut listed: Vec<(PathBuf, fs::FileType)> = entries
        .flatten()
        .filter_map(|entry| entry.file_type().ok().map(|kind| (entry.path(), kind)))
        .collect();
    listed.sort_by(|(left, _), (right, _)| {
        let name = |path: &PathBuf| {
            path.file_name()
                .map(|name| name.as_encoded_bytes().to_vec())
        };
        name(left).cmp(&name(right))
    });
    listed
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

fn read_record(path: &Path) -> Result<JsValue, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let parsed = parse(&String::from_utf8_lossy(&bytes)).map_err(|error| error.to_string())?;
    parse_stored_agent_record(&parsed).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    use super::{sorted_entries, write_record_file};
    use crate::js_value::{JsObject, JsValue, js_text_from_utf16};

    /// A record whose `cwd` and `id` hold a lone surrogate is written under
    /// the UTF-8 names node's `fs` gives it: U+FFFD.
    #[test]
    fn record_path_encodes_a_lone_surrogate_as_node_does() {
        let base = std::env::temp_dir().join(format!("spocky-record-js-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let lone = js_text_from_utf16(&[0xD800]);
        let mut record = JsObject::new();
        record.insert("id", JsValue::String(format!("a{lone}")));
        record.insert("cwd", JsValue::String(format!("/w/{lone}")));
        record.insert("provider", JsValue::String("codex".to_owned()));
        let path = write_record_file(&base, &JsValue::Object(record)).expect("written");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("a\u{FFFD}.json")
        );
        assert!(path.is_file());
        assert!(
            path.parent()
                .and_then(|directory| directory.file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains('\u{FFFD}'))
        );
        std::fs::remove_dir_all(&base).expect("cleanup");
    }

    /// libuv's `uv__fs_scandir_sort` is `strcmp` on the raw name bytes: no
    /// locale, no case folding, and names that are not UTF-8 sort by byte.
    #[test]
    fn directory_entries_sort_by_raw_name_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "spocky-sorted-entries-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("directory");
        let mut names: Vec<Vec<u8>> = ["zeta", "_us", "Beta", "alpha", "\u{e9}", "a-b", "ab"]
            .iter()
            .map(|name| name.as_bytes().to_vec())
            .collect();
        // APFS only accepts UTF-8 names; other file systems take any bytes.
        if cfg!(not(target_vendor = "apple")) {
            names.push(b"\xff\xfe".to_vec());
        }
        for name in &names {
            let path = directory.join(std::ffi::OsStr::from_bytes(name));
            std::fs::write(path, b"").expect("file");
        }
        let listed: Vec<Vec<u8>> = sorted_entries(&directory)
            .iter()
            .map(|(path, _)| path.file_name().expect("name").as_bytes().to_vec())
            .collect();
        std::fs::remove_dir_all(&directory).expect("remove");
        names.sort();
        assert_eq!(listed, names);
        assert_eq!(listed[0], b"Beta");
        assert_eq!(listed[1], b"_us");
    }
}
