//! Workspace labels: a host-wide catalog of coloured labels, their assignment
//! to workspaces, and a journal that lets clients catch up after a gap.
//!
//! Mirrors pinned Paseo `server/workspace-labels`. The catalog lives in
//! `$PASEO_HOME/projects/workspace-labels.json`, next to the workspace
//! registry whose `labels` fields it keeps in step; a crash journal in
//! `workspace-labels.transaction.json` makes each edit atomic across both.

pub mod catalog_store;
pub mod clock;
pub mod error;
pub mod names;
pub mod sequence;
pub mod service;

use std::path::PathBuf;

pub use catalog_store::{
    CatalogIo, RegistryAccess, TransactionPhase, WorkspaceLabelCatalogStore,
    WorkspaceLabelTransaction, WorkspacePublisher,
};
pub use error::{LabelError, WorkspaceLabelErrorCode};
pub use names::{WorkspaceLabelColor, WorkspaceLabelDefinition};
pub use sequence::WorkspaceLabelSequence;
pub use service::WorkspaceLabelService;

/// `createWorkspaceLabelService`'s input.
pub struct WorkspaceLabelServiceOptions<A: RegistryAccess> {
    pub paseo_home: PathBuf,
    pub workspace_registry: A,
    /// The `writeCatalog`, `writeTransaction` and `removeTransaction` options.
    pub io: CatalogIo,
    /// Receives each workspace a label mutation changed.
    pub publisher: Option<WorkspacePublisher>,
    /// `journalLimit`; the baseline default when `None`.
    pub journal_limit: Option<usize>,
}

/// `createWorkspaceLabelService`.
#[must_use]
pub fn create_workspace_label_service<A: RegistryAccess>(
    options: WorkspaceLabelServiceOptions<A>,
) -> WorkspaceLabelService<A> {
    let projects = options.paseo_home.join("projects");
    WorkspaceLabelService::new(
        WorkspaceLabelCatalogStore::new(
            projects.join("workspace-labels.json"),
            projects.join("workspace-labels.transaction.json"),
            options.workspace_registry,
            options.io,
            options.publisher,
        ),
        WorkspaceLabelSequence::new(
            options
                .journal_limit
                .unwrap_or(WorkspaceLabelSequence::DEFAULT_JOURNAL_LIMIT),
        ),
    )
}
