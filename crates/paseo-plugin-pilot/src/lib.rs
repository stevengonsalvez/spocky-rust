//! Contract pilot for managed plugin lifecycle and restart behavior.

#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTraffic {
    direction: &'static str,
    message: String,
}

impl RuntimeTraffic {
    #[must_use]
    pub const fn direction(&self) -> &'static str {
        self.direction
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug)]
pub struct AcquiredPlugin {
    directory: PathBuf,
    identity: PluginSourceIdentity,
    revision: String,
}

impl AcquiredPlugin {
    #[must_use]
    pub const fn identity(&self) -> &PluginSourceIdentity {
        &self.identity
    }

    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn load(self, timeout: Duration) -> Result<LoadedPlugin, PluginError> {
        let manifest: RuntimeManifest =
            serde_json::from_slice(&fs::read(self.directory.join("paseo-plugin.json"))?)?;
        let id = PluginId::new(manifest.id)?;
        let entry = safe_relative_path(&manifest.server)?;
        let mut child = Command::new("node")
            .arg(entry)
            .current_dir(&self.directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let result = exchange_runtime(&mut child, timeout);
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let (contributions, traffic) = result?;
        Ok(LoadedPlugin {
            id,
            candidate: Candidate::new(self.identity, Some(self.revision), contributions.clone()),
            contributions,
            traffic,
        })
    }
}

#[derive(Debug)]
pub struct LoadedPlugin {
    id: PluginId,
    candidate: Candidate,
    contributions: Vec<Contribution>,
    traffic: Vec<RuntimeTraffic>,
}

impl LoadedPlugin {
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    #[must_use]
    pub fn contributions(&self) -> &[Contribution] {
        &self.contributions
    }

    #[must_use]
    pub fn traffic(&self) -> &[RuntimeTraffic] {
        &self.traffic
    }

    #[must_use]
    pub fn into_candidate(self) -> Candidate {
        self.candidate
    }
}

#[derive(Deserialize)]
struct RuntimeManifest {
    id: String,
    server: String,
}

#[derive(Deserialize)]
struct ReadyMessage {
    r#type: String,
    contributions: Vec<WireContribution>,
}

#[derive(Deserialize)]
struct WireContribution {
    kind: String,
    id: String,
}

pub fn acquire_git(
    remote: &str,
    plugin_path: &str,
    reviewed_revision: &str,
    checkout: impl Into<PathBuf>,
    timeout: Duration,
) -> Result<AcquiredPlugin, PluginError> {
    if remote.is_empty()
        || !matches!(reviewed_revision.len(), 40..=64)
        || !reviewed_revision
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(PluginError::InvalidCandidate);
    }
    let checkout = checkout.into();
    let relative_plugin_path = if plugin_path == "." {
        PathBuf::new()
    } else {
        safe_relative_path(plugin_path)?
    };
    run_bounded(
        Command::new("git")
            .args(["clone", "--no-checkout", "--", remote])
            .arg(&checkout),
        timeout,
    )?;
    run_bounded(
        Command::new("git")
            .args(["checkout", "--detach", reviewed_revision])
            .current_dir(&checkout),
        timeout,
    )?;
    let output = run_bounded(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&checkout),
        timeout,
    )?;
    let actual_revision = String::from_utf8(output.stdout)
        .map_err(|_| PluginError::InvalidCommandOutput)?
        .trim()
        .to_owned();
    if actual_revision != reviewed_revision {
        return Err(PluginError::ReviewedRevisionMismatch {
            expected: reviewed_revision.to_owned(),
            actual: actual_revision,
        });
    }
    let directory = checkout.join(&relative_plugin_path);
    if !directory.is_dir() {
        return Err(PluginError::PluginNotFound);
    }
    Ok(AcquiredPlugin {
        directory,
        identity: PluginSourceIdentity::Git {
            remote: remote.to_owned(),
            plugin_path: plugin_path.to_owned(),
        },
        revision: reviewed_revision.to_owned(),
    })
}

pub fn acquire_npm_tarball(
    archive: &Path,
    package_name: &str,
    plugin_path: &str,
    installation: impl Into<PathBuf>,
    timeout: Duration,
) -> Result<AcquiredPlugin, PluginError> {
    let package_segments = npm_package_segments(package_name)?;
    let relative_plugin_path = if plugin_path == "." {
        PathBuf::new()
    } else {
        safe_relative_path(plugin_path)?
    };
    let archive = archive.canonicalize()?;
    let installation = installation.into();
    fs::create_dir_all(&installation)?;
    run_bounded(
        Command::new("npm")
            .args([
                "install",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--package-lock=false",
                "--prefix",
            ])
            .arg(&installation)
            .arg(&archive),
        timeout,
    )?;
    let mut package_root = installation.join("node_modules");
    for segment in package_segments {
        package_root.push(segment);
    }
    let package: NpmPackageManifest =
        serde_json::from_slice(&fs::read(package_root.join("package.json"))?)?;
    if package.name != package_name || package.version.is_empty() {
        return Err(PluginError::InvalidCandidate);
    }
    let directory = package_root.join(relative_plugin_path);
    if !directory.is_dir() {
        return Err(PluginError::PluginNotFound);
    }
    Ok(AcquiredPlugin {
        directory,
        identity: PluginSourceIdentity::Npm {
            package_name: package_name.to_owned(),
            plugin_path: plugin_path.to_owned(),
        },
        revision: package.version,
    })
}

#[derive(Deserialize)]
struct NpmPackageManifest {
    name: String,
    version: String,
}

fn npm_package_segments(package_name: &str) -> Result<Vec<&str>, PluginError> {
    let segments = package_name.split('/').collect::<Vec<_>>();
    let valid_count = if package_name.starts_with('@') { 2 } else { 1 };
    if segments.len() != valid_count
        || segments.iter().any(|segment| {
            segment.is_empty()
                || *segment == "."
                || *segment == ".."
                || segment.contains(['\\', ':'])
        })
    {
        return Err(PluginError::InvalidCandidate);
    }
    Ok(segments)
}

fn safe_relative_path(path: &str) -> Result<PathBuf, PluginError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(PluginError::InvalidCandidate);
    }
    Ok(path.to_owned())
}

fn run_bounded(command: &mut Command, timeout: Duration) -> Result<Output, PluginError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or(PluginError::InvalidCommandOutput)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(PluginError::InvalidCommandOutput)?;
    let stdout_reader = thread::spawn(move || read_to_end(stdout));
    let stderr_reader = thread::spawn(move || read_to_end(stderr));
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let output = Output {
                status,
                stdout: join_reader(stdout_reader)?,
                stderr: join_reader(stderr_reader)?,
            };
            if output.status.success() {
                return Ok(output);
            }
            return Err(PluginError::CommandFailed(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(PluginError::CommandTimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_to_end(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, PluginError> {
    reader
        .join()
        .map_err(|_| PluginError::InvalidCommandOutput)?
        .map_err(PluginError::Io)
}

fn exchange_runtime(
    child: &mut Child,
    timeout: Duration,
) -> Result<(Vec<Contribution>, Vec<RuntimeTraffic>), PluginError> {
    let stdout = child.stdout.take().ok_or(PluginError::RuntimeProtocol)?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let stdin = child.stdin.as_mut().ok_or(PluginError::RuntimeProtocol)?;
    let initialize = r#"{"type":"initialize"}"#;
    writeln!(stdin, "{initialize}")?;
    stdin.flush()?;
    let ready_line = receiver
        .recv_timeout(timeout)
        .map_err(|_| PluginError::RuntimeTimedOut)??;
    let ready: ReadyMessage = serde_json::from_str(&ready_line)?;
    if ready.r#type != "ready" {
        return Err(PluginError::RuntimeProtocol);
    }
    let contributions = ready
        .contributions
        .into_iter()
        .map(|item| match item.kind.as_str() {
            "rpc" => Ok(Contribution::Rpc(item.id)),
            "surface" => Ok(Contribution::Surface(item.id)),
            "settings_screen" => Ok(Contribution::SettingsScreen(item.id)),
            _ => Err(PluginError::RuntimeProtocol),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let shutdown = r#"{"type":"shutdown"}"#;
    writeln!(stdin, "{shutdown}")?;
    stdin.flush()?;
    let stopped_line = receiver
        .recv_timeout(timeout)
        .map_err(|_| PluginError::RuntimeTimedOut)??;
    let stopped: serde_json::Value = serde_json::from_str(&stopped_line)?;
    if stopped.get("type").and_then(serde_json::Value::as_str) != Some("stopped") {
        return Err(PluginError::RuntimeProtocol);
    }
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(PluginError::CommandFailed(format!(
                    "plugin runtime exited with {status}"
                )));
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err(PluginError::RuntimeTimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok((
        contributions,
        vec![
            RuntimeTraffic {
                direction: "host_to_plugin",
                message: initialize.to_owned(),
            },
            RuntimeTraffic {
                direction: "plugin_to_host",
                message: ready_line,
            },
            RuntimeTraffic {
                direction: "host_to_plugin",
                message: shutdown.to_owned(),
            },
            RuntimeTraffic {
                direction: "plugin_to_host",
                message: stopped_line,
            },
        ],
    ))
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
    InvalidCommandOutput,
    CommandFailed(String),
    CommandTimedOut,
    RuntimeProtocol,
    RuntimeTimedOut,
    ReviewedRevisionMismatch { expected: String, actual: String },
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

#[cfg(all(test, unix))]
mod tests {
    use super::run_bounded;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn bounded_command_drains_large_output_before_exit() {
        let output = run_bounded(
            Command::new("/bin/sh").args([
                "-c",
                "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
            ]),
            Duration::from_secs(5),
        )
        .expect("large output must not block child exit");

        assert_eq!(output.stdout.len(), 1_048_576);
        assert_eq!(output.stderr.len(), 1_048_576);
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PluginError {}
