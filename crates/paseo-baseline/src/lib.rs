use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub name: String,
    pub path: PathBuf,
    pub expected: String,
}

impl Baseline {
    /// Creates an immutable source baseline descriptor.
    #[must_use]
    pub fn new(name: impl Into<String>, path: PathBuf, expected: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path,
            expected: expected.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBaseline {
    pub name: String,
    pub path: PathBuf,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineError {
    GitUnavailable(String),
    GitCommandFailed {
        name: String,
        path: PathBuf,
        stderr: String,
    },
    HeadMismatch {
        name: String,
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

impl Display for BaselineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitUnavailable(message) => write!(formatter, "cannot execute git: {message}"),
            Self::GitCommandFailed { name, path, stderr } => write!(
                formatter,
                "cannot read {name} baseline at {}: {stderr}",
                path.display()
            ),
            Self::HeadMismatch {
                name,
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "{name} baseline at {} is {actual}, expected {expected}",
                path.display()
            ),
        }
    }
}

impl Error for BaselineError {}

/// Verifies that each baseline checkout points at its expected commit.
///
/// # Errors
///
/// Returns an error when Git cannot inspect a checkout or when a checkout has
/// moved away from its immutable commit.
pub fn verify_baselines(baselines: &[Baseline]) -> Result<Vec<VerifiedBaseline>, BaselineError> {
    baselines.iter().map(verify_baseline).collect()
}

fn verify_baseline(baseline: &Baseline) -> Result<VerifiedBaseline, BaselineError> {
    let actual = git_head(&baseline.name, &baseline.path)?;
    if actual != baseline.expected {
        return Err(BaselineError::HeadMismatch {
            name: baseline.name.clone(),
            path: baseline.path.clone(),
            expected: baseline.expected.clone(),
            actual,
        });
    }

    Ok(VerifiedBaseline {
        name: baseline.name.clone(),
        path: baseline.path.clone(),
        expected: baseline.expected.clone(),
        actual,
    })
}

fn git_head(name: &str, path: &Path) -> Result<String, BaselineError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| BaselineError::GitUnavailable(error.to_string()))?;

    if !output.status.success() {
        return Err(BaselineError::GitCommandFailed {
            name: name.to_owned(),
            path: path.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
