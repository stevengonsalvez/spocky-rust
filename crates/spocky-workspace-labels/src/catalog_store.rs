//! `WorkspaceLabelCatalogStore`: the label catalog file, its crash journal,
//! and the one commit that moves catalog and workspace assignments together.
//!
//! A commit stages the plan, writes the `prepared` journal, writes the
//! catalog, writes the workspace file, then rewrites the journal as
//! `committed`. The `committed` rewrite is the commit point. A failure before
//! it rolls the catalog and the touched workspaces back to the journal's
//! before-images; a failure after it, or a rollback that itself fails,
//! freezes every registry mutation until restart.

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify_pretty};
use spocky_contracts::zod::{Schema, UnknownKeys, Verdict, verdict};
use spocky_store::StoreError;
use spocky_store::atomic::{FsError, write_json_atomic};
use spocky_store::registry::{
    PersistedWorkspaceRecord, RegistryView, StagedCommit, WorkspaceRegistry,
};

use crate::error::LabelError;
use crate::names::{WORKSPACE_LABEL_COLORS, WorkspaceLabelColor, WorkspaceLabelDefinition};

/// Runs an operation with the workspace registry, which the daemon shares with
/// its other services.
pub trait RegistryAccess: Send + Sync {
    fn with_registry<T>(&self, operation: impl FnOnce(&mut WorkspaceRegistry) -> T) -> T;
}

impl RegistryAccess for Arc<Mutex<WorkspaceRegistry>> {
    fn with_registry<T>(&self, operation: impl FnOnce(&mut WorkspaceRegistry) -> T) -> T {
        operation(&mut self.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// What a planner decides for one commit.
#[derive(Debug)]
pub struct WorkspaceLabelMutation<T> {
    pub labels: Vec<WorkspaceLabelDefinition>,
    pub workspace_updates: Vec<PersistedWorkspaceRecord>,
    pub result: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionPhase {
    Prepared,
    Committed,
}

impl TransactionPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
        }
    }
}

/// `WorkspaceLabelWorkspaceState`: the label-relevant part of a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelWorkspaceState {
    pub workspace_id: String,
    pub labels: Option<Vec<String>>,
    pub updated_at: String,
}

/// `WorkspaceLabelTransaction`, the journal file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelTransaction {
    pub phase: TransactionPhase,
    pub before_labels: Vec<WorkspaceLabelDefinition>,
    pub after_labels: Vec<WorkspaceLabelDefinition>,
    pub before_workspaces: Vec<WorkspaceLabelWorkspaceState>,
    pub after_workspaces: Vec<WorkspaceLabelWorkspaceState>,
}

/// The `writeCatalog` option.
pub type CatalogWriter =
    Box<dyn FnMut(&Path, &[WorkspaceLabelDefinition]) -> Result<(), LabelError> + Send>;
/// The `writeTransaction` option.
pub type TransactionWriter =
    Box<dyn FnMut(&Path, &WorkspaceLabelTransaction) -> Result<(), LabelError> + Send>;
/// The `removeTransaction` option.
pub type TransactionRemover = Box<dyn FnMut(&Path) -> Result<(), LabelError> + Send>;
/// Receives each workspace a committed label mutation changed, as the
/// registry's mutation listeners receive an `upsert`.
pub type WorkspacePublisher = Box<dyn FnMut(&PersistedWorkspaceRecord) + Send>;

/// The injectable file operations, defaulting to the baseline's.
pub struct CatalogIo {
    pub write_catalog: CatalogWriter,
    pub write_transaction: TransactionWriter,
    pub remove_transaction: TransactionRemover,
}

impl Default for CatalogIo {
    fn default() -> Self {
        Self {
            write_catalog: Box::new(write_catalog_file),
            write_transaction: Box::new(write_transaction_file),
            remove_transaction: Box::new(remove_file),
        }
    }
}

/// `writeJsonFileAtomic(filePath, labels)`.
///
/// # Errors
///
/// Returns the failed write.
pub fn write_catalog_file(
    path: &Path,
    labels: &[WorkspaceLabelDefinition],
) -> Result<(), LabelError> {
    write_json_atomic(path, &stringify_pretty(&catalog_value(labels))).map_err(LabelError::from)
}

/// `writeJsonFileAtomic(filePath, transaction)`.
///
/// # Errors
///
/// Returns the failed write.
pub fn write_transaction_file(
    path: &Path,
    transaction: &WorkspaceLabelTransaction,
) -> Result<(), LabelError> {
    write_json_atomic(path, &stringify_pretty(&transaction_value(transaction)))
        .map_err(LabelError::from)
}

/// `fs.rm(filePath)`: a missing file is an error, as without `force`.
///
/// # Errors
///
/// Returns node's `lstat` or `unlink` error.
pub fn remove_file(path: &Path) -> Result<(), LabelError> {
    let fs_error = |syscall, source| {
        LabelError::Store(StoreError::Fs(FsError {
            syscall,
            path: Some(path.to_string_lossy().into_owned()),
            dest: None,
            source,
        }))
    };
    std::fs::symlink_metadata(path).map_err(|source| fs_error("lstat", source))?;
    std::fs::remove_file(path).map_err(|source| fs_error("unlink", source))
}

fn definition_value(label: &WorkspaceLabelDefinition) -> JsValue {
    let mut object = JsObject::new();
    object.insert("name", JsValue::String(label.name.clone()));
    object.insert("color", JsValue::String(label.color.as_str().to_owned()));
    JsValue::Object(object)
}

fn catalog_value(labels: &[WorkspaceLabelDefinition]) -> JsValue {
    JsValue::Array(labels.iter().map(definition_value).collect())
}

fn state_value(state: &WorkspaceLabelWorkspaceState) -> JsValue {
    let mut object = JsObject::new();
    object.insert("workspaceId", JsValue::String(state.workspace_id.clone()));
    if let Some(labels) = &state.labels {
        object.insert(
            "labels",
            JsValue::Array(labels.iter().cloned().map(JsValue::String).collect()),
        );
    }
    object.insert("updatedAt", JsValue::String(state.updated_at.clone()));
    JsValue::Object(object)
}

fn transaction_value(transaction: &WorkspaceLabelTransaction) -> JsValue {
    let states = |states: &[WorkspaceLabelWorkspaceState]| {
        JsValue::Array(states.iter().map(state_value).collect())
    };
    let mut object = JsObject::new();
    object.insert(
        "phase",
        JsValue::String(transaction.phase.as_str().to_owned()),
    );
    object.insert("beforeLabels", catalog_value(&transaction.before_labels));
    object.insert("afterLabels", catalog_value(&transaction.after_labels));
    object.insert("beforeWorkspaces", states(&transaction.before_workspaces));
    object.insert("afterWorkspaces", states(&transaction.after_workspaces));
    JsValue::Object(object)
}

fn definition_schema() -> Schema {
    Schema::Object(
        vec![
            ("name", Schema::String(Vec::new())),
            ("color", Schema::Enum(&WORKSPACE_LABEL_COLORS)),
        ],
        UnknownKeys::Strip,
    )
}

fn catalog_schema() -> Schema {
    Schema::Array(Box::new(definition_schema()))
}

fn transaction_schema() -> Schema {
    let state = Schema::Object(
        vec![
            ("workspaceId", Schema::String(Vec::new())),
            (
                "labels",
                Schema::Optional(Box::new(Schema::Array(Box::new(
                    Schema::String(Vec::new()),
                )))),
            ),
            ("updatedAt", Schema::String(Vec::new())),
        ],
        UnknownKeys::Strip,
    );
    let states = Schema::Array(Box::new(state));
    Schema::Object(
        vec![
            ("phase", Schema::Enum(&["prepared", "committed"])),
            ("beforeLabels", catalog_schema()),
            ("afterLabels", catalog_schema()),
            ("beforeWorkspaces", states.clone()),
            ("afterWorkspaces", states),
        ],
        UnknownKeys::Strip,
    )
}

/// `schema.parse(JSON.parse(raw))`, with the text of the error it throws.
fn parse_with(schema: &Schema, raw: &str) -> Result<JsValue, LabelError> {
    let value = parse(raw).map_err(|error| LabelError::Storage(error.message))?;
    match verdict(schema, &value) {
        Verdict::Valid(output) => Ok(output),
        Verdict::Invalid(issues) => Err(LabelError::Storage(stringify_pretty(&JsValue::Array(
            issues,
        )))),
        Verdict::Unmodeled | Verdict::TooDeep | Verdict::Throws(_) => Err(LabelError::Storage(
            "Maximum call stack size exceeded".to_owned(),
        )),
    }
}

fn text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn definitions_from(value: Option<&JsValue>) -> Vec<WorkspaceLabelDefinition> {
    value
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            Some(WorkspaceLabelDefinition {
                name: text(item.get("name")),
                color: WorkspaceLabelColor::parse(item.get("color")?.as_str()?)?,
            })
        })
        .collect()
}

fn states_from(value: Option<&JsValue>) -> Vec<WorkspaceLabelWorkspaceState> {
    value
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .map(|item| WorkspaceLabelWorkspaceState {
            workspace_id: text(item.get("workspaceId")),
            labels: item
                .get("labels")
                .and_then(JsValue::as_array)
                .map(|labels| labels.iter().map(|label| text(Some(label))).collect()),
            updated_at: text(item.get("updatedAt")),
        })
        .collect()
}

/// `fs.readFile(path, "utf8")`; a missing file is `None`.
fn read_utf8(path: &Path) -> Result<Option<String>, LabelError> {
    let fs_error = |syscall, path: Option<&Path>, source| {
        LabelError::Store(StoreError::Fs(FsError {
            syscall,
            path: path.map(|path| path.to_string_lossy().into_owned()),
            dest: None,
            source,
        }))
    };
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(fs_error("open", Some(path), source)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| fs_error("read", None, source))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn catalogs_equal(left: &[WorkspaceLabelDefinition], right: &[WorkspaceLabelDefinition]) -> bool {
    left == right
}

fn workspace_state(workspace: &PersistedWorkspaceRecord) -> WorkspaceLabelWorkspaceState {
    WorkspaceLabelWorkspaceState {
        workspace_id: workspace.workspace_id.clone(),
        labels: workspace.labels.clone(),
        updated_at: workspace.updated_at.clone(),
    }
}

fn transaction_for<T>(
    current_labels: &[WorkspaceLabelDefinition],
    mutation: &WorkspaceLabelMutation<T>,
    workspaces: RegistryView<'_, PersistedWorkspaceRecord>,
) -> WorkspaceLabelTransaction {
    WorkspaceLabelTransaction {
        phase: TransactionPhase::Prepared,
        before_labels: current_labels.to_vec(),
        after_labels: mutation.labels.clone(),
        before_workspaces: mutation
            .workspace_updates
            .iter()
            .filter_map(|workspace| workspaces.get(&workspace.workspace_id))
            .map(workspace_state)
            .collect(),
        after_workspaces: mutation
            .workspace_updates
            .iter()
            .map(workspace_state)
            .collect(),
    }
}

pub struct WorkspaceLabelCatalogStore<A: RegistryAccess> {
    file_path: PathBuf,
    transaction_path: PathBuf,
    registry: A,
    io: CatalogIo,
    publisher: Option<WorkspacePublisher>,
    loaded: bool,
    labels: Vec<WorkspaceLabelDefinition>,
    blocked: bool,
}

impl<A: RegistryAccess> WorkspaceLabelCatalogStore<A> {
    #[must_use]
    pub fn new(
        file_path: PathBuf,
        transaction_path: PathBuf,
        registry: A,
        io: CatalogIo,
        publisher: Option<WorkspacePublisher>,
    ) -> Self {
        Self {
            file_path,
            transaction_path,
            registry,
            io,
            publisher,
            loaded: false,
            labels: Vec::new(),
            blocked: false,
        }
    }

    /// Loads the catalog, first rolling back or cleaning up an interrupted
    /// commit's journal.
    ///
    /// # Errors
    ///
    /// Returns the failed read, parse or recovery; the next call tries again.
    pub fn initialize(&mut self) -> Result<(), LabelError> {
        if self.loaded {
            return Ok(());
        }
        self.load_and_recover()
    }

    /// # Errors
    ///
    /// Returns the error of [`Self::initialize`].
    pub fn list(&mut self) -> Result<Vec<WorkspaceLabelDefinition>, LabelError> {
        self.initialize()?;
        Ok(self.labels.clone())
    }

    /// Plans and commits one catalog and workspace mutation.
    ///
    /// # Errors
    ///
    /// Returns the planner's or the storage's error, or
    /// [`LabelError::StorageUncertain`] once storage is frozen.
    pub fn commit<T>(
        &mut self,
        planner: impl FnOnce(
            &[WorkspaceLabelDefinition],
            RegistryView<'_, PersistedWorkspaceRecord>,
        ) -> Result<WorkspaceLabelMutation<T>, LabelError>,
    ) -> Result<T, LabelError> {
        self.initialize()?;
        if self.blocked {
            return Err(LabelError::StorageUncertain);
        }

        let current = self.labels.clone();
        let transaction: RefCell<Option<WorkspaceLabelTransaction>> = RefCell::new(None);
        let next_labels: RefCell<Vec<WorkspaceLabelDefinition>> = RefCell::new(Vec::new());
        let committed = Cell::new(false);
        let io = RefCell::new(&mut self.io);
        let file_path = &self.file_path;
        let transaction_path = &self.transaction_path;
        let not_staged =
            || LabelError::Storage("Workspace label transaction was not staged".to_owned());
        let outcome = self.registry.with_registry(|registry| {
            registry.commit_staged(
                |workspaces| {
                    let mutation = planner(&current, workspaces)?;
                    let staged = transaction_for(&current, &mutation, workspaces);
                    let force_persist = !mutation.workspace_updates.is_empty()
                        || !catalogs_equal(&current, &mutation.labels);
                    *transaction.borrow_mut() = Some(staged);
                    *next_labels.borrow_mut() = mutation.labels;
                    Ok(StagedCommit {
                        updates: mutation.workspace_updates,
                        result: mutation.result,
                        force_persist,
                    })
                },
                || {
                    let staged = transaction.borrow();
                    let staged = staged.as_ref().ok_or_else(not_staged)?;
                    let mut io = io.borrow_mut();
                    (io.write_transaction)(transaction_path, staged)?;
                    (io.write_catalog)(file_path, &staged.after_labels)
                },
                || {
                    let mut staged = transaction.borrow_mut();
                    let staged = staged.as_mut().ok_or_else(not_staged)?;
                    staged.phase = TransactionPhase::Committed;
                    (io.borrow_mut().write_transaction)(transaction_path, staged)
                },
                || committed.set(true),
            )
        });

        match outcome {
            Ok((result, changed)) => {
                if committed.get() {
                    self.labels = next_labels.into_inner();
                }
                if let Some(publisher) = &mut self.publisher {
                    for workspace in &changed {
                        publisher(workspace);
                    }
                }
                let _ = (self.io.remove_transaction)(&self.transaction_path);
                Ok(result)
            }
            Err(error) => Err(self.resolve_failed_commit(error)),
        }
    }

    /// What a failed commit leaves behind decides what the caller sees:
    /// nothing durable means the original error, a rolled-back journal means
    /// the original error, and anything uncertain freezes storage.
    fn resolve_failed_commit(&mut self, error: LabelError) -> LabelError {
        let Ok(durable) = self.read_transaction() else {
            return self.block_until_restart();
        };
        let Some(durable) = durable else {
            return error;
        };
        if durable.phase == TransactionPhase::Committed {
            return self.block_until_restart();
        }
        if self.recover(&durable).is_err() {
            return self.block_until_restart();
        }
        error
    }

    fn block_until_restart(&mut self) -> LabelError {
        self.blocked = true;
        self.registry
            .with_registry(WorkspaceRegistry::block_mutations_until_restart);
        LabelError::StorageUncertain
    }

    fn load_and_recover(&mut self) -> Result<(), LabelError> {
        if let Some(transaction) = self.read_transaction()? {
            self.recover(&transaction)?;
            self.loaded = true;
            return Ok(());
        }
        self.labels = self.read_catalog()?;
        self.loaded = true;
        Ok(())
    }

    fn read_catalog(&self) -> Result<Vec<WorkspaceLabelDefinition>, LabelError> {
        let Some(raw) = read_utf8(&self.file_path)? else {
            return Ok(Vec::new());
        };
        Ok(definitions_from(Some(&parse_with(
            &catalog_schema(),
            &raw,
        )?)))
    }

    fn read_transaction(&self) -> Result<Option<WorkspaceLabelTransaction>, LabelError> {
        let Some(raw) = read_utf8(&self.transaction_path)? else {
            return Ok(None);
        };
        let value = parse_with(&transaction_schema(), &raw)?;
        let phase = if value.get("phase").and_then(JsValue::as_str) == Some("committed") {
            TransactionPhase::Committed
        } else {
            TransactionPhase::Prepared
        };
        Ok(Some(WorkspaceLabelTransaction {
            phase,
            before_labels: definitions_from(value.get("beforeLabels")),
            after_labels: definitions_from(value.get("afterLabels")),
            before_workspaces: states_from(value.get("beforeWorkspaces")),
            after_workspaces: states_from(value.get("afterWorkspaces")),
        }))
    }

    fn recover(&mut self, transaction: &WorkspaceLabelTransaction) -> Result<(), LabelError> {
        if transaction.phase == TransactionPhase::Committed {
            // The marker exists only after both data files are durable. It is cleanup state,
            // never an instruction to replay stale after-images over newer workspace mutations.
            self.labels = self.read_catalog()?;
            let _ = (self.io.remove_transaction)(&self.transaction_path);
            return Ok(());
        }
        let labels = &transaction.before_labels;
        let io = RefCell::new(&mut self.io);
        let file_path = &self.file_path;
        let transaction_path = &self.transaction_path;
        self.registry.with_registry(|registry| {
            registry.commit_staged(
                |workspaces| {
                    let updates = transaction
                        .before_workspaces
                        .iter()
                        .filter_map(|state| {
                            let current = workspaces.get(&state.workspace_id)?;
                            Some(PersistedWorkspaceRecord {
                                labels: state.labels.clone(),
                                updated_at: state.updated_at.clone(),
                                ..current.clone()
                            })
                        })
                        .collect();
                    Ok::<_, LabelError>(StagedCommit {
                        updates,
                        result: (),
                        force_persist: true,
                    })
                },
                || (io.borrow_mut().write_catalog)(file_path, labels),
                || (io.borrow_mut().remove_transaction)(transaction_path),
                || {},
            )
        })?;
        self.labels.clone_from(labels);
        Ok(())
    }
}
