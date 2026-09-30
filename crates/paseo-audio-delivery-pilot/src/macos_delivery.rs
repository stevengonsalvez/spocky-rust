use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{ProcessResult, RuntimeError};
use serde::Serialize;

const APP_NAME: &str = "Paseo.app";
const EXECUTABLE_NAME: &str = "Paseo";
const BUNDLE_ID: &str = "sh.paseo.desktop";
const MANIFEST_NAME: &str = "paseo-update-manifest.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOsAppDeliverySnapshot {
    pub active_version: Option<String>,
    pub executable_digest: Option<String>,
    pub state_digest: Option<String>,
    pub bundle_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MacOsAppDeliveryRuntime {
    installation_root: PathBuf,
    state_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacOsDeliveryEvidenceStep {
    pub operation: String,
    pub outcome: String,
    pub active_version: Option<String>,
    pub executable_digest: Option<String>,
    pub state_digest: Option<String>,
    pub launch_stdout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacOsDeliveryEvidenceReport {
    pub schema_version: u32,
    pub contract_id: String,
    pub baseline: String,
    pub package_format: String,
    pub installed_bundle_path: PathBuf,
    pub retained_state_path: PathBuf,
    pub corrupt_update_preserved_executable: bool,
    pub corrupt_update_preserved_state: bool,
    pub steps: Vec<MacOsDeliveryEvidenceStep>,
    pub limitations: Vec<String>,
}

impl MacOsAppDeliveryRuntime {
    #[must_use]
    pub fn new(installation_root: PathBuf, state_root: PathBuf) -> Self {
        Self {
            installation_root,
            state_root,
        }
    }

    /// Installs a validated unsigned macOS application bundle into a disposable root.
    ///
    /// # Errors
    ///
    /// Returns an error if an install exists, bundle validation fails, or filesystem work fails.
    pub fn install(
        &mut self,
        source_bundle: &Path,
    ) -> Result<MacOsAppDeliverySnapshot, RuntimeError> {
        if self.bundle_path().exists() {
            return Err(RuntimeError::InvalidPackage("already installed"));
        }
        validate_bundle(source_bundle)?;
        fs::create_dir_all(&self.installation_root)?;
        let staging = self.staging_path();
        remove_directory_if_present(&staging)?;
        copy_directory(source_bundle, &staging)?;
        validate_bundle(&staging)?;
        fs::rename(&staging, self.bundle_path())?;
        self.snapshot()
    }

    /// Activates a validated bundle while retaining the previous bundle for rollback.
    ///
    /// # Errors
    ///
    /// Returns an error if no install exists, validation fails, or filesystem work fails.
    pub fn update(
        &mut self,
        source_bundle: &Path,
    ) -> Result<MacOsAppDeliverySnapshot, RuntimeError> {
        let active = self.bundle_path();
        if !active.exists() {
            return Err(RuntimeError::InvalidPackage("not installed"));
        }
        validate_bundle(source_bundle)?;

        let staging = self.staging_path();
        remove_directory_if_present(&staging)?;
        copy_directory(source_bundle, &staging)?;
        if let Err(error) = validate_bundle(&staging) {
            remove_directory_if_present(&staging)?;
            return Err(error);
        }

        let rollback = self.rollback_path();
        remove_directory_if_present(&rollback)?;
        fs::rename(&active, &rollback)?;
        if let Err(error) = fs::rename(&staging, &active) {
            fs::rename(&rollback, &active)?;
            return Err(error.into());
        }
        self.snapshot()
    }

    /// Swaps the active and retained application bundles.
    ///
    /// # Errors
    ///
    /// Returns an error if rollback is unavailable or filesystem work fails.
    pub fn rollback(&mut self) -> Result<MacOsAppDeliverySnapshot, RuntimeError> {
        let active = self.bundle_path();
        let rollback = self.rollback_path();
        if !active.exists() || !rollback.exists() {
            return Err(RuntimeError::InvalidPackage("rollback unavailable"));
        }

        let temporary = self.installation_root.join(".Paseo.app.rollback-swap");
        remove_directory_if_present(&temporary)?;
        fs::rename(&active, &temporary)?;
        if let Err(error) = fs::rename(&rollback, &active) {
            fs::rename(&temporary, &active)?;
            return Err(error.into());
        }
        fs::rename(&temporary, &rollback)?;
        self.snapshot()
    }

    /// Removes installed bundles and optionally preserves user state outside the bundle.
    ///
    /// # Errors
    ///
    /// Returns an error if no install exists or filesystem work fails.
    pub fn uninstall(
        &mut self,
        retain_state: bool,
    ) -> Result<MacOsAppDeliverySnapshot, RuntimeError> {
        if !self.bundle_path().exists() {
            return Err(RuntimeError::InvalidPackage("not installed"));
        }
        remove_directory_if_present(&self.bundle_path())?;
        remove_directory_if_present(&self.rollback_path())?;
        if !retain_state {
            remove_directory_if_present(&self.state_root)?;
        }
        self.snapshot()
    }

    /// Launches the executable from the active application bundle.
    ///
    /// # Errors
    ///
    /// Returns an error if no valid bundle exists or the process cannot launch.
    pub fn launch(&self) -> Result<ProcessResult, RuntimeError> {
        validate_bundle(&self.bundle_path())?;
        let output = Command::new(self.executable_path())
            .env("PASEO_STATE_FILE", self.user_state_path())
            .output()?;
        Ok(ProcessResult {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    /// Writes disposable user state outside the application bundle.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be written atomically.
    pub fn write_user_state(&self, state: &[u8]) -> Result<(), RuntimeError> {
        fs::create_dir_all(&self.state_root)?;
        let target = self.user_state_path();
        let temporary = target.with_extension("tmp");
        fs::write(&temporary, state)?;
        fs::rename(temporary, target)?;
        Ok(())
    }

    /// Reads current bundle and state digests.
    ///
    /// # Errors
    ///
    /// Returns an error if installed content or state cannot be read.
    pub fn snapshot(&self) -> Result<MacOsAppDeliverySnapshot, RuntimeError> {
        let bundle = self.bundle_path();
        let (active_version, executable_digest) = if bundle.exists() {
            let manifest = validate_bundle(&bundle)?;
            (Some(manifest.version), Some(manifest.executable_digest))
        } else {
            (None, None)
        };
        let state_digest = match fs::read(self.user_state_path()) {
            Ok(bytes) => Some(digest(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(MacOsAppDeliverySnapshot {
            active_version,
            executable_digest,
            state_digest,
            bundle_path: bundle,
        })
    }

    #[must_use]
    pub fn bundle_path(&self) -> PathBuf {
        self.installation_root.join(APP_NAME)
    }

    #[must_use]
    pub fn user_state_path(&self) -> PathBuf {
        self.state_root.join("delivery-state.txt")
    }

    fn executable_path(&self) -> PathBuf {
        self.bundle_path()
            .join("Contents/MacOS")
            .join(EXECUTABLE_NAME)
    }

    fn rollback_path(&self) -> PathBuf {
        self.installation_root.join("Paseo.rollback.app")
    }

    fn staging_path(&self) -> PathBuf {
        self.installation_root.join(".Paseo.staging.app")
    }
}

/// Exercises install, launch, update rejection, upgrade, rollback, and uninstall on real bundles.
///
/// All paths remain under `root`. The run never signs, publishes, or touches `/Applications`.
///
/// # Errors
///
/// Returns an error when fixture creation, lifecycle work, or executable launch fails.
pub fn run_macos_delivery_feasibility(
    root: &Path,
) -> Result<MacOsDeliveryEvidenceReport, RuntimeError> {
    fs::create_dir_all(root)?;
    let packages = root.join("packages");
    let bundle_100 = packages.join("Paseo-1.0.0.app");
    let bundle_110 = packages.join("Paseo-1.1.0.app");
    let corrupt_120 = packages.join("Paseo-1.2.0-corrupt.app");
    create_unsigned_macos_app_bundle(&bundle_100, "1.0.0")?;
    create_unsigned_macos_app_bundle(&bundle_110, "1.1.0")?;
    create_unsigned_macos_app_bundle(&corrupt_120, "1.2.0")?;
    fs::write(
        corrupt_120.join("Contents/MacOS/Paseo"),
        b"#!/bin/sh\nprintf 'tampered\\n'\n",
    )?;

    let installation_root = root.join("installation");
    let state_root = root.join("state");
    let mut runtime = MacOsAppDeliveryRuntime::new(installation_root, state_root);
    runtime.write_user_state(b"agent-state-a\n")?;

    let mut steps = Vec::new();
    let installed = runtime.install(&bundle_100)?;
    let installed_launch = successful_launch_stdout(runtime.launch()?)?;
    steps.push(evidence_step(
        "install_launch",
        "supported",
        installed.clone(),
        Some(installed_launch),
    ));

    let before_rejected_update = runtime.snapshot()?;
    let Err(rejection) = runtime.update(&corrupt_120) else {
        return Err(RuntimeError::ProcessFailed(
            "corrupt app update was unexpectedly accepted".into(),
        ));
    };
    let after_rejected_update = runtime.snapshot()?;
    let rejected_launch = successful_launch_stdout(runtime.launch()?)?;
    steps.push(evidence_step(
        "corrupt_update_rejected",
        &rejection.to_string(),
        after_rejected_update.clone(),
        Some(rejected_launch),
    ));

    let upgraded = runtime.update(&bundle_110)?;
    let upgraded_launch = successful_launch_stdout(runtime.launch()?)?;
    steps.push(evidence_step(
        "valid_update_launch",
        "supported",
        upgraded,
        Some(upgraded_launch),
    ));

    let rolled_back = runtime.rollback()?;
    let rollback_launch = successful_launch_stdout(runtime.launch()?)?;
    steps.push(evidence_step(
        "rollback_launch",
        "supported",
        rolled_back,
        Some(rollback_launch),
    ));

    let uninstalled = runtime.uninstall(true)?;
    steps.push(evidence_step(
        "uninstall_retain_state",
        "supported",
        uninstalled,
        None,
    ));

    Ok(MacOsDeliveryEvidenceReport {
        schema_version: 1,
        contract_id: "P2-DELIVERY-01".into(),
        baseline: "paseo@5de45e208690b0efc51c59a585ae9729325a9204".into(),
        package_format: "unsigned_macos_app_bundle".into(),
        installed_bundle_path: runtime.bundle_path(),
        retained_state_path: runtime.user_state_path(),
        corrupt_update_preserved_executable: before_rejected_update.executable_digest
            == after_rejected_update.executable_digest,
        corrupt_update_preserved_state: before_rejected_update.state_digest
            == after_rejected_update.state_digest,
        steps,
        limitations: [
            "unsigned_disposable_bundle_only",
            "no_notarization_or_gatekeeper_assessment",
            "no_electron_updater_network_or_quit_and_install",
            "no_production_application_install",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    })
}

fn evidence_step(
    operation: &str,
    outcome: &str,
    snapshot: MacOsAppDeliverySnapshot,
    launch_stdout: Option<String>,
) -> MacOsDeliveryEvidenceStep {
    MacOsDeliveryEvidenceStep {
        operation: operation.into(),
        outcome: outcome.into(),
        active_version: snapshot.active_version,
        executable_digest: snapshot.executable_digest,
        state_digest: snapshot.state_digest,
        launch_stdout,
    }
}

fn successful_launch_stdout(result: ProcessResult) -> Result<String, RuntimeError> {
    if result.exit_code != Some(0) {
        return Err(RuntimeError::ProcessFailed(format!(
            "app executable exited with {:?}: {}",
            result.exit_code,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    String::from_utf8(result.stdout)
        .map_err(|_| RuntimeError::ProcessFailed("app executable emitted non-UTF-8 output".into()))
}

/// Creates a minimal unsigned macOS application bundle with an update checksum.
///
/// # Errors
///
/// Returns an error if the version is invalid or bundle files cannot be written.
pub fn create_unsigned_macos_app_bundle(bundle: &Path, version: &str) -> Result<(), RuntimeError> {
    validate_version(version)?;
    let contents = bundle.join("Contents");
    let executable_directory = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&executable_directory)?;
    fs::create_dir_all(&resources)?;

    let executable = format!(
        "#!/bin/sh\nset -eu\nprintf 'paseo-version={version}\\n'\nif [ -n \"${{PASEO_STATE_FILE:-}}\" ] && [ -f \"$PASEO_STATE_FILE\" ]; then\n  cat \"$PASEO_STATE_FILE\"\nfi\n"
    );
    let executable_path = executable_directory.join(EXECUTABLE_NAME);
    fs::write(&executable_path, executable.as_bytes())?;
    fs::set_permissions(&executable_path, fs::Permissions::from_mode(0o755))?;

    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>\n<key>CFBundleExecutable</key><string>{EXECUTABLE_NAME}</string>\n<key>CFBundleName</key><string>Paseo</string>\n<key>CFBundleShortVersionString</key><string>{version}</string>\n<key>CFBundleVersion</key><string>{version}</string>\n</dict></plist>\n"
    );
    fs::write(contents.join("Info.plist"), plist)?;

    let manifest = format!(
        "format=paseo-macos-app-v1\nsigning=unsigned\nbundle_id={BUNDLE_ID}\nversion={version}\nexecutable={EXECUTABLE_NAME}\nexecutable_digest={}\n",
        digest(executable.as_bytes())
    );
    fs::write(resources.join(MANIFEST_NAME), manifest)?;
    Ok(())
}

struct BundleManifest {
    version: String,
    executable_digest: String,
}

fn validate_bundle(bundle: &Path) -> Result<BundleManifest, RuntimeError> {
    if bundle.extension().and_then(|value| value.to_str()) != Some("app") {
        return Err(RuntimeError::InvalidPackage("package is not an app bundle"));
    }
    let contents = bundle.join("Contents");
    let manifest = fs::read_to_string(contents.join("Resources").join(MANIFEST_NAME))?;
    let value = |key: &str| {
        manifest
            .lines()
            .find_map(|line| line.strip_prefix(key).map(str::to_owned))
    };
    if value("format=").as_deref() != Some("paseo-macos-app-v1") {
        return Err(RuntimeError::InvalidPackage(
            "unsupported app bundle format",
        ));
    }
    if value("signing=").as_deref() != Some("unsigned") {
        return Err(RuntimeError::InvalidPackage(
            "app bundle must be explicitly unsigned",
        ));
    }
    if value("bundle_id=").as_deref() != Some(BUNDLE_ID) {
        return Err(RuntimeError::InvalidPackage(
            "unexpected app bundle identifier",
        ));
    }
    if value("executable=").as_deref() != Some(EXECUTABLE_NAME) {
        return Err(RuntimeError::InvalidPackage("unexpected app executable"));
    }
    let version = value("version=").ok_or(RuntimeError::InvalidPackage("missing app version"))?;
    validate_version(&version)?;
    let expected_digest = value("executable_digest=").ok_or(RuntimeError::InvalidPackage(
        "missing app executable checksum",
    ))?;
    let executable_path = contents.join("MacOS").join(EXECUTABLE_NAME);
    let executable = fs::read(&executable_path)?;
    let executable_digest = digest(&executable);
    if executable_digest != expected_digest {
        return Err(RuntimeError::InvalidPackage(
            "app executable checksum mismatch",
        ));
    }
    if fs::metadata(&executable_path)?.permissions().mode() & 0o111 == 0 {
        return Err(RuntimeError::InvalidPackage(
            "app executable is not executable",
        ));
    }
    let plist = fs::read_to_string(contents.join("Info.plist"))?;
    for expected in [
        format!("<key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>"),
        format!("<key>CFBundleExecutable</key><string>{EXECUTABLE_NAME}</string>"),
        format!("<key>CFBundleShortVersionString</key><string>{version}</string>"),
    ] {
        if !plist.contains(&expected) {
            return Err(RuntimeError::InvalidPackage("app Info.plist mismatch"));
        }
    }
    Ok(BundleManifest {
        version,
        executable_digest,
    })
}

fn validate_version(version: &str) -> Result<(), RuntimeError> {
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(RuntimeError::InvalidPackage("invalid app version"));
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), RuntimeError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(RuntimeError::InvalidPackage(
                "app bundle contains unsupported filesystem entry",
            ));
        }
    }
    Ok(())
}

fn remove_directory_if_present(path: &Path) -> Result<(), RuntimeError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}
