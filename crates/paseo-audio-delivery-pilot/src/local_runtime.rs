use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::NativeCapability;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCommand {
    program: OsString,
    args: Vec<OsString>,
    timeout: Duration,
}

impl ProcessCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs the command within its deadline while draining both output streams.
    ///
    /// # Errors
    ///
    /// Returns an error when the process times out or the operating system cannot launch or wait
    /// for it. A timeout terminates the process group on Unix and the child process elsewhere.
    pub fn run(&self) -> io::Result<ProcessResult> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("child stdout pipe is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("child stderr pipe is unavailable"))?;
        let stdout_reader = read_stream(stdout);
        let stderr_reader = read_stream(stderr);
        let started = Instant::now();

        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= self.timeout {
                terminate_process_tree(&mut child)?;
                let _ = child.wait();
                let _ = join_stream(stdout_reader);
                let _ = join_stream(stderr_reader);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "process timed out after {} ms: {}",
                        self.timeout.as_millis(),
                        self.program.to_string_lossy()
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        };

        Ok(ProcessResult {
            exit_code: status.code(),
            stdout: join_stream(stdout_reader)?,
            stderr: join_stream(stderr_reader)?,
        })
    }
}

fn read_stream(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_stream(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("output reader thread panicked"))?
}

#[cfg(unix)]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    let process_group = format!("-{}", child.id());
    let killed = Command::new("/bin/kill")
        .args(["-KILL", "--", &process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if killed.success() {
        Ok(())
    } else {
        child.kill()
    }
}

#[cfg(not(unix))]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    child.kill()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidDeviceIdentity {
    pub serial: String,
    pub android_release: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRuntimeEvidence {
    pub capability: NativeCapability,
    pub succeeded: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl NativeRuntimeEvidence {
    #[must_use]
    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidDeviceAdapter {
    adb: PathBuf,
    serial: String,
}

impl AndroidDeviceAdapter {
    pub fn new(adb: impl Into<PathBuf>, serial: impl Into<String>) -> Self {
        Self {
            adb: adb.into(),
            serial: serial.into(),
        }
    }

    /// Reads identity properties from a connected Android device.
    ///
    /// # Errors
    ///
    /// Returns an error when `adb` cannot run or a property query fails.
    pub fn identity(&self) -> Result<AndroidDeviceIdentity, RuntimeError> {
        Ok(AndroidDeviceIdentity {
            serial: self.serial.clone(),
            android_release: self.get_property("ro.build.version.release")?,
            model: self.get_property("ro.product.model")?,
        })
    }

    /// Executes the closest safe OS-level command for a native capability.
    ///
    /// Push requires external service credentials, so the adapter records it as unsupported
    /// without contacting a production service.
    ///
    /// # Errors
    ///
    /// Returns an error when `adb` cannot launch.
    pub fn invoke(
        &self,
        capability: NativeCapability,
    ) -> Result<NativeRuntimeEvidence, RuntimeError> {
        let args: &[&str] = match capability {
            NativeCapability::Push => {
                return Ok(NativeRuntimeEvidence {
                    capability,
                    succeeded: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: "push requires test service credentials".to_owned(),
                });
            }
            NativeCapability::Camera => &[
                "shell",
                "am",
                "start",
                "-a",
                "android.media.action.IMAGE_CAPTURE",
            ],
            NativeCapability::FilePicker => &[
                "shell",
                "am",
                "start",
                "-a",
                "android.intent.action.OPEN_DOCUMENT",
                "-c",
                "android.intent.category.OPENABLE",
                "-t",
                "text/plain",
            ],
            NativeCapability::Haptics => &[
                "shell",
                "cmd",
                "vibrator_manager",
                "synced",
                "oneshot",
                "100",
            ],
            NativeCapability::Notifications => &[
                "shell",
                "cmd",
                "notification",
                "post",
                "paseo-runtime",
                "Paseo runtime notification",
            ],
            NativeCapability::Background => &["shell", "input", "keyevent", "KEYCODE_HOME"],
            NativeCapability::DeepLink => &[
                "shell",
                "am",
                "start",
                "-a",
                "android.intent.action.VIEW",
                "-d",
                "paseo://app/",
            ],
        };
        let result = self.adb_command(args).run()?;
        let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        let combined = format!("{stdout}{stderr}");
        let succeeded = result.exit_code == Some(0)
            && !combined.contains("Error:")
            && !combined.contains("unable to resolve Intent");
        Ok(NativeRuntimeEvidence {
            capability,
            succeeded,
            exit_code: result.exit_code,
            stdout,
            stderr,
        })
    }

    fn get_property(&self, property: &str) -> Result<String, RuntimeError> {
        let result = self.adb_command(&["shell", "getprop", property]).run()?;
        if result.exit_code != Some(0) {
            return Err(RuntimeError::ProcessFailed(
                String::from_utf8_lossy(&result.stderr).trim().to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
    }

    fn adb_command(&self, args: &[&str]) -> ProcessCommand {
        ProcessCommand::new(&self.adb)
            .args([OsStr::new("-s"), OsStr::new(&self.serial)])
            .args(args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFileMetadata {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: usize,
    pub file_bytes: usize,
}

/// Writes mono, signed 16-bit PCM samples in a RIFF/WAVE container.
///
/// # Errors
///
/// Returns an error for an invalid sample rate, oversized input, or filesystem failure.
pub fn create_pcm16_wav(
    path: &Path,
    sample_rate: u32,
    samples: &[i16],
) -> Result<PcmFileMetadata, RuntimeError> {
    if sample_rate == 0 {
        return Err(RuntimeError::InvalidPackage("sample rate must be positive"));
    }
    let data_bytes = samples
        .len()
        .checked_mul(2)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or(RuntimeError::InvalidPackage("PCM input is too large"))?;
    let riff_bytes = 36_u32
        .checked_add(data_bytes)
        .ok_or(RuntimeError::InvalidPackage("PCM input is too large"))?;
    let byte_rate = sample_rate
        .checked_mul(2)
        .ok_or(RuntimeError::InvalidPackage("sample rate is too large"))?;

    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_bytes.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, &bytes)?;

    Ok(PcmFileMetadata {
        sample_rate,
        channels: 1,
        samples: samples.len(),
        file_bytes: bytes.len(),
    })
}

#[derive(Debug)]
pub enum RuntimeError {
    Io(io::Error),
    InvalidPackage(&'static str),
    ProcessFailed(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidPackage(message) => formatter.write_str(message),
            Self::ProcessFailed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidPackage(_) | Self::ProcessFailed(_) => None,
        }
    }
}

impl From<io::Error> for RuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverySnapshot {
    pub active_version: Option<String>,
    pub active_payload_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocalDeliveryRuntime {
    root: PathBuf,
}

impl LocalDeliveryRuntime {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Installs an explicitly unsigned package into an empty local root.
    ///
    /// # Errors
    ///
    /// Returns an error when a package is invalid, an install already exists, or I/O fails.
    pub fn install(&mut self, package: &Path) -> Result<DeliverySnapshot, RuntimeError> {
        if self.active_version()?.is_some() {
            return Err(RuntimeError::InvalidPackage("already installed"));
        }
        let manifest = validate_package(package)?;
        self.copy_package(package, &manifest.version)?;
        atomic_write(&self.active_version_path(), manifest.version.as_bytes())?;
        self.snapshot()
    }

    /// Validates and activates a new unsigned package while retaining the old version.
    ///
    /// # Errors
    ///
    /// Returns an error when no install exists, the package is invalid, or I/O fails.
    pub fn update(&mut self, package: &Path) -> Result<DeliverySnapshot, RuntimeError> {
        let previous = self
            .active_version()?
            .ok_or(RuntimeError::InvalidPackage("not installed"))?;
        let manifest = validate_package(package)?;
        self.copy_package(package, &manifest.version)?;
        atomic_write(&self.rollback_version_path(), previous.as_bytes())?;
        atomic_write(&self.active_version_path(), manifest.version.as_bytes())?;
        self.snapshot()
    }

    /// Restores the version retained by the most recent successful update.
    ///
    /// # Errors
    ///
    /// Returns an error when rollback state is absent or I/O fails.
    pub fn rollback(&mut self) -> Result<DeliverySnapshot, RuntimeError> {
        let rollback = read_trimmed(&self.rollback_version_path())?
            .ok_or(RuntimeError::InvalidPackage("rollback unavailable"))?;
        atomic_write(&self.active_version_path(), rollback.as_bytes())?;
        fs::remove_file(self.rollback_version_path())?;
        self.snapshot()
    }

    /// Removes the active install and optionally retains its payload as user state evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when no install exists or I/O fails.
    pub fn uninstall(&mut self, retain_state: bool) -> Result<DeliverySnapshot, RuntimeError> {
        if self.active_version()?.is_none() {
            return Err(RuntimeError::InvalidPackage("not installed"));
        }
        if retain_state {
            let payload = fs::read(self.active_payload())?;
            atomic_write(&self.retained_state_path(), &payload)?;
        }
        fs::remove_file(self.active_version_path())?;
        if self.rollback_version_path().exists() {
            fs::remove_file(self.rollback_version_path())?;
        }
        self.snapshot()
    }

    /// Reads the active version marker.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker cannot be read.
    pub fn active_version(&self) -> Result<Option<String>, RuntimeError> {
        read_trimmed(&self.active_version_path())
    }

    #[must_use]
    pub fn active_payload(&self) -> PathBuf {
        let version = self
            .active_version()
            .ok()
            .flatten()
            .unwrap_or_else(|| "absent".to_owned());
        self.root.join("versions").join(version).join("payload.bin")
    }

    #[must_use]
    pub fn retained_state_path(&self) -> PathBuf {
        self.root.join("state").join("retained.bin")
    }

    fn active_version_path(&self) -> PathBuf {
        self.root.join("active-version")
    }

    fn rollback_version_path(&self) -> PathBuf {
        self.root.join("rollback-version")
    }

    fn copy_package(&self, package: &Path, version: &str) -> Result<(), RuntimeError> {
        let destination = self.root.join("versions").join(version);
        fs::create_dir_all(&destination)?;
        fs::copy(
            package.join("manifest.txt"),
            destination.join("manifest.txt"),
        )?;
        fs::copy(package.join("payload.bin"), destination.join("payload.bin"))?;
        Ok(())
    }

    fn snapshot(&self) -> Result<DeliverySnapshot, RuntimeError> {
        let active_version = self.active_version()?;
        let active_payload_digest = active_version
            .as_ref()
            .map(|_| fs::read(self.active_payload()).map(|bytes| digest(&bytes)))
            .transpose()?;
        Ok(DeliverySnapshot {
            active_version,
            active_payload_digest,
        })
    }
}

/// Creates a local package marked as unsigned with a payload checksum.
///
/// # Errors
///
/// Returns an error when the package directory or files cannot be written.
pub fn create_unsigned_package(
    package: &Path,
    version: &str,
    payload: &[u8],
) -> Result<(), RuntimeError> {
    if version.is_empty() || version.contains(['\n', '\r', '/']) {
        return Err(RuntimeError::InvalidPackage("invalid version"));
    }
    fs::create_dir_all(package)?;
    fs::write(package.join("payload.bin"), payload)?;
    let manifest = format!(
        "format=paseo-local-package-v1\nsigning=unsigned\nversion={version}\npayload_digest={}\n",
        digest(payload)
    );
    fs::write(package.join("manifest.txt"), manifest)?;
    Ok(())
}

struct PackageManifest {
    version: String,
}

fn validate_package(package: &Path) -> Result<PackageManifest, RuntimeError> {
    let manifest = fs::read_to_string(package.join("manifest.txt"))?;
    let value = |key: &str| {
        manifest
            .lines()
            .find_map(|line| line.strip_prefix(key).map(str::to_owned))
    };
    if value("format=").as_deref() != Some("paseo-local-package-v1") {
        return Err(RuntimeError::InvalidPackage("unsupported package format"));
    }
    if value("signing=").as_deref() != Some("unsigned") {
        return Err(RuntimeError::InvalidPackage(
            "package must be explicitly unsigned",
        ));
    }
    let version = value("version=").ok_or(RuntimeError::InvalidPackage("missing version"))?;
    let expected_digest =
        value("payload_digest=").ok_or(RuntimeError::InvalidPackage("missing payload checksum"))?;
    let payload = fs::read(package.join("payload.bin"))?;
    if digest(&payload) != expected_digest {
        return Err(RuntimeError::InvalidPackage("payload checksum mismatch"));
    }
    Ok(PackageManifest { version })
}

fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), RuntimeError> {
    let parent = path
        .parent()
        .ok_or(RuntimeError::InvalidPackage("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_trimmed(path: &Path) -> Result<Option<String>, RuntimeError> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value.trim().to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
