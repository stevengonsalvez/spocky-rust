//! Contract pilot for managed plugin lifecycle and restart behavior.

#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, PluginError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.chars().enumerate().all(|(index, character)| {
                character.is_ascii_lowercase()
                    || character == '-'
                    || (index > 0 && character.is_ascii_digit())
            });
        if valid && value.as_bytes()[0].is_ascii_lowercase() {
            Ok(Self(value))
        } else {
            Err(PluginError::InvalidPluginId)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PluginSourceIdentity {
    Directory {
        path: String,
    },
    Git {
        remote: String,
        plugin_path: String,
    },
    Npm {
        package_name: String,
        plugin_path: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Contribution {
    Rpc(String),
    Surface(String),
    SettingsScreen(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    identity: PluginSourceIdentity,
    revision: Option<String>,
    contributions: Vec<Contribution>,
    activation_fails: bool,
}

impl Candidate {
    pub fn new(
        identity: PluginSourceIdentity,
        revision: Option<String>,
        contributions: impl IntoIterator<Item = Contribution>,
    ) -> Self {
        Self {
            identity,
            revision,
            contributions: contributions.into_iter().collect(),
            activation_fails: false,
        }
    }

    #[must_use]
    pub fn with_activation_failure(mut self) -> Self {
        self.activation_fails = true;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Installation {
    identity: PluginSourceIdentity,
    revision: Option<String>,
    contributions: Vec<Contribution>,
}

impl Installation {
    #[must_use]
    pub const fn identity(&self) -> &PluginSourceIdentity {
        &self.identity
    }

    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewedUpdate {
    pub id: PluginId,
    pub expected_identity: PluginSourceIdentity,
    pub expected_revision: String,
    pub target_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportedContribution {
    pub plugin_id: PluginId,
    pub contribution: Contribution,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PluginState {
    installations: BTreeMap<PluginId, Installation>,
    settings: BTreeMap<PluginId, BTreeMap<String, String>>,
}

pub struct PluginHost {
    path: PathBuf,
    state: PluginState,
}

impl PluginHost {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, PluginError> {
        let path = path.into();
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PluginState::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, state })
    }

    pub fn install(&mut self, id: PluginId, candidate: Candidate) -> Result<(), PluginError> {
        validate_candidate(&candidate)?;
        if candidate.activation_fails {
            return Err(PluginError::ActivationFailed);
        }
        let installation = Installation {
            identity: candidate.identity,
            revision: candidate.revision,
            contributions: candidate.contributions,
        };
        let previous = self.state.installations.insert(id.clone(), installation);
        if let Err(error) = self.persist() {
            match previous {
                Some(previous) => {
                    self.state.installations.insert(id, previous);
                }
                None => {
                    self.state.installations.remove(&id);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn review_update(
        &self,
        id: &PluginId,
        target_revision: String,
    ) -> Result<ReviewedUpdate, PluginError> {
        let current = self
            .state
            .installations
            .get(id)
            .ok_or(PluginError::PluginNotFound)?;
        let expected_revision = current
            .revision
            .clone()
            .ok_or(PluginError::LocalDirectoryCannotUpdate)?;
        Ok(ReviewedUpdate {
            id: id.clone(),
            expected_identity: current.identity.clone(),
            expected_revision,
            target_revision,
        })
    }

    pub fn apply_reviewed(
        &mut self,
        review: ReviewedUpdate,
        candidate: Candidate,
    ) -> Result<(), PluginError> {
        let current = self
            .state
            .installations
            .get(&review.id)
            .ok_or(PluginError::PluginNotFound)?;
        if current.identity != review.expected_identity
            || current.revision.as_deref() != Some(review.expected_revision.as_str())
        {
            return Err(PluginError::ReviewedStateChanged);
        }
        if candidate.identity != review.expected_identity
            || candidate.revision.as_deref() != Some(review.target_revision.as_str())
        {
            return Err(PluginError::CandidateDoesNotMatchReview);
        }
        validate_candidate(&candidate)?;
        if candidate.activation_fails {
            return Err(PluginError::ActivationFailed);
        }
        self.install(review.id, candidate)
    }

    pub fn write_settings(
        &mut self,
        id: &PluginId,
        values: BTreeMap<String, String>,
    ) -> Result<(), PluginError> {
        if !self.state.installations.contains_key(id) {
            return Err(PluginError::PluginNotFound);
        }
        self.state.settings.insert(id.clone(), values);
        self.persist()
    }

    #[must_use]
    pub fn settings(&self, id: &PluginId) -> Option<&BTreeMap<String, String>> {
        self.state.settings.get(id)
    }

    pub fn remove(&mut self, id: &PluginId) -> Result<(), PluginError> {
        if self.state.installations.remove(id).is_none() {
            return Err(PluginError::PluginNotFound);
        }
        self.state.settings.remove(id);
        self.persist()
    }

    #[must_use]
    pub fn installation(&self, id: &PluginId) -> Option<&Installation> {
        self.state.installations.get(id)
    }

    #[must_use]
    pub fn installations(&self) -> &BTreeMap<PluginId, Installation> {
        &self.state.installations
    }

    #[must_use]
    pub fn contributions(&self) -> Vec<TransportedContribution> {
        self.state
            .installations
            .iter()
            .flat_map(|(plugin_id, installation)| {
                installation
                    .contributions
                    .iter()
                    .cloned()
                    .map(|contribution| TransportedContribution {
                        plugin_id: plugin_id.clone(),
                        contribution,
                    })
            })
            .collect()
    }

    #[must_use]
    pub fn contributions_for(&self, id: &PluginId) -> Vec<Contribution> {
        self.state
            .installations
            .get(id)
            .map_or_else(Vec::new, |installation| installation.contributions.clone())
    }

    fn persist(&self) -> Result<(), PluginError> {
        let bytes = serde_json::to_vec_pretty(&self.state)?;
        atomic_write(&self.path, &bytes)?;
        Ok(())
    }
}

fn validate_candidate(candidate: &Candidate) -> Result<(), PluginError> {
    match (&candidate.identity, &candidate.revision) {
        (PluginSourceIdentity::Directory { path }, None) if !path.is_empty() => Ok(()),
        (
            PluginSourceIdentity::Git {
                remote,
                plugin_path,
            },
            Some(revision),
        ) if !remote.is_empty()
            && !plugin_path.is_empty()
            && matches!(revision.len(), 40..=64)
            && revision
                .chars()
                .all(|character| character.is_ascii_hexdigit()) =>
        {
            Ok(())
        }
        (
            PluginSourceIdentity::Npm {
                package_name,
                plugin_path,
            },
            Some(version),
        ) if !package_name.is_empty() && !plugin_path.is_empty() && !version.is_empty() => Ok(()),
        _ => Err(PluginError::InvalidCandidate),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[derive(Debug)]
pub enum PluginError {
    InvalidPluginId,
    InvalidCandidate,
    PluginNotFound,
    LocalDirectoryCannotUpdate,
    ReviewedStateChanged,
    CandidateDoesNotMatchReview,
    ActivationFailed,
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl PartialEq for PluginError {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Eq for PluginError {}

impl From<std::io::Error> for PluginError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for PluginError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PluginError {}
