//! File-backed project and workspace registries.
//!
//! Mirrors pinned Paseo `workspace-registry.ts`: `$PASEO_HOME/projects/projects.json`
//! and `$PASEO_HOME/projects/workspaces.json` each hold one JSON array written
//! by `JSON.stringify(records, null, 2)` through an atomic temp file and
//! rename. Files are read with `JSON.parse` semantics ([`crate::js_value`])
//! and records with the zod schema semantics of the baseline: unknown keys
//! are dropped, keys are re-emitted in schema order, and any invalid record
//! makes the whole file load as empty. The baseline then logs the failure
//! and keeps accepting mutations, so the next write replaces the file; this
//! port does the same and keeps the failure for the caller to log. The cache
//! keeps JavaScript `Map` semantics: entries are keyed by the id they were
//! stored under, in insertion order. String fields hold JavaScript text.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use spocky_contracts::js::{self, js_sort_by, locale_compare};

use crate::StoreError;
use crate::atomic::write_json_atomic;
use crate::js_value::{JsObject, JsValue, parse, stringify_pretty};
use crate::path_compare::are_equivalent_paths;

/// A zod validation failure for one registry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError {
    pub field: &'static str,
    pub expected: &'static str,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "field '{}' expected {}",
            self.field, self.expected
        )
    }
}

impl std::error::Error for RecordError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Git,
    NonGit,
}

impl ProjectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::NonGit => "non_git",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "git" => Some(Self::Git),
            "non_git" => Some(Self::NonGit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
    LocalCheckout,
    Worktree,
    Directory,
}

impl WorkspaceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalCheckout => "local_checkout",
            Self::Worktree => "worktree",
            Self::Directory => "directory",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "local_checkout" => Some(Self::LocalCheckout),
            "worktree" => Some(Self::Worktree),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

/// `UntrustedWorkspaceSourceSchema`: `kind` is always `"change_request"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrustedWorkspaceSource {
    pub forge: String,
    pub number: u64,
    pub head_repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedProjectRecord {
    pub project_id: String,
    pub root_path: String,
    pub kind: ProjectKind,
    pub display_name: String,
    pub project_key: Option<String>,
    pub custom_name: Option<String>,
    pub custom_icon_revision: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedWorkspaceRecord {
    pub workspace_id: String,
    pub project_id: String,
    pub cwd: String,
    pub kind: WorkspaceKind,
    pub display_name: String,
    pub title: Option<String>,
    pub branch: Option<String>,
    pub worktree_root: Option<String>,
    pub base_branch: Option<String>,
    pub is_paseo_owned_worktree: bool,
    pub main_repo_root: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub auto_archived_change_request_url: Option<String>,
    pub pinned_at: Option<String>,
    pub labels: Option<Vec<String>>,
    pub untrusted_source: Option<UntrustedWorkspaceSource>,
}

/// One registry record type with its zod schema and stable id.
pub trait RegistryRecord: Clone {
    fn id(&self) -> &str;

    /// Applies the record's zod schema to one parsed JSON value.
    ///
    /// # Errors
    ///
    /// Returns the first field that fails validation.
    fn from_value(value: &JsValue) -> Result<Self, RecordError>;

    /// Emits the record in schema key order, as zod output.
    fn to_value(&self) -> JsValue;

    /// Returns a copy with `updatedAt` and `archivedAt` set to `archived_at`.
    #[must_use]
    fn archived(&self, archived_at: &str) -> Self;

    fn archived_at(&self) -> Option<&str>;
}

impl RegistryRecord for PersistedProjectRecord {
    fn id(&self) -> &str {
        &self.project_id
    }

    fn from_value(value: &JsValue) -> Result<Self, RecordError> {
        let object = as_object(value, "record")?;
        let project_id = required_string(object, "projectId")?;
        let root_path = required_string(object, "rootPath")?;
        let kind = ProjectKind::parse(&required_string(object, "kind")?).ok_or(RecordError {
            field: "kind",
            expected: "\"git\" | \"non_git\"",
        })?;
        Ok(Self {
            project_id,
            root_path,
            kind,
            display_name: required_string(object, "displayName")?,
            project_key: nullish_string(object, "projectKey")?,
            custom_name: nullish_string(object, "customName")?,
            custom_icon_revision: nullish_string(object, "customIconRevision")?,
            created_at: required_string(object, "createdAt")?,
            updated_at: required_string(object, "updatedAt")?,
            archived_at: nullable_string(object, "archivedAt")?,
        })
    }

    fn to_value(&self) -> JsValue {
        let mut map = JsObject::new();
        map.insert("projectId", text(&self.project_id));
        map.insert("rootPath", text(&self.root_path));
        map.insert("kind", text(self.kind.as_str()));
        map.insert("displayName", text(&self.display_name));
        map.insert("projectKey", option_value(self.project_key.as_ref()));
        map.insert("customName", option_value(self.custom_name.as_ref()));
        map.insert(
            "customIconRevision",
            option_value(self.custom_icon_revision.as_ref()),
        );
        map.insert("createdAt", text(&self.created_at));
        map.insert("updatedAt", text(&self.updated_at));
        map.insert("archivedAt", option_value(self.archived_at.as_ref()));
        JsValue::Object(map)
    }

    fn archived(&self, archived_at: &str) -> Self {
        Self {
            updated_at: archived_at.to_owned(),
            archived_at: Some(archived_at.to_owned()),
            ..self.clone()
        }
    }

    fn archived_at(&self) -> Option<&str> {
        self.archived_at.as_deref()
    }
}

impl RegistryRecord for PersistedWorkspaceRecord {
    fn id(&self) -> &str {
        &self.workspace_id
    }

    fn from_value(value: &JsValue) -> Result<Self, RecordError> {
        let object = as_object(value, "record")?;
        let workspace_id = required_string(object, "workspaceId")?;
        let project_id = required_string(object, "projectId")?;
        let cwd = required_string(object, "cwd")?;
        let kind = WorkspaceKind::parse(&required_string(object, "kind")?).ok_or(RecordError {
            field: "kind",
            expected: "\"local_checkout\" | \"worktree\" | \"directory\"",
        })?;
        Ok(Self {
            workspace_id,
            project_id,
            cwd,
            kind,
            display_name: required_string(object, "displayName")?,
            title: nullish_string(object, "title")?,
            branch: nullish_string(object, "branch")?,
            worktree_root: nullish_string(object, "worktreeRoot")?,
            base_branch: nullish_string(object, "baseBranch")?,
            is_paseo_owned_worktree: defaulted_bool(object, "isPaseoOwnedWorktree")?,
            main_repo_root: nullish_string(object, "mainRepoRoot")?,
            created_at: required_string(object, "createdAt")?,
            updated_at: required_string(object, "updatedAt")?,
            archived_at: nullable_string(object, "archivedAt")?,
            auto_archived_change_request_url: nullish_string(
                object,
                "autoArchivedChangeRequestUrl",
            )?,
            pinned_at: nullish_string(object, "pinnedAt")?,
            labels: optional_string_array(object, "labels")?,
            untrusted_source: optional_untrusted_source(object)?,
        })
    }

    fn to_value(&self) -> JsValue {
        let mut map = JsObject::new();
        map.insert("workspaceId", text(&self.workspace_id));
        map.insert("projectId", text(&self.project_id));
        map.insert("cwd", text(&self.cwd));
        map.insert("kind", text(self.kind.as_str()));
        map.insert("displayName", text(&self.display_name));
        map.insert("title", option_value(self.title.as_ref()));
        map.insert("branch", option_value(self.branch.as_ref()));
        map.insert("worktreeRoot", option_value(self.worktree_root.as_ref()));
        map.insert("baseBranch", option_value(self.base_branch.as_ref()));
        map.insert(
            "isPaseoOwnedWorktree",
            JsValue::Bool(self.is_paseo_owned_worktree),
        );
        map.insert("mainRepoRoot", option_value(self.main_repo_root.as_ref()));
        map.insert("createdAt", text(&self.created_at));
        map.insert("updatedAt", text(&self.updated_at));
        map.insert("archivedAt", option_value(self.archived_at.as_ref()));
        map.insert(
            "autoArchivedChangeRequestUrl",
            option_value(self.auto_archived_change_request_url.as_ref()),
        );
        map.insert("pinnedAt", option_value(self.pinned_at.as_ref()));
        if let Some(labels) = &self.labels {
            map.insert(
                "labels",
                JsValue::Array(labels.iter().map(|label| text(label)).collect()),
            );
        }
        if let Some(source) = &self.untrusted_source {
            let mut nested = JsObject::new();
            nested.insert("kind", text("change_request"));
            nested.insert("forge", text(&source.forge));
            #[allow(
                clippy::cast_precision_loss,
                reason = "parsed numbers never exceed Number.MAX_SAFE_INTEGER"
            )]
            nested.insert("number", JsValue::Number(source.number as f64));
            nested.insert("headRepository", text(&source.head_repository));
            map.insert("untrustedSource", JsValue::Object(nested));
        }
        JsValue::Object(map)
    }

    fn archived(&self, archived_at: &str) -> Self {
        Self {
            updated_at: archived_at.to_owned(),
            archived_at: Some(archived_at.to_owned()),
            ..self.clone()
        }
    }

    fn archived_at(&self) -> Option<&str> {
        self.archived_at.as_deref()
    }
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn as_object<'a>(value: &'a JsValue, field: &'static str) -> Result<&'a JsObject, RecordError> {
    value.as_object().ok_or(RecordError {
        field,
        expected: "object",
    })
}

fn option_value(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Null, |inner| JsValue::String(inner.clone()))
}

/// `z.string()`.
fn required_string(object: &JsObject, field: &'static str) -> Result<String, RecordError> {
    match object.get(field) {
        Some(JsValue::String(value)) => Ok(value.clone()),
        _ => Err(RecordError {
            field,
            expected: "string",
        }),
    }
}

/// `z.string().nullable()`: the key is required.
fn nullable_string(object: &JsObject, field: &'static str) -> Result<Option<String>, RecordError> {
    match object.get(field) {
        Some(JsValue::String(value)) => Ok(Some(value.clone())),
        Some(JsValue::Null) => Ok(None),
        _ => Err(RecordError {
            field,
            expected: "string | null",
        }),
    }
}

/// `z.string().nullable().optional()` with `?? null`, and
/// `z.string().nullable().default(null)`: missing and `null` both become null.
fn nullish_string(object: &JsObject, field: &'static str) -> Result<Option<String>, RecordError> {
    match object.get(field) {
        None | Some(JsValue::Null) => Ok(None),
        Some(JsValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(RecordError {
            field,
            expected: "string | null | undefined",
        }),
    }
}

/// `z.boolean().default(false)`.
fn defaulted_bool(object: &JsObject, field: &'static str) -> Result<bool, RecordError> {
    match object.get(field) {
        None => Ok(false),
        Some(JsValue::Bool(value)) => Ok(*value),
        Some(_) => Err(RecordError {
            field,
            expected: "boolean | undefined",
        }),
    }
}

/// `z.array(z.string()).optional()`.
fn optional_string_array(
    object: &JsObject,
    field: &'static str,
) -> Result<Option<Vec<String>>, RecordError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let error = RecordError {
        field,
        expected: "string[] | undefined",
    };
    let items = value.as_array().ok_or_else(|| error.clone())?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| error.clone())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// JavaScript `Number.MAX_SAFE_INTEGER`, the upper bound of zod v4 `.int()`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn optional_untrusted_source(
    object: &JsObject,
) -> Result<Option<UntrustedWorkspaceSource>, RecordError> {
    let Some(value) = object.get("untrustedSource") else {
        return Ok(None);
    };
    let nested = as_object(value, "untrustedSource")?;
    if nested.get("kind").and_then(JsValue::as_str) != Some("change_request") {
        return Err(RecordError {
            field: "untrustedSource.kind",
            expected: "\"change_request\"",
        });
    }
    let forge = required_string(nested, "forge")?;
    let number = nested
        .get("number")
        .and_then(JsValue::as_f64)
        .filter(|number| number.fract() == 0.0 && *number >= 1.0 && *number <= MAX_SAFE_INTEGER)
        .ok_or(RecordError {
            field: "untrustedSource.number",
            expected: "positive safe integer",
        })?;
    let head_repository = required_string(nested, "headRepository")?;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range checked against 1..=Number.MAX_SAFE_INTEGER above"
    )]
    let number = number as u64;
    Ok(Some(UntrustedWorkspaceSource {
        forge,
        number,
        head_repository,
    }))
}

/// Parses a whole registry file as `z.array(schema).parse(JSON.parse(raw))`.
///
/// # Errors
///
/// Returns an error when the text is not JSON, not an array, or any record fails.
pub fn parse_registry_file<R: RegistryRecord>(raw: &str) -> Result<Vec<R>, StoreError> {
    let value = parse(raw).map_err(StoreError::JsonSyntax)?;
    let items = value
        .as_array()
        .ok_or(StoreError::InvalidRecord(RecordError {
            field: "root",
            expected: "array",
        }))?;
    items
        .iter()
        .map(|item| R::from_value(item).map_err(StoreError::InvalidRecord))
        .collect()
}

/// Renders records exactly as `JSON.stringify(records, null, 2)`.
#[must_use]
pub fn render_registry_file<R: RegistryRecord>(records: &[R]) -> String {
    stringify_pretty(&JsValue::Array(
        records.iter().map(RegistryRecord::to_value).collect(),
    ))
}

/// `writeRecords`: writes the rendered registry file text to `path`.
pub type RecordWriter = Box<dyn FnMut(&Path, &str) -> Result<(), StoreError> + Send>;

/// `FileBackedRegistry`: lazy load, `Map`-keyed cache, write on change.
pub struct FileRegistry<R: RegistryRecord> {
    path: PathBuf,
    /// `(key, record)` in `Map` insertion order. The key is the id the
    /// record was stored under, which an updater may later change.
    cache: Option<Vec<(String, R)>>,
    load_failure: Option<StoreError>,
    /// `mutationsBlockedUntilRestart`.
    blocked: bool,
    /// The `writeRecords` option; `None` is `writeJsonFileAtomic`.
    writer: Option<RecordWriter>,
}

impl<R: RegistryRecord + std::fmt::Debug> std::fmt::Debug for FileRegistry<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileRegistry")
            .field("path", &self.path)
            .field("cache", &self.cache)
            .field("load_failure", &self.load_failure)
            .field("blocked", &self.blocked)
            .finish_non_exhaustive()
    }
}

/// The records a [`FileRegistry::commit_staged`] planner reads: the cache as
/// the `Map` the baseline passes it, in insertion order.
#[derive(Debug)]
pub struct RegistryView<'a, R>(&'a [(String, R)]);

impl<R> Clone for RegistryView<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for RegistryView<'_, R> {}

impl<'a, R> RegistryView<'a, R> {
    /// `Map.get(id)`.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&'a R> {
        self.0
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, record)| record)
    }

    /// `Map.values()`.
    pub fn values(&self) -> impl Iterator<Item = &'a R> {
        self.0.iter().map(|(_, record)| record)
    }
}

/// What a [`FileRegistry::commit_staged`] planner stages: the records to set,
/// the caller's result, and whether to run the write hooks even when no record
/// changes.
#[derive(Debug)]
pub struct StagedCommit<R, T> {
    pub updates: Vec<R>,
    pub result: T,
    pub force_persist: bool,
}

impl<R: RegistryRecord> FileRegistry<R> {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cache: None,
            load_failure: None,
            blocked: false,
            writer: None,
        }
    }

    /// The `writeRecords` constructor option.
    #[must_use]
    pub fn with_writer(mut self, writer: RecordWriter) -> Self {
        self.writer = Some(writer);
        self
    }

    /// `freezeMutationsUntilRestart`: every later mutation fails.
    pub fn block_mutations_until_restart(&mut self) {
        self.blocked = true;
    }

    fn ensure_unblocked(&self) -> Result<(), StoreError> {
        if self.blocked {
            return Err(StoreError::MutationsBlocked);
        }
        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn exists_on_disk(&self) -> bool {
        self.path.exists()
    }

    /// Loads the file once. A missing file yields an empty cache silently;
    /// an unreadable or invalid one yields an empty cache and is kept in
    /// [`Self::load_failure`], where the baseline logs
    /// `"Failed to load registry file"`.
    pub fn initialize(&mut self) -> Option<&StoreError> {
        if self.cache.is_none() {
            let (records, failure) = match fs::read(&self.path) {
                Ok(bytes) => match parse_registry_file::<R>(&String::from_utf8_lossy(&bytes)) {
                    Ok(records) => (keyed(records), None),
                    Err(error) => (Vec::new(), Some(error)),
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), None),
                Err(source) => (
                    Vec::new(),
                    Some(StoreError::Io {
                        operation: "read registry file",
                        source,
                    }),
                ),
            };
            self.cache = Some(records);
            self.load_failure = failure;
        }
        self.load_failure.as_ref()
    }

    /// The failure from the one load, if the file was unreadable or invalid.
    #[must_use]
    pub const fn load_failure(&self) -> Option<&StoreError> {
        self.load_failure.as_ref()
    }

    fn entries(&mut self) -> &mut Vec<(String, R)> {
        self.initialize();
        self.cache.get_or_insert_with(Vec::new)
    }

    pub fn list(&mut self) -> Vec<R> {
        self.entries()
            .iter()
            .map(|(_, record)| record.clone())
            .collect()
    }

    /// `Map.get(id)`: looks up by stored key.
    pub fn get(&mut self, id: &str) -> Option<R> {
        self.entries()
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, record)| record.clone())
    }

    /// Sets one record under its id, keeping the position of an existing key.
    ///
    /// # Errors
    ///
    /// Returns an error if the schema rejects the record or the atomic write
    /// fails; the cache is then unchanged.
    pub fn upsert(&mut self, record: R) -> Result<(), StoreError> {
        let record = schema_parse(record)?;
        self.ensure_unblocked()?;
        let mut staged = self.entries().clone();
        set_entry(&mut staged, record.id().to_owned(), record);
        self.commit(staged)
    }

    /// Replaces the record stored under `id` through `updater`. The result
    /// stays under `id` at the same position even if the updater changed
    /// the record's own id, as `records.set(id, next)` does.
    ///
    /// # Errors
    ///
    /// Returns an error if the schema rejects the result or the atomic write fails.
    pub fn update(
        &mut self,
        id: &str,
        updater: impl FnOnce(&R) -> R,
    ) -> Result<Option<R>, StoreError> {
        self.ensure_unblocked()?;
        let mut staged = self.entries().clone();
        let Some(existing) = staged
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, record)| record)
        else {
            return Ok(None);
        };
        let next = schema_parse(updater(existing))?;
        set_entry(&mut staged, id.to_owned(), next.clone());
        self.commit(staged)?;
        Ok(Some(next))
    }

    /// `archiveIfPresent`.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn archive_if_present(
        &mut self,
        id: &str,
        archived_at: &str,
    ) -> Result<Option<R>, StoreError> {
        self.update(id, |existing| existing.archived(archived_at))
    }

    /// `archiveIfActive`: an empty `archivedAt` counts as active, as a falsy
    /// value does in the baseline.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn archive_if_active(
        &mut self,
        id: &str,
        archived_at: &str,
    ) -> Result<Option<R>, StoreError> {
        self.ensure_unblocked()?;
        let active = self
            .entries()
            .iter()
            .any(|(key, record)| key == id && record.archived_at().is_none_or(str::is_empty));
        if !active {
            return Ok(None);
        }
        self.archive_if_present(id, archived_at)
    }

    /// `removeIfPresent`.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn remove_if_present(&mut self, id: &str) -> Result<Option<R>, StoreError> {
        self.ensure_unblocked()?;
        let mut staged = self.entries().clone();
        let Some(index) = staged.iter().position(|(key, _)| key == id) else {
            return Ok(None);
        };
        let (_, removed) = staged.remove(index);
        self.commit(staged)?;
        Ok(Some(removed))
    }

    fn commit(&mut self, staged: Vec<(String, R)>) -> Result<(), StoreError> {
        self.write_entries(&staged)?;
        self.cache = Some(staged);
        Ok(())
    }

    fn write_entries(&mut self, entries: &[(String, R)]) -> Result<(), StoreError> {
        let records: Vec<R> = entries.iter().map(|(_, record)| record.clone()).collect();
        let text = render_registry_file(&records);
        match &mut self.writer {
            Some(writer) => writer(&self.path, &text),
            None => write_json_atomic(&self.path, &text),
        }
    }

    /// `mutateCache` with hooks, as `commitWorkspaceLabelMutation` calls it.
    ///
    /// `stage` plans against the cache; each update is validated and set under
    /// its own id in a copy. Nothing is written, and no hook runs, when no
    /// record changes and the plan does not force persistence. Otherwise
    /// `before_write` runs, then the file is written if a record changed, then
    /// `after_write`, then the cache is replaced and `after_commit` runs. A
    /// failure in any step leaves the cache as it was.
    ///
    /// Returns the result and the records that changed, for the caller to
    /// publish as mutations.
    ///
    /// # Errors
    ///
    /// Returns the error of the first step that fails, or
    /// [`StoreError::MutationsBlocked`] before any of them.
    pub fn commit_staged<T, E: From<StoreError>>(
        &mut self,
        stage: impl FnOnce(RegistryView<'_, R>) -> Result<StagedCommit<R, T>, E>,
        before_write: impl FnOnce() -> Result<(), E>,
        after_write: impl FnOnce() -> Result<(), E>,
        after_commit: impl FnOnce(),
    ) -> Result<(T, Vec<R>), E> {
        self.ensure_unblocked()?;
        let staged = stage(RegistryView(self.entries()))?;
        let mut changed = Vec::with_capacity(staged.updates.len());
        for record in staged.updates {
            changed.push(schema_parse(record)?);
        }
        let mut entries = self.entries().clone();
        for record in &changed {
            set_entry(&mut entries, record.id().to_owned(), record.clone());
        }
        // Every parsed update is a new object, so any update changes the map.
        let records_changed = !changed.is_empty();
        if !records_changed && !staged.force_persist {
            return Ok((staged.result, changed));
        }
        before_write()?;
        if records_changed {
            self.write_entries(&entries)?;
        }
        after_write()?;
        if records_changed {
            self.cache = Some(entries);
        }
        after_commit();
        Ok((staged.result, changed))
    }
}

/// `schema.parse(record)`: every write path re-validates the record, so a
/// value the schema rejects (for example a zero `untrustedSource.number`)
/// fails before anything is written.
/// A typed record round-trips unchanged when valid, so the record is returned
/// as given.
fn schema_parse<R: RegistryRecord>(record: R) -> Result<R, StoreError> {
    R::from_value(&record.to_value()).map_err(StoreError::InvalidRecord)?;
    Ok(record)
}

/// `Map.set` semantics: replace in place, otherwise append.
fn set_entry<R>(entries: &mut Vec<(String, R)>, key: String, record: R) {
    match entries.iter().position(|(existing, _)| *existing == key) {
        Some(index) => entries[index].1 = record,
        None => entries.push((key, record)),
    }
}

/// Loading into a `Map` keeps the first position and the last value of a repeated id.
fn keyed<R: RegistryRecord>(records: Vec<R>) -> Vec<(String, R)> {
    let mut entries = Vec::with_capacity(records.len());
    for record in records {
        set_entry(&mut entries, record.id().to_owned(), record);
    }
    entries
}

pub type ProjectRegistry = FileRegistry<PersistedProjectRecord>;
pub type WorkspaceRegistry = FileRegistry<PersistedWorkspaceRecord>;

/// Input to [`ProjectRegistry::get_or_create_active_by_root`].
#[derive(Debug, Clone)]
pub struct ProjectRootInput<'a> {
    pub root_path: &'a str,
    pub kind: ProjectKind,
    pub display_name: &'a str,
    pub project_key: Option<&'a str>,
    pub timestamp: &'a str,
}

/// Outcome of [`ProjectRegistry::get_or_create_active_by_root`]; any variant
/// other than `Existing` was upserted and must be published as a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAllocation {
    Existing(PersistedProjectRecord),
    Refreshed(PersistedProjectRecord),
    Created(PersistedProjectRecord),
}

impl ProjectAllocation {
    #[must_use]
    pub const fn record(&self) -> &PersistedProjectRecord {
        match self {
            Self::Existing(record) | Self::Refreshed(record) | Self::Created(record) => record,
        }
    }
}

impl FileRegistry<PersistedProjectRecord> {
    /// `FileBackedProjectRegistry.archive`: archives only an active project.
    /// Returns the archived record to publish, or `None` when nothing changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn archive(
        &mut self,
        project_id: &str,
        archived_at: &str,
    ) -> Result<Option<PersistedProjectRecord>, StoreError> {
        self.archive_if_active(project_id, archived_at)
    }

    /// Returns the oldest active project at an equivalent root, refreshing its
    /// kind and key when they differ, or creates one with a fresh id.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn get_or_create_active_by_root(
        &mut self,
        input: &ProjectRootInput<'_>,
        mut project_id_factory: impl FnMut() -> String,
    ) -> Result<ProjectAllocation, StoreError> {
        let mut matching: Vec<PersistedProjectRecord> = self
            .list()
            .into_iter()
            .filter(|project| {
                project.archived_at.as_deref().is_none_or(str::is_empty)
                    && are_equivalent_paths(&project.root_path, input.root_path)
            })
            .collect();
        // `.sort(compare)[0]`: with an unparseable date the comparator is not
        // a consistent ordering, so a minimum scan can pick another project.
        js_sort_by(&mut matching, compare_projects);
        if let Some(active) = matching.into_iter().next() {
            if active.kind == input.kind && active.project_key.as_deref() == input.project_key {
                return Ok(ProjectAllocation::Existing(active));
            }
            let refreshed = PersistedProjectRecord {
                kind: input.kind,
                project_key: input.project_key.map(str::to_owned),
                updated_at: input.timestamp.to_owned(),
                ..active
            };
            self.upsert(refreshed.clone())?;
            return Ok(ProjectAllocation::Refreshed(refreshed));
        }
        loop {
            let project_id = project_id_factory();
            if self.get(&project_id).is_some() {
                continue;
            }
            let record = PersistedProjectRecord {
                project_id,
                root_path: input.root_path.to_owned(),
                kind: input.kind,
                display_name: input.display_name.to_owned(),
                project_key: input.project_key.map(str::to_owned),
                custom_name: None,
                custom_icon_revision: None,
                created_at: input.timestamp.to_owned(),
                updated_at: input.timestamp.to_owned(),
                archived_at: None,
            };
            self.upsert(record.clone())?;
            return Ok(ProjectAllocation::Created(record));
        }
    }
}

impl FileRegistry<PersistedWorkspaceRecord> {
    /// `FileBackedWorkspaceRegistry.archive`: stamps `updatedAt` and
    /// `archivedAt` even on an archived record, and records the consumed
    /// change request URL when one is given (a truthy value in the baseline).
    /// Returns the record to publish, or `None` when the id is unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic write fails.
    pub fn archive(
        &mut self,
        workspace_id: &str,
        archived_at: &str,
        auto_archived_change_request_url: Option<&str>,
    ) -> Result<Option<PersistedWorkspaceRecord>, StoreError> {
        self.update(workspace_id, |existing| {
            let mut next = existing.archived(archived_at);
            if let Some(url) = auto_archived_change_request_url.filter(|url| !url.is_empty()) {
                next.auto_archived_change_request_url = Some(url.to_owned());
            }
            next
        })
    }
}

/// `Date.parse(left.createdAt) - Date.parse(right.createdAt) ||
/// left.projectId.localeCompare(right.projectId)`. A `NaN` or zero
/// difference falls through to the id collation.
// Epoch milliseconds are below 2^53, so the `f64` difference is exact.
#[allow(clippy::cast_precision_loss)]
fn compare_projects(left: &PersistedProjectRecord, right: &PersistedProjectRecord) -> f64 {
    let millis = |text: &str| js::date_parse(text).map_or(f64::NAN, |millis| millis as f64);
    let difference = millis(&left.created_at) - millis(&right.created_at);
    if difference != 0.0 && !difference.is_nan() {
        return difference;
    }
    match locale_compare(&left.project_id, &right.project_id) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }
}

/// `resolveProjectDisplayName`.
#[must_use]
pub fn resolve_project_display_name(record: &PersistedProjectRecord) -> &str {
    record
        .custom_name
        .as_deref()
        .unwrap_or(&record.display_name)
}

/// `resolveWorkspaceDisplayName`: the title always wins.
#[must_use]
pub fn resolve_workspace_display_name(record: &PersistedWorkspaceRecord) -> &str {
    record.title.as_deref().unwrap_or(&record.display_name)
}
