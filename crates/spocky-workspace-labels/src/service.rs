//! `WorkspaceLabelService`: label edits, one operation at a time, each one a
//! single catalog commit followed by its publication.

use std::sync::{Mutex, PoisonError};

use spocky_store::registry::{PersistedWorkspaceRecord, RegistryView};

use crate::catalog_store::{RegistryAccess, WorkspaceLabelCatalogStore, WorkspaceLabelMutation};
use crate::clock::now_iso;
use crate::error::{LabelError, WorkspaceLabelErrorCode};
use crate::names::{
    WorkspaceLabelColor, WorkspaceLabelDefinition, normalize_workspace_label_name,
    workspace_label_key,
};
use crate::sequence::{
    Subscriber, WorkspaceLabelChange, WorkspaceLabelCursor, WorkspaceLabelSequence,
    WorkspaceLabelSync,
};

/// What [`WorkspaceLabelService::subscribe`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelSubscription {
    pub snapshot: WorkspaceLabelSync,
    /// Hand to [`WorkspaceLabelService::unsubscribe`].
    pub id: u64,
}

/// `setAssignment`'s result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelAssignment {
    pub label: WorkspaceLabelDefinition,
    pub workspace_labels: Vec<String>,
}

/// `update`'s result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelUpdate {
    pub label: WorkspaceLabelDefinition,
    pub affected_workspace_count: usize,
}

struct AssignmentCommit {
    definition: WorkspaceLabelDefinition,
    workspace_labels: Vec<String>,
    catalog_changed: bool,
}

struct UpdateCommit {
    label: WorkspaceLabelDefinition,
    affected_workspace_count: usize,
    changed: bool,
    previous_name: Option<String>,
}

struct DeleteCommit {
    affected_workspace_count: usize,
    deleted_name: Option<String>,
}

struct State<A: RegistryAccess> {
    catalog: WorkspaceLabelCatalogStore<A>,
    sequence: WorkspaceLabelSequence,
}

/// Operations run one at a time (`exclusive`): each holds the service for its
/// whole commit and publication.
pub struct WorkspaceLabelService<A: RegistryAccess> {
    state: Mutex<State<A>>,
}

impl<A: RegistryAccess> WorkspaceLabelService<A> {
    #[must_use]
    pub fn new(catalog: WorkspaceLabelCatalogStore<A>, sequence: WorkspaceLabelSequence) -> Self {
        Self {
            state: Mutex::new(State { catalog, sequence }),
        }
    }

    fn exclusive<T>(&self, operation: impl FnOnce(&mut State<A>) -> T) -> T {
        operation(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// # Errors
    ///
    /// Returns the catalog's load error.
    pub fn initialize(&self) -> Result<(), LabelError> {
        self.exclusive(|state| state.catalog.initialize())
    }

    /// # Errors
    ///
    /// Returns the catalog's load error.
    pub fn list(
        &self,
        cursor: Option<&WorkspaceLabelCursor>,
    ) -> Result<WorkspaceLabelSync, LabelError> {
        self.exclusive(|state| {
            let labels = state.catalog.list()?;
            Ok(state.sequence.synchronize(&labels, cursor))
        })
    }

    /// Subscribes `on_change`, then answers with the snapshot or catch-up the
    /// cursor allows.
    ///
    /// # Errors
    ///
    /// Returns the catalog's load error, after dropping the subscription.
    pub fn subscribe(
        &self,
        cursor: Option<&WorkspaceLabelCursor>,
        on_change: Subscriber,
    ) -> Result<WorkspaceLabelSubscription, LabelError> {
        self.exclusive(|state| {
            let id = state.sequence.subscribe(on_change);
            match state.catalog.list() {
                Ok(labels) => Ok(WorkspaceLabelSubscription {
                    snapshot: state.sequence.synchronize(&labels, cursor),
                    id,
                }),
                Err(error) => {
                    state.sequence.unsubscribe(id);
                    Err(error)
                }
            }
        })
    }

    pub fn unsubscribe(&self, id: u64) {
        self.exclusive(|state| state.sequence.unsubscribe(id));
    }

    /// Assigns or unassigns a label on one workspace, creating the catalog
    /// entry on first assignment. An existing definition wins over the one in
    /// the request.
    ///
    /// # Errors
    ///
    /// Returns an empty name, an unknown or archived workspace, or a storage failure.
    pub fn set_assignment(
        &self,
        workspace_id: &str,
        label: &WorkspaceLabelDefinition,
        assigned: bool,
    ) -> Result<WorkspaceLabelAssignment, LabelError> {
        self.exclusive(|state| {
            let name = require_name(&label.name)?;
            let committed = state.catalog.commit(|catalog, workspaces| {
                let workspace = workspaces
                    .get(workspace_id)
                    .filter(|workspace| workspace.archived_at.as_deref().is_none_or(str::is_empty))
                    .ok_or_else(|| {
                        LabelError::label(
                            WorkspaceLabelErrorCode::WorkspaceNotFound,
                            "Workspace not found",
                        )
                    })?;
                let key = workspace_label_key(&name);
                let existing = catalog
                    .iter()
                    .find(|candidate| workspace_label_key(&candidate.name) == key);
                let definition = existing
                    .cloned()
                    .unwrap_or_else(|| WorkspaceLabelDefinition {
                        name: name.clone(),
                        color: label.color,
                    });
                let current = workspace.labels.clone().unwrap_or_default();
                let changed_labels =
                    update_assignment_labels(&current, &key, &definition.name, assigned);
                let workspace_labels = changed_labels.clone().unwrap_or(current);
                let catalog_changed = existing.is_none() && assigned;
                let mut labels = catalog.to_vec();
                if catalog_changed {
                    labels.push(definition.clone());
                }
                let workspace_updates = if changed_labels.is_some() {
                    vec![PersistedWorkspaceRecord {
                        labels: (!workspace_labels.is_empty()).then(|| workspace_labels.clone()),
                        updated_at: now_iso(),
                        ..workspace.clone()
                    }]
                } else {
                    Vec::new()
                };
                Ok(WorkspaceLabelMutation {
                    labels,
                    workspace_updates,
                    result: AssignmentCommit {
                        definition,
                        workspace_labels,
                        catalog_changed,
                    },
                })
            })?;
            if committed.catalog_changed {
                state.sequence.publish(WorkspaceLabelChange::Upsert {
                    label: committed.definition.clone(),
                    previous_name: None,
                });
            }
            Ok(WorkspaceLabelAssignment {
                label: committed.definition,
                workspace_labels: committed.workspace_labels,
            })
        })
    }

    /// Edits a label: a new name, a new colour, or both, in one catalog
    /// commit. An omitted field is left alone.
    ///
    /// Renaming onto a name the host already has is rejected, not merged. The
    /// check happens inside the planner, so a rejected collision applies
    /// neither field.
    ///
    /// # Errors
    ///
    /// Returns an empty name, an unknown label, a name collision, or a storage failure.
    pub fn update(
        &self,
        name: &str,
        new_name: Option<&str>,
        color: Option<WorkspaceLabelColor>,
    ) -> Result<WorkspaceLabelUpdate, LabelError> {
        self.exclusive(|state| {
            let from_key = workspace_label_key(&require_name(name)?);
            let new_name = new_name.map(require_name).transpose()?;
            let committed = state.catalog.commit(|catalog, workspaces| {
                let existing_index = catalog
                    .iter()
                    .position(|label| workspace_label_key(&label.name) == from_key)
                    .ok_or_else(|| {
                        LabelError::label(WorkspaceLabelErrorCode::NotFound, "Label not found")
                    })?;
                let existing = &catalog[existing_index];
                // A label keeping its own key is not colliding with itself, so case-only edits pass.
                if let Some(new_name) = &new_name {
                    let to_key = workspace_label_key(new_name);
                    if to_key != from_key
                        && catalog
                            .iter()
                            .any(|label| workspace_label_key(&label.name) == to_key)
                    {
                        return Err(LabelError::label(
                            WorkspaceLabelErrorCode::NameTaken,
                            "A label with that name already exists",
                        ));
                    }
                }
                let name_changed = new_name.as_ref().is_some_and(|new| *new != existing.name);
                let color_changed = color.is_some_and(|color| color != existing.color);
                if !name_changed && !color_changed {
                    return Ok(WorkspaceLabelMutation {
                        labels: catalog.to_vec(),
                        workspace_updates: Vec::new(),
                        result: UpdateCommit {
                            label: existing.clone(),
                            affected_workspace_count: 0,
                            changed: false,
                            previous_name: None,
                        },
                    });
                }
                let label = WorkspaceLabelDefinition {
                    name: match &new_name {
                        Some(new) if name_changed => new.clone(),
                        _ => existing.name.clone(),
                    },
                    color: color.unwrap_or(existing.color),
                };
                // Workspaces store names, so only a rename rewrites assignments; a recolour is
                // catalog-only.
                let workspace_updates = if name_changed {
                    rewrite_assignments(workspaces, &from_key, Some(&label.name))
                } else {
                    Vec::new()
                };
                let mut labels = catalog.to_vec();
                labels[existing_index] = label.clone();
                Ok(WorkspaceLabelMutation {
                    labels,
                    result: UpdateCommit {
                        label,
                        affected_workspace_count: workspace_updates.len(),
                        changed: true,
                        previous_name: name_changed.then(|| existing.name.clone()),
                    },
                    workspace_updates,
                })
            })?;
            if committed.changed {
                state.sequence.publish(WorkspaceLabelChange::Upsert {
                    label: committed.label.clone(),
                    previous_name: committed.previous_name,
                });
            }
            Ok(WorkspaceLabelUpdate {
                label: committed.label,
                affected_workspace_count: committed.affected_workspace_count,
            })
        })
    }

    /// Removes a label from the catalog and from every workspace, archived
    /// ones included. An unknown label deletes nothing.
    ///
    /// # Errors
    ///
    /// Returns an empty name or a storage failure.
    pub fn delete(&self, name: &str) -> Result<usize, LabelError> {
        self.exclusive(|state| {
            let key = workspace_label_key(&require_name(name)?);
            let committed = state.catalog.commit(|catalog, workspaces| {
                let Some(existing_index) = catalog
                    .iter()
                    .position(|label| workspace_label_key(&label.name) == key)
                else {
                    return Ok(WorkspaceLabelMutation {
                        labels: catalog.to_vec(),
                        workspace_updates: Vec::new(),
                        result: DeleteCommit {
                            affected_workspace_count: 0,
                            deleted_name: None,
                        },
                    });
                };
                let workspace_updates = rewrite_assignments(workspaces, &key, None);
                let mut labels = catalog.to_vec();
                let removed = labels.remove(existing_index);
                Ok(WorkspaceLabelMutation {
                    labels,
                    result: DeleteCommit {
                        affected_workspace_count: workspace_updates.len(),
                        deleted_name: Some(removed.name),
                    },
                    workspace_updates,
                })
            })?;
            if let Some(name) = committed.deleted_name {
                state
                    .sequence
                    .publish(WorkspaceLabelChange::Remove { name });
            }
            Ok(committed.affected_workspace_count)
        })
    }

    /// How many workspaces, archived ones included, a delete would rewrite.
    ///
    /// # Errors
    ///
    /// Returns an empty name or a storage failure.
    pub fn count_affected_workspaces(&self, name: &str) -> Result<usize, LabelError> {
        self.exclusive(|state| {
            let key = workspace_label_key(&require_name(name)?);
            state.catalog.commit(|catalog, workspaces| {
                Ok(WorkspaceLabelMutation {
                    labels: catalog.to_vec(),
                    workspace_updates: Vec::new(),
                    result: workspaces
                        .values()
                        .filter(|workspace| workspace_has_label(workspace, &key))
                        .count(),
                })
            })
        })
    }
}

/// The labels after the change, or `None` when it changes nothing.
fn update_assignment_labels(
    current: &[String],
    key: &str,
    display_name: &str,
    assigned: bool,
) -> Option<Vec<String>> {
    let assigned_index = current
        .iter()
        .position(|label| workspace_label_key(label) == key);
    match (assigned, assigned_index) {
        (true, None) => {
            let mut next = current.to_vec();
            next.push(display_name.to_owned());
            Some(next)
        }
        (false, Some(index)) => {
            let mut next = current.to_vec();
            next.remove(index);
            Some(next)
        }
        _ => None,
    }
}

fn workspace_has_label(workspace: &PersistedWorkspaceRecord, key: &str) -> bool {
    workspace
        .labels
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|label| workspace_label_key(label) == key)
}

fn rewrite_assignments(
    workspaces: RegistryView<'_, PersistedWorkspaceRecord>,
    from_key: &str,
    to: Option<&str>,
) -> Vec<PersistedWorkspaceRecord> {
    let now = now_iso();
    workspaces
        .values()
        .filter(|workspace| workspace_has_label(workspace, from_key))
        .map(|workspace| {
            let next: Vec<String> = workspace
                .labels
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|label| {
                    if workspace_label_key(label) == from_key {
                        to.map(str::to_owned)
                    } else {
                        Some(label.clone())
                    }
                })
                .collect();
            PersistedWorkspaceRecord {
                labels: (!next.is_empty()).then_some(next),
                updated_at: now.clone(),
                ..workspace.clone()
            }
        })
        .collect()
}

fn require_name(raw: &str) -> Result<String, LabelError> {
    let name = normalize_workspace_label_name(raw);
    if name.is_empty() {
        return Err(LabelError::label(
            WorkspaceLabelErrorCode::NameEmpty,
            "Label name cannot be empty",
        ));
    }
    Ok(name)
}
