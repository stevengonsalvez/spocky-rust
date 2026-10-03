//! `WorkspaceLabelSequence`: the generation-tagged change journal that lets a
//! reconnecting client catch up instead of re-reading the catalog.

use crate::names::WorkspaceLabelDefinition;

/// `WorkspaceLabelChange`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceLabelChange {
    Upsert {
        label: WorkspaceLabelDefinition,
        previous_name: Option<String>,
    },
    Remove {
        name: String,
    },
}

/// `SequencedChange`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequencedChange {
    pub generation: String,
    pub seq: u64,
    pub change: WorkspaceLabelChange,
}

/// `WorkspaceLabelCursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelCursor {
    pub generation: String,
    pub after_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Snapshot,
    Changes,
}

impl SyncMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Changes => "changes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelRemoval {
    pub name: String,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelSyncMetadata {
    pub mode: SyncMode,
    pub generation: String,
    pub head_seq: u64,
    pub removals: Vec<WorkspaceLabelRemoval>,
}

/// `WorkspaceLabelSync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelSync {
    pub labels: Vec<WorkspaceLabelDefinition>,
    pub sync: WorkspaceLabelSyncMetadata,
}

/// A subscriber's handler. Its error is ignored: publication follows durable
/// persistence and cannot fail the mutation.
pub type Subscriber = Box<dyn FnMut(&SequencedChange) -> Result<(), SubscriberError> + Send>;

/// What a subscriber's handler raises, the `throw` of the baseline's handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriberError;

/// `Map` with insertion order: setting an existing key keeps its place, and
/// deleting then setting moves the key to the end.
struct OrderedMap<V>(Vec<(String, V)>);

impl<V> OrderedMap<V> {
    const fn new() -> Self {
        Self(Vec::new())
    }

    fn delete(&mut self, key: &str) -> bool {
        match self.0.iter().position(|(existing, _)| existing == key) {
            Some(index) => {
                self.0.remove(index);
                true
            }
            None => false,
        }
    }

    fn set(&mut self, key: String, value: V) {
        match self.0.iter_mut().find(|(existing, _)| *existing == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    fn into_values(self) -> Vec<V> {
        self.0.into_iter().map(|(_, value)| value).collect()
    }
}

pub struct WorkspaceLabelSequence {
    generation: String,
    head_seq: u64,
    journal: Vec<SequencedChange>,
    journal_limit: usize,
    subscribers: Vec<(u64, Subscriber)>,
    next_subscriber: u64,
}

impl WorkspaceLabelSequence {
    /// The baseline's default `journalLimit`.
    pub const DEFAULT_JOURNAL_LIMIT: usize = 256;

    #[must_use]
    pub fn new(journal_limit: usize) -> Self {
        Self {
            generation: uuid::Uuid::new_v4().to_string(),
            head_seq: 0,
            journal: Vec::new(),
            journal_limit,
            subscribers: Vec::new(),
            next_subscriber: 0,
        }
    }

    /// Returns the id to hand to [`Self::unsubscribe`].
    pub fn subscribe(&mut self, listener: Subscriber) -> u64 {
        let id = self.next_subscriber;
        self.next_subscriber += 1;
        self.subscribers.push((id, listener));
        id
    }

    pub fn unsubscribe(&mut self, id: u64) {
        self.subscribers.retain(|(existing, _)| *existing != id);
    }

    pub fn publish(&mut self, change: WorkspaceLabelChange) {
        self.head_seq += 1;
        let entry = SequencedChange {
            generation: self.generation.clone(),
            seq: self.head_seq,
            change,
        };
        self.journal.push(entry.clone());
        if self.journal.len() > self.journal_limit {
            self.journal.remove(0);
        }
        for (_, subscriber) in &mut self.subscribers {
            // Publication follows durable persistence and cannot fail the mutation.
            let _ = subscriber(&entry);
        }
    }

    #[must_use]
    pub fn synchronize(
        &self,
        catalog: &[WorkspaceLabelDefinition],
        cursor: Option<&WorkspaceLabelCursor>,
    ) -> WorkspaceLabelSync {
        let oldest_seq = self
            .journal
            .first()
            .map_or(self.head_seq + 1, |entry| entry.seq);
        let caught_up = cursor.filter(|cursor| {
            cursor.generation == self.generation
                && cursor.after_seq <= self.head_seq
                && cursor.after_seq + 1 >= oldest_seq
        });
        let Some(cursor) = caught_up else {
            return WorkspaceLabelSync {
                labels: catalog.to_vec(),
                sync: WorkspaceLabelSyncMetadata {
                    mode: SyncMode::Snapshot,
                    generation: self.generation.clone(),
                    head_seq: self.head_seq,
                    removals: Vec::new(),
                },
            };
        };

        let mut upserts = OrderedMap::<WorkspaceLabelDefinition>::new();
        let mut removals = OrderedMap::<WorkspaceLabelRemoval>::new();
        for entry in self
            .journal
            .iter()
            .filter(|entry| entry.seq > cursor.after_seq)
        {
            match &entry.change {
                WorkspaceLabelChange::Remove { name } => {
                    let key = name.to_lowercase();
                    let was_created_during_catch_up = upserts.delete(&key);
                    if !was_created_during_catch_up {
                        removals.set(
                            key,
                            WorkspaceLabelRemoval {
                                name: name.clone(),
                                seq: entry.seq,
                            },
                        );
                    }
                }
                WorkspaceLabelChange::Upsert {
                    label,
                    previous_name,
                } => {
                    // `if (entry.change.previousName)`: an empty name is falsy.
                    if let Some(previous) = previous_name.as_deref().filter(|name| !name.is_empty())
                    {
                        let previous_key = previous.to_lowercase();
                        let was_created_during_catch_up = upserts.delete(&previous_key);
                        if !was_created_during_catch_up {
                            removals.set(
                                previous_key,
                                WorkspaceLabelRemoval {
                                    name: previous.to_owned(),
                                    seq: entry.seq,
                                },
                            );
                        }
                    }
                    let key = label.name.to_lowercase();
                    removals.delete(&key);
                    upserts.set(key, label.clone());
                }
            }
        }
        WorkspaceLabelSync {
            labels: upserts.into_values(),
            sync: WorkspaceLabelSyncMetadata {
                mode: SyncMode::Changes,
                generation: self.generation.clone(),
                head_seq: self.head_seq,
                removals: removals.into_values(),
            },
        }
    }
}
