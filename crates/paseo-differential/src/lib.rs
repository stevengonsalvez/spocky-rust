use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as _;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl Artifact {
    #[must_use]
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCounts {
    pub fixtures: u64,
    pub assertions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub initial_files: Vec<Artifact>,
    pub expected_counts: ExecutionCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    #[serde(default = "default_process_timeout_ms")]
    pub timeout_ms: u64,
}

const fn default_process_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturePlan {
    pub structured_output: Option<PathBuf>,
    pub artifacts: Vec<PathBuf>,
    pub state: Vec<PathBuf>,
    pub screenshots: Vec<PathBuf>,
    pub accessibility: Option<PathBuf>,
    pub performance: Option<PathBuf>,
    pub recovery: Option<PathBuf>,
    pub counts: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPlan {
    pub scenario: Scenario,
    pub original: ProcessSpec,
    pub rust: ProcessSpec,
    pub state_environment_variable: String,
    pub captures: CapturePlan,
    pub normalization_rules: Vec<NormalizationRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum ObservationSlot<T> {
    Missing,
    Value(T),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub structured_output: ObservationSlot<Value>,
    pub stdout: ObservationSlot<Vec<u8>>,
    pub stderr: ObservationSlot<Vec<u8>>,
    pub exit_code: ObservationSlot<i32>,
    pub artifacts: ObservationSlot<Vec<Artifact>>,
    pub state: ObservationSlot<Vec<Artifact>>,
    pub screenshots: ObservationSlot<Vec<Artifact>>,
    pub accessibility: ObservationSlot<Value>,
    pub performance: ObservationSlot<Value>,
    pub recovery: ObservationSlot<Value>,
    pub counts: ObservationSlot<ExecutionCounts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationCategory {
    GeneratedId,
    WallClock,
    TemporaryPath,
}

impl NormalizationCategory {
    fn token_name(self) -> &'static str {
        match self {
            Self::GeneratedId => "generated_id",
            Self::WallClock => "wall_clock",
            Self::TemporaryPath => "temporary_path",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "slot", content = "selector", rename_all = "snake_case")]
pub enum NormalizationTarget {
    StructuredJsonPointer(String),
    Stdout,
    Stderr,
    Artifact(String),
    State(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizationRule {
    pub id: String,
    pub category: NormalizationCategory,
    pub reason: String,
    pub owner: String,
    pub target: NormalizationTarget,
    pub exact_values: Vec<String>,
}

impl NormalizationRule {
    #[must_use]
    pub fn exact_json(
        id: impl Into<String>,
        category: NormalizationCategory,
        reason: impl Into<String>,
        owner: impl Into<String>,
        pointer: impl Into<String>,
        exact_values: Vec<impl Into<String>>,
    ) -> Self {
        Self::new(
            id,
            category,
            reason,
            owner,
            NormalizationTarget::StructuredJsonPointer(pointer.into()),
            exact_values,
        )
    }

    #[must_use]
    pub fn exact_text(
        id: impl Into<String>,
        category: NormalizationCategory,
        reason: impl Into<String>,
        owner: impl Into<String>,
        target: NormalizationTarget,
        exact_values: Vec<impl Into<String>>,
    ) -> Self {
        Self::new(id, category, reason, owner, target, exact_values)
    }

    fn new(
        id: impl Into<String>,
        category: NormalizationCategory,
        reason: impl Into<String>,
        owner: impl Into<String>,
        target: NormalizationTarget,
        exact_values: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            reason: reason.into(),
            owner: owner.into(),
            target,
            exact_values: exact_values.into_iter().map(Into::into).collect(),
        }
    }

    fn token(&self) -> String {
        format!("{{{{{}:{}}}}}", self.category.token_name(), self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub path: String,
}

impl Comparison {
    #[must_use]
    pub fn different(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub raw: Observation,
    pub normalized: Observation,
    pub raw_digests: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DifferentialManifest {
    pub format_version: u32,
    pub scenario: Scenario,
    pub rules: Vec<NormalizationRule>,
    pub original: Evidence,
    pub rust: Evidence,
    pub equivalent: bool,
    pub differences: Vec<Comparison>,
    pub executed_fixtures: u64,
    pub executed_assertions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DifferentialFailureManifest {
    pub status: String,
    pub format_version: u32,
    pub scenario: Scenario,
    pub rules: Vec<NormalizationRule>,
    pub error: String,
    pub original_raw: Observation,
    pub rust_raw: Observation,
    pub original_raw_digests: BTreeMap<String, String>,
    pub rust_raw_digests: BTreeMap<String, String>,
}

impl DifferentialFailureManifest {
    /// Serializes a failed comparison while retaining both raw observations.
    ///
    /// # Errors
    ///
    /// Returns an error if failure serialization fails.
    pub fn to_bytes(report: &Self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(report)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DifferentialRun {
    Compared(Box<DifferentialManifest>),
    ComparisonFailed(Box<DifferentialFailureManifest>),
}

impl DifferentialManifest {
    /// Serializes the report with stable field and map ordering.
    ///
    /// # Errors
    ///
    /// Returns an error if report serialization fails.
    pub fn to_bytes(report: &Self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(report)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    InvalidRule { id: String, message: String },
    NormalizationMiss { id: String, target: String },
    NonTextArtifact { id: String, target: String },
    Io { operation: String, message: String },
    UnsafePath(PathBuf),
}

impl Display for HarnessError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRule { id, message } => {
                write!(formatter, "normalization rule {id} is invalid: {message}")
            }
            Self::NormalizationMiss { id, target } => {
                write!(formatter, "normalization rule {id} did not match {target}")
            }
            Self::NonTextArtifact { id, target } => {
                write!(
                    formatter,
                    "normalization rule {id} targeted non-text {target}"
                )
            }
            Self::Io { operation, message } => write!(formatter, "{operation}: {message}"),
            Self::UnsafePath(path) => {
                write!(
                    formatter,
                    "path must stay inside disposable state: {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for HarnessError {}

/// Compares raw observations after applying only declared normalization rules.
///
/// # Errors
///
/// Returns an error for incomplete rules, missing targets, or text rules aimed
/// at binary data. Raw observations are never mutated.
pub fn compare_observations(
    scenario: &Scenario,
    original_raw: Observation,
    rust_raw: Observation,
    rules: &[NormalizationRule],
) -> Result<DifferentialManifest, HarnessError> {
    compare_with_roots(scenario, original_raw, rust_raw, rules, None, None)
}

/// Executes identical scenario inputs against original and Rust processes.
///
/// Each process receives a separate disposable state directory through the
/// configured environment variable. Initial files are copied into both roots.
///
/// # Errors
///
/// Returns an error when disposable state cannot be prepared, a configured
/// path escapes that state, or normalization rules are invalid.
pub fn run_differential(plan: &RunPlan) -> Result<DifferentialManifest, HarnessError> {
    with_executed_observations(plan, |original, rust, original_root, rust_root| {
        compare_with_roots(
            &plan.scenario,
            original,
            rust,
            &plan.normalization_rules,
            Some(original_root),
            Some(rust_root),
        )
    })
}

/// Executes a scenario and retains raw observations when comparison setup fails.
///
/// # Errors
///
/// Returns an error only when the disposable scenario cannot be prepared or run.
pub fn run_differential_preserving_evidence(
    plan: &RunPlan,
) -> Result<DifferentialRun, HarnessError> {
    with_executed_observations(plan, |original, rust, original_root, rust_root| {
        let original_raw = original.clone();
        let rust_raw = rust.clone();
        match compare_with_roots(
            &plan.scenario,
            original,
            rust,
            &plan.normalization_rules,
            Some(original_root),
            Some(rust_root),
        ) {
            Ok(report) => Ok(DifferentialRun::Compared(Box::new(report))),
            Err(error) => Ok(DifferentialRun::ComparisonFailed(Box::new(
                DifferentialFailureManifest {
                    status: "error".into(),
                    format_version: 1,
                    scenario: plan.scenario.clone(),
                    rules: plan.normalization_rules.clone(),
                    error: error.to_string(),
                    original_raw_digests: digest_observation(&original_raw),
                    rust_raw_digests: digest_observation(&rust_raw),
                    original_raw,
                    rust_raw,
                },
            ))),
        }
    })
}

fn with_executed_observations<T>(
    plan: &RunPlan,
    finish: impl FnOnce(Observation, Observation, &Path, &Path) -> Result<T, HarnessError>,
) -> Result<T, HarnessError> {
    if plan.state_environment_variable.trim().is_empty() {
        return Err(HarnessError::Io {
            operation: "validate state environment variable".into(),
            message: "name is empty".into(),
        });
    }
    let original_state = DisposableState::new("original")?;
    let rust_state = DisposableState::new("rust")?;
    prepare_initial_state(original_state.path(), &plan.scenario.initial_files)?;
    prepare_initial_state(rust_state.path(), &plan.scenario.initial_files)?;

    let original = execute(
        &plan.original,
        &plan.scenario,
        &plan.captures,
        &plan.state_environment_variable,
        original_state.path(),
    );
    let rust = execute(
        &plan.rust,
        &plan.scenario,
        &plan.captures,
        &plan.state_environment_variable,
        rust_state.path(),
    );

    finish(original, rust, original_state.path(), rust_state.path())
}

fn compare_with_roots(
    scenario: &Scenario,
    original_raw: Observation,
    rust_raw: Observation,
    rules: &[NormalizationRule],
    original_root: Option<&Path>,
    rust_root: Option<&Path>,
) -> Result<DifferentialManifest, HarnessError> {
    validate_rules(rules)?;
    let original = evidence(original_raw, rules, original_root)?;
    let rust = evidence(rust_raw, rules, rust_root)?;
    let differences = compare_slots(scenario, &original.normalized, &rust.normalized);
    let (executed_fixtures, executed_assertions) = counts(&original.raw);

    Ok(DifferentialManifest {
        format_version: 1,
        scenario: scenario.clone(),
        rules: rules.to_vec(),
        original,
        rust,
        equivalent: differences.is_empty(),
        differences,
        executed_fixtures,
        executed_assertions,
    })
}

fn validate_rules(rules: &[NormalizationRule]) -> Result<(), HarnessError> {
    let mut ids = BTreeSet::new();
    for rule in rules {
        let invalid = if rule.id.trim().is_empty() {
            Some("id is empty")
        } else if rule.reason.trim().is_empty() {
            Some("reason is empty")
        } else if rule.owner.trim().is_empty() {
            Some("owner is empty")
        } else if rule.exact_values.is_empty() || rule.exact_values.iter().any(String::is_empty) {
            Some("exact values are empty")
        } else if !ids.insert(rule.id.clone()) {
            Some("id is duplicated")
        } else {
            None
        };
        if let Some(message) = invalid {
            return Err(HarnessError::InvalidRule {
                id: rule.id.clone(),
                message: message.into(),
            });
        }
    }
    Ok(())
}

fn evidence(
    raw: Observation,
    rules: &[NormalizationRule],
    state_root: Option<&Path>,
) -> Result<Evidence, HarnessError> {
    let raw_digests = digest_observation(&raw);
    let mut normalized = raw.clone();
    for rule in rules {
        apply_rule(&mut normalized, rule, state_root)?;
    }
    Ok(Evidence {
        raw,
        normalized,
        raw_digests,
    })
}

fn apply_rule(
    observation: &mut Observation,
    rule: &NormalizationRule,
    state_root: Option<&Path>,
) -> Result<(), HarnessError> {
    let token = rule.token();
    let exact_values = resolved_values(rule, state_root);
    let matched = match &rule.target {
        NormalizationTarget::StructuredJsonPointer(pointer) => {
            let ObservationSlot::Value(value) = &mut observation.structured_output else {
                return Err(miss(rule));
            };
            let Some(Value::String(value)) = value.pointer_mut(pointer) else {
                return Err(miss(rule));
            };
            replace_exact_value(value, &exact_values, &token)
        }
        NormalizationTarget::Stdout => replace_bytes_slot(
            &mut observation.stdout,
            rule,
            &exact_values,
            &token,
            "stdout",
        )?,
        NormalizationTarget::Stderr => replace_bytes_slot(
            &mut observation.stderr,
            rule,
            &exact_values,
            &token,
            "stderr",
        )?,
        NormalizationTarget::Artifact(name) => replace_artifact_slot(
            &mut observation.artifacts,
            name,
            rule,
            &exact_values,
            &token,
            "artifact",
        )?,
        NormalizationTarget::State(name) => replace_artifact_slot(
            &mut observation.state,
            name,
            rule,
            &exact_values,
            &token,
            "state",
        )?,
    };
    if matched { Ok(()) } else { Err(miss(rule)) }
}

fn replace_bytes_slot(
    slot: &mut ObservationSlot<Vec<u8>>,
    rule: &NormalizationRule,
    exact_values: &[String],
    token: &str,
    target: &str,
) -> Result<bool, HarnessError> {
    let ObservationSlot::Value(bytes) = slot else {
        return Ok(false);
    };
    let text = std::str::from_utf8(bytes).map_err(|_| HarnessError::NonTextArtifact {
        id: rule.id.clone(),
        target: target.into(),
    })?;
    let (normalized, matched) = replace_declared(text, exact_values, token);
    if matched {
        *bytes = normalized.into_bytes();
    }
    Ok(matched)
}

fn replace_artifact_slot(
    slot: &mut ObservationSlot<Vec<Artifact>>,
    name: &str,
    rule: &NormalizationRule,
    exact_values: &[String],
    token: &str,
    kind: &str,
) -> Result<bool, HarnessError> {
    let ObservationSlot::Value(artifacts) = slot else {
        return Ok(false);
    };
    let Some(artifact) = artifacts.iter_mut().find(|artifact| artifact.name == name) else {
        return Ok(false);
    };
    let target = format!("{kind}:{name}");
    let text = std::str::from_utf8(&artifact.bytes).map_err(|_| HarnessError::NonTextArtifact {
        id: rule.id.clone(),
        target,
    })?;
    let (normalized, matched) = replace_declared(text, exact_values, token);
    if matched {
        artifact.bytes = normalized.into_bytes();
    }
    Ok(matched)
}

fn replace_exact_value(value: &mut String, allowed: &[String], token: &str) -> bool {
    if allowed.contains(value) {
        token.clone_into(value);
        true
    } else {
        false
    }
}

fn replace_declared(text: &str, allowed: &[String], token: &str) -> (String, bool) {
    let mut result = text.to_owned();
    let mut matched = false;
    for exact in allowed {
        if result.contains(exact) {
            result = result.replace(exact, token);
            matched = true;
        }
    }
    (result, matched)
}

fn resolved_values(rule: &NormalizationRule, state_root: Option<&Path>) -> Vec<String> {
    rule.exact_values
        .iter()
        .map(|value| {
            if value == "$PASEO_DIFFERENTIAL_STATE" {
                state_root.map_or_else(|| value.clone(), |root| root.display().to_string())
            } else {
                value.clone()
            }
        })
        .collect()
}

fn miss(rule: &NormalizationRule) -> HarnessError {
    HarnessError::NormalizationMiss {
        id: rule.id.clone(),
        target: format!("{:?}", rule.target),
    }
}

fn compare_slots(
    scenario: &Scenario,
    original: &Observation,
    rust: &Observation,
) -> Vec<Comparison> {
    let mut differences = Vec::new();
    compare_slot(
        &mut differences,
        "structured_output",
        &original.structured_output,
        &rust.structured_output,
    );
    compare_slot(&mut differences, "stdout", &original.stdout, &rust.stdout);
    compare_slot(&mut differences, "stderr", &original.stderr, &rust.stderr);
    compare_slot(
        &mut differences,
        "exit_code",
        &original.exit_code,
        &rust.exit_code,
    );
    compare_slot(
        &mut differences,
        "artifacts",
        &original.artifacts,
        &rust.artifacts,
    );
    compare_slot(&mut differences, "state", &original.state, &rust.state);
    compare_slot(
        &mut differences,
        "screenshots",
        &original.screenshots,
        &rust.screenshots,
    );
    compare_slot(
        &mut differences,
        "accessibility",
        &original.accessibility,
        &rust.accessibility,
    );
    compare_slot(
        &mut differences,
        "performance",
        &original.performance,
        &rust.performance,
    );
    compare_slot(
        &mut differences,
        "recovery",
        &original.recovery,
        &rust.recovery,
    );
    compare_slot(&mut differences, "counts", &original.counts, &rust.counts);
    if original.counts != ObservationSlot::Value(scenario.expected_counts)
        || rust.counts != ObservationSlot::Value(scenario.expected_counts)
    {
        differences.push(Comparison::different("counts:expected"));
    }
    differences
}

fn compare_slot<T: PartialEq>(
    differences: &mut Vec<Comparison>,
    path: &str,
    original: &ObservationSlot<T>,
    rust: &ObservationSlot<T>,
) {
    if matches!(original, ObservationSlot::Error(_))
        || matches!(rust, ObservationSlot::Error(_))
        || original != rust
    {
        differences.push(Comparison::different(path));
    }
}

fn counts(observation: &Observation) -> (u64, u64) {
    match observation.counts {
        ObservationSlot::Value(counts) => (counts.fixtures, counts.assertions),
        ObservationSlot::Missing | ObservationSlot::Error(_) => (0, 0),
    }
}

fn digest_observation(observation: &Observation) -> BTreeMap<String, String> {
    let mut digests = BTreeMap::new();
    digest_serialized(
        &mut digests,
        "structured_output",
        &observation.structured_output,
    );
    digest_serialized(&mut digests, "stdout", &observation.stdout);
    digest_serialized(&mut digests, "stderr", &observation.stderr);
    digest_serialized(&mut digests, "exit_code", &observation.exit_code);
    digest_artifacts(&mut digests, "artifact", &observation.artifacts);
    digest_artifacts(&mut digests, "state", &observation.state);
    digest_artifacts(&mut digests, "screenshot", &observation.screenshots);
    digest_serialized(&mut digests, "accessibility", &observation.accessibility);
    digest_serialized(&mut digests, "performance", &observation.performance);
    digest_serialized(&mut digests, "recovery", &observation.recovery);
    digest_serialized(&mut digests, "counts", &observation.counts);
    digests
}

fn digest_artifacts(
    digests: &mut BTreeMap<String, String>,
    prefix: &str,
    slot: &ObservationSlot<Vec<Artifact>>,
) {
    match slot {
        ObservationSlot::Value(artifacts) => {
            for artifact in artifacts {
                digests.insert(
                    format!("{prefix}:{}", artifact.name),
                    sha256(&artifact.bytes),
                );
            }
        }
        ObservationSlot::Missing | ObservationSlot::Error(_) => {
            digest_serialized(digests, prefix, slot);
        }
    }
}

fn digest_serialized<T: Serialize>(digests: &mut BTreeMap<String, String>, name: &str, value: &T) {
    let bytes = serde_json::to_vec(value).expect("serializable observation value");
    digests.insert(name.into(), sha256(&bytes));
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a string cannot fail");
            hex
        })
}

static STATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct DisposableState {
    path: PathBuf,
}

impl DisposableState {
    fn new(side: &str) -> Result<Self, HarnessError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| HarnessError::Io {
                operation: "read system clock".into(),
                message: error.to_string(),
            })?
            .as_nanos();
        let sequence = STATE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "paseo-differential-{side}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(|error| io_error("create disposable state", &error))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DisposableState {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn prepare_initial_state(root: &Path, files: &[Artifact]) -> Result<(), HarnessError> {
    for file in files {
        let relative = Path::new(&file.name);
        let path = resolve_inside(root, relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create initial state parent", &error))?;
        }
        fs::write(path, &file.bytes).map_err(|error| io_error("write initial state", &error))?;
    }
    Ok(())
}

fn execute(
    process: &ProcessSpec,
    scenario: &Scenario,
    captures: &CapturePlan,
    state_variable: &str,
    state_root: &Path,
) -> Observation {
    let stdout_path = state_root.join(".paseo-differential-stdout");
    let stderr_path = state_root.join(".paseo-differential-stderr");
    let stdout_file = fs::File::create(&stdout_path);
    let stderr_file = fs::File::create(&stderr_path);
    let process_result = stdout_file
        .and_then(|stdout_file| stderr_file.map(|stderr_file| (stdout_file, stderr_file)));

    let process_result = process_result.and_then(|(stdout_file, stderr_file)| {
        Command::new(&process.program)
            .args(&process.arguments)
            .args(&scenario.arguments)
            .envs(&process.environment)
            .envs(&scenario.environment)
            .env(state_variable, state_root)
            .current_dir(state_root)
            .stdout(stdout_file)
            .stderr(stderr_file)
            .spawn()
    });

    let (stdout, stderr, exit_code) = match process_result {
        Ok(mut child) => {
            let timeout = Duration::from_millis(process.timeout_ms.max(1));
            let deadline = Instant::now() + timeout;
            let exit_code = loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        break status.code().map_or_else(
                            || {
                                ObservationSlot::Error(
                                    "process terminated without exit code".into(),
                                )
                            },
                            ObservationSlot::Value,
                        );
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break ObservationSlot::Error(format!(
                            "process timed out after {} ms",
                            process.timeout_ms.max(1)
                        ));
                    }
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break ObservationSlot::Error(format!("wait for process: {error}"));
                    }
                }
            };
            (
                read_process_output(&stdout_path, "stdout"),
                read_process_output(&stderr_path, "stderr"),
                exit_code,
            )
        }
        Err(error) => {
            let message = format!("cannot execute {}: {error}", process.program.display());
            (
                ObservationSlot::Error(message.clone()),
                ObservationSlot::Error(message.clone()),
                ObservationSlot::Error(message),
            )
        }
    };

    Observation {
        structured_output: capture_json(state_root, captures.structured_output.as_deref()),
        stdout,
        stderr,
        exit_code,
        artifacts: capture_artifacts(state_root, &captures.artifacts),
        state: capture_artifacts(state_root, &captures.state),
        screenshots: capture_artifacts(state_root, &captures.screenshots),
        accessibility: capture_json(state_root, captures.accessibility.as_deref()),
        performance: capture_json(state_root, captures.performance.as_deref()),
        recovery: capture_json(state_root, captures.recovery.as_deref()),
        counts: capture_counts(state_root, captures.counts.as_deref()),
    }
}

fn read_process_output(path: &Path, name: &str) -> ObservationSlot<Vec<u8>> {
    fs::read(path).map_or_else(
        |error| ObservationSlot::Error(format!("read process {name}: {error}")),
        ObservationSlot::Value,
    )
}

fn capture_json(root: &Path, relative: Option<&Path>) -> ObservationSlot<Value> {
    let Some(relative) = relative else {
        return ObservationSlot::Missing;
    };
    read_capture(root, relative)
        .and_then(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("parse {} as JSON: {error}", relative.display()))
        })
        .map_or_else(ObservationSlot::Error, ObservationSlot::Value)
}

fn capture_counts(root: &Path, relative: Option<&Path>) -> ObservationSlot<ExecutionCounts> {
    let Some(relative) = relative else {
        return ObservationSlot::Missing;
    };
    read_capture(root, relative)
        .and_then(|bytes| {
            serde_json::from_slice(&bytes).map_err(|error| {
                format!("parse {} as execution counts: {error}", relative.display())
            })
        })
        .map_or_else(ObservationSlot::Error, ObservationSlot::Value)
}

fn capture_artifacts(root: &Path, paths: &[PathBuf]) -> ObservationSlot<Vec<Artifact>> {
    if paths.is_empty() {
        return ObservationSlot::Missing;
    }
    paths
        .iter()
        .map(|relative| {
            read_capture(root, relative)
                .map(|bytes| Artifact::new(relative.display().to_string(), bytes))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_or_else(ObservationSlot::Error, ObservationSlot::Value)
}

fn read_capture(root: &Path, relative: &Path) -> Result<Vec<u8>, String> {
    let path = resolve_inside(root, relative).map_err(|error| error.to_string())?;
    fs::read(path).map_err(|error| format!("read capture {}: {error}", relative.display()))
}

fn resolve_inside(root: &Path, relative: &Path) -> Result<PathBuf, HarnessError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(HarnessError::UnsafePath(relative.to_path_buf()));
    }
    Ok(root.join(relative))
}

fn io_error(operation: &str, error: &io::Error) -> HarnessError {
    HarnessError::Io {
        operation: operation.into(),
        message: error.to_string(),
    }
}
