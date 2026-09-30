//! Linux delivery lifecycle against the pinned Paseo desktop packaging baseline.
//!
//! Two lanes share one report. The `AppImage` lane is a filesystem runtime that mirrors the
//! baseline's stable `AppImage` name, `~/.local/bin/paseo` link, and manifest integrity check. The
//! deb lane drives the real `dpkg` with baseline maintainer scripts and launcher, and must only
//! run in a disposable Linux root.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::sha512::{sha512_base64, sha512_hex};
use crate::{ProcessCommand, ProcessResult, RuntimeError};
use serde::Serialize;

pub const LINUX_DELIVERY_BASELINE: &str = "paseo@5de45e208690b0efc51c59a585ae9729325a9204";
pub const DISPOSABLE_ROOT_ENV: &str = "SPOCKY_DELIVERY_DISPOSABLE_ROOT";

// Compatibility identifiers: electron-builder.yml `appId`, `productName`, `executableName`,
// and the `appImage.artifactName` that keeps the updater path stable across versions.
const EXECUTABLE: &str = "Paseo";
const APPIMAGE_NAME: &str = "Paseo-x64.AppImage";
const PACKAGE_NAME: &str = "paseo";
const INSTALL_PREFIX: &str = "/opt/Paseo";
const USR_BIN_LINK: &str = "/usr/bin/Paseo";
const STATE_FILE: &str = "delivery-state.txt";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 4096;

/// Baseline launcher, `packages/desktop/scripts/linux-sandbox/launcher.sh`, byte for byte.
pub const BASELINE_LAUNCHER: &str = include_str!("../fixtures/linux/launcher.sh");
/// Baseline `deb.afterInstall`, `packages/desktop/scripts/linux-sandbox/after-install.tpl`.
pub const BASELINE_AFTER_INSTALL: &str = include_str!("../fixtures/linux/after-install.tpl");
/// electron-builder 26.8.1 default `templates/linux/after-remove.tpl` used by the baseline deb.
pub const BASELINE_AFTER_REMOVE: &str = include_str!("../fixtures/linux/after-remove.tpl");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxDeliveryStep {
    pub lane: String,
    pub operation: String,
    pub outcome: String,
    pub command: Option<Vec<String>>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
    pub active_version: Option<String>,
    pub executable_digest: Option<String>,
    pub state_digest: Option<String>,
    pub observations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxDeliveryEvidenceReport {
    pub schema_version: u32,
    pub contract_id: String,
    pub baseline: String,
    pub host: BTreeMap<String, String>,
    pub baseline_fixtures: BTreeMap<String, String>,
    pub appimage_corrupt_update_preserved_install: bool,
    pub deb_corrupt_update_preserved_install: bool,
    pub steps: Vec<LinuxDeliveryStep>,
    pub limitations: Vec<String>,
}

#[must_use]
pub fn linux_delivery_limitations() -> Vec<String> {
    [
        "fixture_payloads_not_electron_builds",
        "appimage_lane_uses_shell_fixture_not_squashfs_appimage",
        "no_rpm_or_tar_gz_lane",
        "no_apparmor_profile_or_user_namespace_enabled_launch",
        "no_electron_updater_network_download_or_quit_and_install",
        "no_rollout_admission_or_beta_channel",
        "no_desktop_entry_mime_or_icon_registration",
        "no_signing_or_repository_metadata",
        "rollback_is_pilot_semantics_baseline_has_no_rollback",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[must_use]
pub fn baseline_fixture_digests() -> BTreeMap<String, String> {
    [
        ("launcher.sh", BASELINE_LAUNCHER),
        ("after-install.tpl", BASELINE_AFTER_INSTALL),
        ("after-remove.tpl", BASELINE_AFTER_REMOVE),
    ]
    .into_iter()
    .map(|(name, text)| {
        (
            name.to_owned(),
            format!("sha512:{}", sha512_hex(text.as_bytes())),
        )
    })
    .collect()
}

/// Update manifest in the shape electron-updater reads from `latest-linux.yml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateManifest {
    pub version: String,
    pub path: String,
    pub sha512: String,
    pub size: u64,
}

impl UpdateManifest {
    /// Describes `payload` the way the release workflow stamps a published artifact.
    #[must_use]
    pub fn describe(version: &str, payload: &[u8]) -> Self {
        Self {
            version: version.to_owned(),
            path: APPIMAGE_NAME.to_owned(),
            sha512: sha512_base64(payload),
            size: payload.len() as u64,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "version: {version}\nfiles:\n  - url: {path}\n    sha512: {sha512}\n    size: {size}\npath: {path}\nsha512: {sha512}\n",
            version = self.version,
            path = self.path,
            sha512 = self.sha512,
            size = self.size,
        )
    }

    /// Parses the top-level `version`, `path`, and `sha512` plus the first file `size`.
    ///
    /// # Errors
    ///
    /// Returns an error when a required field is missing or malformed.
    pub fn parse(text: &str) -> Result<Self, RuntimeError> {
        let field = |prefix: &str| {
            text.lines()
                .find_map(|line| line.trim_start_matches([' ', '-']).strip_prefix(prefix))
                .map(|value| value.trim().to_owned())
        };
        let top = |prefix: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(prefix))
                .map(|value| value.trim().to_owned())
        };
        let size = field("size:")
            .and_then(|value| value.parse().ok())
            .ok_or(RuntimeError::InvalidPackage("update manifest size missing"))?;
        Ok(Self {
            version: top("version:").ok_or(RuntimeError::InvalidPackage(
                "update manifest version missing",
            ))?,
            path: top("path:")
                .ok_or(RuntimeError::InvalidPackage("update manifest path missing"))?,
            sha512: top("sha512:").ok_or(RuntimeError::InvalidPackage(
                "update manifest sha512 missing",
            ))?,
            size,
        })
    }

    /// Rejects a payload whose name, size, digest, or embedded version disagrees with the manifest.
    ///
    /// # Errors
    ///
    /// Returns an error describing the first mismatch.
    pub fn verify(&self, payload: &[u8]) -> Result<(), RuntimeError> {
        if self.path != APPIMAGE_NAME {
            return Err(RuntimeError::InvalidPackage(
                "unexpected update artifact name",
            ));
        }
        if payload.len() as u64 != self.size {
            return Err(RuntimeError::InvalidPackage("update size mismatch"));
        }
        if sha512_base64(payload) != self.sha512 {
            return Err(RuntimeError::InvalidPackage("update sha512 mismatch"));
        }
        if fixture_version(payload).as_deref() != Some(self.version.as_str()) {
            return Err(RuntimeError::InvalidPackage(
                "update payload version does not match manifest",
            ));
        }
        Ok(())
    }
}

/// Builds the launched program shared by the `AppImage` and deb fixtures.
///
/// Line two carries the version marker used to read an install without executing it.
fn fixture_program(version: &str, appimage: bool) -> Result<String, RuntimeError> {
    validate_version(version)?;
    let appimage_line = if appimage {
        "printf 'appimage=%s\\n' \"${APPIMAGE:-}\"\n"
    } else {
        ""
    };
    Ok(format!(
        "#!/bin/sh\n# version={version}\nset -eu\nprintf 'paseo-version={version}\\n'\nprintf 'argv=%s\\n' \"$*\"\n{appimage_line}if [ -n \"${{PASEO_HOME:-}}\" ] && [ -f \"$PASEO_HOME/{STATE_FILE}\" ]; then\n  cat \"$PASEO_HOME/{STATE_FILE}\"\nfi\n"
    ))
}

fn fixture_version(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .nth(1)?
        .strip_prefix("# version=")
        .map(str::to_owned)
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

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), RuntimeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Writes a fixture `AppImage` and returns its bytes.
///
/// # Errors
///
/// Returns an error for an invalid version or a filesystem failure.
pub fn create_linux_appimage_fixture(path: &Path, version: &str) -> Result<Vec<u8>, RuntimeError> {
    let bytes = fixture_program(version, true)?.into_bytes();
    write_executable(path, &bytes)?;
    Ok(bytes)
}

fn file_digest(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| format!("sha512:{}", sha512_hex(&bytes)))
}

fn remove_file_if_present(path: &Path) -> Result<(), RuntimeError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn truncated(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end();
    if text.len() <= OUTPUT_LIMIT {
        return text.to_owned();
    }
    let mut end = OUTPUT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[truncated]", &text[..end])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxAppImageSnapshot {
    pub active_version: Option<String>,
    pub retained_version: Option<String>,
    pub executable_digest: Option<String>,
    pub state_digest: Option<String>,
    pub cli_link_target: Option<PathBuf>,
    pub cli_link_resolves: bool,
}

/// Stable-name `AppImage` install with an updater-style in-place replacement.
#[derive(Debug, Clone)]
pub struct LinuxAppImageRuntime {
    install_dir: PathBuf,
    home: PathBuf,
}

impl LinuxAppImageRuntime {
    #[must_use]
    pub fn new(install_dir: PathBuf, home: PathBuf) -> Self {
        Self { install_dir, home }
    }

    #[must_use]
    pub fn appimage_path(&self) -> PathBuf {
        self.install_dir.join(APPIMAGE_NAME)
    }

    #[must_use]
    pub fn cli_link_path(&self) -> PathBuf {
        self.home.join(".local/bin/paseo")
    }

    #[must_use]
    pub fn paseo_home(&self) -> PathBuf {
        self.home.join(".paseo")
    }

    #[must_use]
    pub fn user_state_path(&self) -> PathBuf {
        self.paseo_home().join(STATE_FILE)
    }

    fn previous_path(&self) -> PathBuf {
        self.install_dir.join(format!("{APPIMAGE_NAME}.previous"))
    }

    fn staging_path(&self) -> PathBuf {
        self.install_dir.join(format!(".{APPIMAGE_NAME}.staging"))
    }

    fn swap_path(&self) -> PathBuf {
        self.install_dir.join(format!(".{APPIMAGE_NAME}.swap"))
    }

    /// Installs a manifest-verified `AppImage` and links the CLI to its stable path.
    ///
    /// # Errors
    ///
    /// Returns an error if an install exists, verification fails, or filesystem work fails.
    pub fn install(
        &mut self,
        source: &Path,
        manifest: &UpdateManifest,
    ) -> Result<LinuxAppImageSnapshot, RuntimeError> {
        if self.appimage_path().exists() {
            return Err(RuntimeError::InvalidPackage("already installed"));
        }
        self.stage(source, manifest)?;
        fs::rename(self.staging_path(), self.appimage_path())?;
        self.link_cli()?;
        self.snapshot()
    }

    /// Replaces the `AppImage` at its stable path and retains the previous file for rollback.
    ///
    /// The active path never disappears: the previous file is copied aside and the staged file
    /// replaces it with one atomic rename. Copies, not hard links: a Docker Desktop bind mount
    /// served the new content through a hard-linked name, so link semantics are not portable.
    ///
    /// # Errors
    ///
    /// Returns an error if nothing is installed, verification fails, or filesystem work fails.
    /// A rejected update leaves the install and user state untouched.
    pub fn update(
        &mut self,
        source: &Path,
        manifest: &UpdateManifest,
    ) -> Result<LinuxAppImageSnapshot, RuntimeError> {
        let active = self.appimage_path();
        if !active.exists() {
            return Err(RuntimeError::InvalidPackage("not installed"));
        }
        self.stage(source, manifest)?;
        let previous = self.previous_path();
        remove_file_if_present(&previous)?;
        fs::copy(&active, &previous)?;
        if let Err(error) = fs::rename(self.staging_path(), &active) {
            remove_file_if_present(&self.staging_path())?;
            return Err(error.into());
        }
        self.snapshot()
    }

    /// Swaps the active and retained `AppImage` files without changing the stable path.
    ///
    /// # Errors
    ///
    /// Returns an error if no retained file exists or filesystem work fails.
    pub fn rollback(&mut self) -> Result<LinuxAppImageSnapshot, RuntimeError> {
        let active = self.appimage_path();
        let previous = self.previous_path();
        if !active.exists() || !previous.exists() {
            return Err(RuntimeError::InvalidPackage("rollback unavailable"));
        }
        let swap = self.swap_path();
        remove_file_if_present(&swap)?;
        fs::copy(&active, &swap)?;
        fs::rename(&previous, &active)?;
        fs::rename(&swap, &previous)?;
        self.snapshot()
    }

    /// Removes the `AppImage` files. The CLI link is user-owned and stays, as in the baseline,
    /// where deleting the file is the only uninstall and leaves the link dangling.
    ///
    /// # Errors
    ///
    /// Returns an error if nothing is installed or filesystem work fails.
    pub fn uninstall(&mut self, retain_state: bool) -> Result<LinuxAppImageSnapshot, RuntimeError> {
        if !self.appimage_path().exists() {
            return Err(RuntimeError::InvalidPackage("not installed"));
        }
        remove_file_if_present(&self.appimage_path())?;
        remove_file_if_present(&self.previous_path())?;
        if !retain_state {
            match fs::remove_dir_all(self.paseo_home()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.snapshot()
    }

    /// Runs the `AppImage` through the CLI link with `APPIMAGE` and `PASEO_HOME` set.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot launch or exceeds its deadline.
    pub fn launch(&self) -> Result<ProcessResult, RuntimeError> {
        Ok(ProcessCommand::new("env")
            .arg(format!("APPIMAGE={}", self.appimage_path().display()))
            .arg(format!("PASEO_HOME={}", self.paseo_home().display()))
            .arg(self.cli_link_path())
            .arg("launch")
            .run()?)
    }

    /// Writes disposable user state under `PASEO_HOME`, outside the install.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be written atomically.
    pub fn write_user_state(&self, state: &[u8]) -> Result<(), RuntimeError> {
        fs::create_dir_all(self.paseo_home())?;
        let target = self.user_state_path();
        let temporary = target.with_extension("tmp");
        fs::write(&temporary, state)?;
        fs::rename(temporary, target)?;
        Ok(())
    }

    /// Reads the current install, retained file, state, and CLI link.
    ///
    /// # Errors
    ///
    /// Returns an error if an installed file cannot be read.
    pub fn snapshot(&self) -> Result<LinuxAppImageSnapshot, RuntimeError> {
        let read_version = |path: PathBuf| -> Result<Option<String>, RuntimeError> {
            match fs::read(path) {
                Ok(bytes) => Ok(fixture_version(&bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            }
        };
        let cli = self.cli_link_path();
        Ok(LinuxAppImageSnapshot {
            active_version: read_version(self.appimage_path())?,
            retained_version: read_version(self.previous_path())?,
            executable_digest: file_digest(&self.appimage_path()),
            state_digest: file_digest(&self.user_state_path()),
            cli_link_target: fs::read_link(&cli).ok(),
            cli_link_resolves: cli.exists(),
        })
    }

    fn stage(&self, source: &Path, manifest: &UpdateManifest) -> Result<(), RuntimeError> {
        let bytes = fs::read(source)?;
        manifest.verify(&bytes)?;
        write_executable(&self.staging_path(), &bytes)
    }

    fn link_cli(&self) -> Result<(), RuntimeError> {
        let link = self.cli_link_path();
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::symlink_metadata(&link).is_ok() {
            fs::remove_file(&link)?;
        }
        symlink(self.appimage_path(), link)?;
        Ok(())
    }
}

fn launch_text(result: &ProcessResult) -> Result<String, RuntimeError> {
    if result.exit_code != Some(0) {
        return Err(RuntimeError::ProcessFailed(format!(
            "app exited with {:?}: {}",
            result.exit_code,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(truncated(&result.stdout))
}

fn appimage_step(
    operation: &str,
    outcome: &str,
    snapshot: &LinuxAppImageSnapshot,
    launch: Option<String>,
) -> LinuxDeliveryStep {
    let mut observations = BTreeMap::from([
        (
            "cliLinkResolves".to_owned(),
            snapshot.cli_link_resolves.to_string(),
        ),
        (
            "retainedVersion".to_owned(),
            snapshot
                .retained_version
                .clone()
                .unwrap_or_else(|| "none".to_owned()),
        ),
    ]);
    if let Some(target) = &snapshot.cli_link_target {
        observations.insert("cliLinkTarget".to_owned(), target.display().to_string());
    }
    LinuxDeliveryStep {
        lane: "appimage".to_owned(),
        operation: operation.to_owned(),
        outcome: outcome.to_owned(),
        command: None,
        exit_code: None,
        output: launch,
        active_version: snapshot.active_version.clone(),
        executable_digest: snapshot.executable_digest.clone(),
        state_digest: snapshot.state_digest.clone(),
        observations,
    }
}

/// Exercises install, corrupt-update rejection, in-place update, rollback, and uninstall on real
/// files under `root`, launching the installed fixture through the CLI link each time.
///
/// # Errors
///
/// Returns an error when fixture creation, a lifecycle operation, or a launch fails.
pub fn run_linux_appimage_lifecycle(
    root: &Path,
) -> Result<(Vec<LinuxDeliveryStep>, bool), RuntimeError> {
    let packages = root.join("appimage-packages");
    let manifests =
        |version: &str, name: &str| -> Result<(PathBuf, UpdateManifest), RuntimeError> {
            let path = packages.join(name);
            let bytes = create_linux_appimage_fixture(&path, version)?;
            Ok((path, UpdateManifest::describe(version, &bytes)))
        };
    let (source_100, manifest_100) = manifests("1.0.0", "1.0.0-Paseo-x64.AppImage")?;
    let (source_110, manifest_110) = manifests("1.1.0", "1.1.0-Paseo-x64.AppImage")?;
    let (corrupt_120, manifest_120) = manifests("1.2.0", "1.2.0-Paseo-x64.AppImage")?;
    // Corrupt after the manifest was stamped, as a damaged download would be.
    let mut damaged = fs::read(&corrupt_120)?;
    damaged.extend_from_slice(b"# tampered\n");
    fs::write(&corrupt_120, damaged)?;

    let mut runtime =
        LinuxAppImageRuntime::new(root.join("appimage-install"), root.join("appimage-home"));
    runtime.write_user_state(b"agent-state-a\n")?;
    let mut steps = Vec::new();

    let installed = runtime.install(&source_100, &manifest_100)?;
    let launch = launch_text(&runtime.launch()?)?;
    steps.push(appimage_step(
        "install_launch",
        "supported",
        &installed,
        Some(launch),
    ));

    let before = runtime.snapshot()?;
    let Err(rejection) = runtime.update(&corrupt_120, &manifest_120) else {
        return Err(RuntimeError::ProcessFailed(
            "corrupt AppImage update was unexpectedly accepted".into(),
        ));
    };
    let after = runtime.snapshot()?;
    let launch = launch_text(&runtime.launch()?)?;
    steps.push(appimage_step(
        "corrupt_update_rejected",
        &rejection.to_string(),
        &after,
        Some(launch),
    ));
    let preserved = before == after;

    let upgraded = runtime.update(&source_110, &manifest_110)?;
    let launch = launch_text(&runtime.launch()?)?;
    steps.push(appimage_step(
        "valid_update_launch",
        "supported",
        &upgraded,
        Some(launch),
    ));

    let rolled_back = runtime.rollback()?;
    let launch = launch_text(&runtime.launch()?)?;
    steps.push(appimage_step(
        "rollback_launch",
        "supported",
        &rolled_back,
        Some(launch),
    ));

    let uninstalled = runtime.uninstall(true)?;
    steps.push(appimage_step(
        "uninstall_retain_state",
        "supported",
        &uninstalled,
        None,
    ));
    Ok((steps, preserved))
}

fn render_template(template: &str) -> String {
    template
        .replace("${executable}", EXECUTABLE)
        .replace("${sanitizedProductName}", EXECUTABLE)
}

/// Lays out a deb payload tree: baseline launcher, fixture program, sandbox helper, and the
/// baseline maintainer scripts with electron-builder variables substituted.
///
/// # Errors
///
/// Returns an error for an invalid version or a filesystem failure.
pub fn create_linux_deb_tree(
    tree: &Path,
    version: &str,
    architecture: &str,
) -> Result<(), RuntimeError> {
    let program = fixture_program(version, false)?;
    let prefix = tree.join(INSTALL_PREFIX.trim_start_matches('/'));
    write_executable(&prefix.join(EXECUTABLE), BASELINE_LAUNCHER.as_bytes())?;
    write_executable(
        &prefix.join(format!("{EXECUTABLE}.bin")),
        program.as_bytes(),
    )?;
    write_executable(&prefix.join("chrome-sandbox"), b"helper\n")?;
    write_executable(&prefix.join("resources/bin/paseo"), b"#!/bin/sh\nexit 0\n")?;

    let control = format!(
        "Package: {PACKAGE_NAME}\nVersion: {version}\nSection: devel\nPriority: optional\nArchitecture: {architecture}\nMaintainer: Spocky delivery pilot <delivery-pilot@invalid>\nDescription: Linux delivery pilot payload for the Paseo desktop baseline\n"
    );
    fs::create_dir_all(tree.join("DEBIAN"))?;
    fs::write(tree.join("DEBIAN/control"), control)?;
    write_executable(
        &tree.join("DEBIAN/postinst"),
        render_template(BASELINE_AFTER_INSTALL).as_bytes(),
    )?;
    write_executable(
        &tree.join("DEBIAN/postrm"),
        render_template(BASELINE_AFTER_REMOVE).as_bytes(),
    )?;
    Ok(())
}

/// Builds a root-owned deb from `tree` with the host `dpkg-deb`.
///
/// # Errors
///
/// Returns an error when `dpkg-deb` is unavailable or rejects the tree.
pub fn build_linux_deb(tree: &Path, deb: &Path) -> Result<(), RuntimeError> {
    let result = ProcessCommand::new("dpkg-deb")
        .arg("--root-owner-group")
        .arg("--build")
        .arg(tree)
        .arg(deb)
        .timeout(COMMAND_TIMEOUT)
        .run()?;
    if result.exit_code == Some(0) {
        return Ok(());
    }
    Err(RuntimeError::ProcessFailed(format!(
        "dpkg-deb failed: {}",
        String::from_utf8_lossy(&result.stderr)
    )))
}

#[derive(Debug, Clone)]
pub struct LinuxDebLifecycleConfig {
    pub root: PathBuf,
    pub user: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub disposable_root_acknowledged: bool,
}

fn execute(program: &str, args: &[String]) -> Result<(Vec<String>, ProcessResult), RuntimeError> {
    let result = ProcessCommand::new(program)
        .args(args)
        .timeout(COMMAND_TIMEOUT)
        .run()?;
    let command = std::iter::once(program.to_owned())
        .chain(args.iter().cloned())
        .collect();
    Ok((command, result))
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn require_success(label: &str, result: &ProcessResult) -> Result<(), RuntimeError> {
    if result.exit_code == Some(0) {
        return Ok(());
    }
    Err(RuntimeError::ProcessFailed(format!(
        "{label} exited with {:?}: {}",
        result.exit_code,
        String::from_utf8_lossy(&result.stderr)
    )))
}

struct DebLane<'a> {
    config: &'a LinuxDebLifecycleConfig,
    steps: Vec<LinuxDeliveryStep>,
}

impl DebLane<'_> {
    fn paseo_home(&self) -> PathBuf {
        self.config.home.join(".paseo")
    }

    fn state_path(&self) -> PathBuf {
        self.paseo_home().join(STATE_FILE)
    }

    fn installed_version() -> Option<String> {
        fs::read(Path::new(INSTALL_PREFIX).join(format!("{EXECUTABLE}.bin")))
            .ok()
            .and_then(|bytes| fixture_version(&bytes))
    }

    fn executable_digest() -> Option<String> {
        file_digest(&Path::new(INSTALL_PREFIX).join(format!("{EXECUTABLE}.bin")))
    }

    fn observations() -> Result<BTreeMap<String, String>, RuntimeError> {
        let mut map = BTreeMap::new();
        let (_, status) = execute(
            "dpkg-query",
            &strings(&["-W", "-f=${db:Status-Abbrev}|${Version}", PACKAGE_NAME]),
        )?;
        map.insert(
            "dpkgStatus".to_owned(),
            if status.exit_code == Some(0) {
                truncated(&status.stdout)
            } else {
                "not-installed".to_owned()
            },
        );
        let link = Path::new(USR_BIN_LINK);
        map.insert(
            "usrBinLink".to_owned(),
            fs::read_link(link).map_or_else(
                |_| "absent".to_owned(),
                |target| target.display().to_string(),
            ),
        );
        map.insert("usrBinResolves".to_owned(), link.exists().to_string());
        let alternative = Path::new("/etc/alternatives").join(EXECUTABLE);
        map.insert(
            "alternativesLink".to_owned(),
            fs::read_link(&alternative).map_or_else(
                |_| "absent".to_owned(),
                |target| target.display().to_string(),
            ),
        );
        map.insert(
            "installPrefixPresent".to_owned(),
            Path::new(INSTALL_PREFIX).exists().to_string(),
        );
        map.insert(
            "chromeSandbox".to_owned(),
            fs::metadata(Path::new(INSTALL_PREFIX).join("chrome-sandbox")).map_or_else(
                |_| "absent".to_owned(),
                |metadata| {
                    format!(
                        "{}:{}:{:o}",
                        metadata.uid(),
                        metadata.gid(),
                        metadata.mode() & 0o7777
                    )
                },
            ),
        );
        Ok(map)
    }

    fn launch(&self) -> Result<(Vec<String>, ProcessResult), RuntimeError> {
        execute(
            "env",
            &[
                "-i".to_owned(),
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
                format!("HOME={}", self.config.home.display()),
                format!("PASEO_HOME={}", self.paseo_home().display()),
                "/usr/bin/setpriv".to_owned(),
                format!("--reuid={}", self.config.uid),
                format!("--regid={}", self.config.gid),
                "--clear-groups".to_owned(),
                USR_BIN_LINK.to_owned(),
                "launch".to_owned(),
            ],
        )
    }

    fn record(
        &mut self,
        operation: &str,
        command: Option<(Vec<String>, &ProcessResult)>,
        outcome: &str,
        extra: &[(&str, String)],
    ) -> Result<(), RuntimeError> {
        let mut observations = Self::observations()?;
        for (key, value) in extra {
            observations.insert((*key).to_owned(), value.clone());
        }
        let (command, exit_code, output) = match command {
            Some((command, result)) => {
                let mut text = truncated(&result.stdout);
                let errors = truncated(&result.stderr);
                if !errors.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("[stderr] ");
                    text.push_str(&errors);
                }
                (Some(command), result.exit_code, Some(text))
            }
            None => (None, None, None),
        };
        self.steps.push(LinuxDeliveryStep {
            lane: "deb".to_owned(),
            operation: operation.to_owned(),
            outcome: outcome.to_owned(),
            command,
            exit_code,
            output,
            active_version: Self::installed_version(),
            executable_digest: Self::executable_digest(),
            state_digest: file_digest(&self.state_path()),
            observations,
        });
        Ok(())
    }

    fn dpkg(
        &mut self,
        operation: &str,
        flag: &str,
        target: &str,
    ) -> Result<ProcessResult, RuntimeError> {
        let (command, result) = execute("dpkg", &strings(&[flag, target]))?;
        let outcome = if result.exit_code == Some(0) {
            "supported"
        } else {
            "rejected"
        };
        self.record(operation, Some((command, &result)), outcome, &[])?;
        Ok(result)
    }

    fn launch_step(&mut self, operation: &str) -> Result<(), RuntimeError> {
        let (command, result) = self.launch()?;
        require_success("launch", &result)?;
        let sandbox = truncated(&result.stderr);
        self.record(
            operation,
            Some((command, &result)),
            "supported",
            &[("launcherSandbox", sandbox)],
        )
    }
}

fn ensure_disposable(config: &LinuxDebLifecycleConfig) -> Result<(), RuntimeError> {
    if !config.disposable_root_acknowledged {
        return Err(RuntimeError::InvalidPackage(
            "deb lane mutates /opt and /usr/bin; set SPOCKY_DELIVERY_DISPOSABLE_ROOT=1 in a disposable root",
        ));
    }
    let (_, uid) = execute("id", &strings(&["-u"]))?;
    if truncated(&uid.stdout) != "0" {
        return Err(RuntimeError::InvalidPackage("deb lane requires root"));
    }
    let (_, installed) = execute("dpkg-query", &strings(&["-W", PACKAGE_NAME]))?;
    if installed.exit_code == Some(0)
        || Path::new(INSTALL_PREFIX).exists()
        || fs::symlink_metadata(USR_BIN_LINK).is_ok()
    {
        return Err(RuntimeError::InvalidPackage(
            "refusing to run: a Paseo install already exists on this host",
        ));
    }
    Ok(())
}

fn ensure_user(config: &LinuxDebLifecycleConfig) -> Result<(), RuntimeError> {
    let (_, existing) = execute("getent", &strings(&["passwd", &config.user]))?;
    if existing.exit_code == Some(0) {
        return Ok(());
    }
    let (_, created) = execute(
        "useradd",
        &[
            "--create-home".to_owned(),
            "--home-dir".to_owned(),
            config.home.display().to_string(),
            "--uid".to_owned(),
            config.uid.to_string(),
            "--user-group".to_owned(),
            config.user.clone(),
        ],
    )?;
    require_success("useradd", &created)
}

/// Drives the real `dpkg` through install, corrupt-update rejection, upgrade, downgrade-by-reinstall
/// rollback, remove, and purge with baseline maintainer scripts, launching as an unprivileged user.
///
/// Mutates `/opt/Paseo` and `/usr/bin/Paseo`: call only inside a disposable Linux root.
///
/// # Errors
///
/// Returns an error when a precondition fails or a step that must succeed does not.
pub fn run_linux_deb_lifecycle(
    config: &LinuxDebLifecycleConfig,
) -> Result<(Vec<LinuxDeliveryStep>, bool), RuntimeError> {
    ensure_disposable(config)?;
    ensure_user(config)?;
    let (_, architecture) = execute("dpkg", &strings(&["--print-architecture"]))?;
    let architecture = truncated(&architecture.stdout);

    let packages = config.root.join("deb-packages");
    fs::create_dir_all(&packages)?;
    let mut debs = BTreeMap::new();
    for version in ["1.0.0", "1.1.0", "1.2.0"] {
        let tree = packages.join(format!("tree-{version}"));
        create_linux_deb_tree(&tree, version, &architecture)?;
        let deb = packages.join(format!("{PACKAGE_NAME}_{version}_{architecture}.deb"));
        build_linux_deb(&tree, &deb)?;
        debs.insert(version, deb);
    }
    // A truncated archive is the damage dpkg can detect: it carries no content signature.
    let corrupt = packages.join("paseo_1.2.0_corrupt.deb");
    let valid_120 = fs::read(&debs["1.2.0"])?;
    fs::write(&corrupt, &valid_120[..valid_120.len() / 2])?;

    let mut lane = DebLane {
        config,
        steps: Vec::new(),
    };
    fs::create_dir_all(lane.paseo_home())?;
    fs::write(lane.state_path(), b"agent-state-a\n")?;
    let (_, owned) = execute(
        "chown",
        &[
            "-R".to_owned(),
            format!("{}:{}", config.uid, config.gid),
            lane.paseo_home().display().to_string(),
        ],
    )?;
    require_success("chown", &owned)?;

    let deb_path = |version: &str| debs[version].display().to_string();

    let installed = lane.dpkg("install", "-i", &deb_path("1.0.0"))?;
    require_success("dpkg install", &installed)?;
    lane.launch_step("install_launch")?;

    let before = lane.steps.last().map(|step| {
        (
            step.executable_digest.clone(),
            step.state_digest.clone(),
            step.active_version.clone(),
        )
    });
    let rejected = lane.dpkg(
        "corrupt_update_rejected",
        "-i",
        &corrupt.display().to_string(),
    )?;
    if rejected.exit_code == Some(0) {
        return Err(RuntimeError::ProcessFailed(
            "truncated deb was unexpectedly accepted".into(),
        ));
    }
    let after = lane.steps.last().map(|step| {
        (
            step.executable_digest.clone(),
            step.state_digest.clone(),
            step.active_version.clone(),
        )
    });
    let preserved = before == after;
    lane.launch_step("corrupt_update_launch")?;

    let upgraded = lane.dpkg("valid_update", "-i", &deb_path("1.1.0"))?;
    require_success("dpkg upgrade", &upgraded)?;
    lane.launch_step("valid_update_launch")?;

    let downgraded = lane.dpkg("rollback_by_reinstall", "-i", &deb_path("1.0.0"))?;
    require_success("dpkg rollback", &downgraded)?;
    lane.launch_step("rollback_launch")?;

    let removed = lane.dpkg("remove_retain_state", "-r", PACKAGE_NAME)?;
    require_success("dpkg remove", &removed)?;
    let purged = lane.dpkg("purge_retain_state", "-P", PACKAGE_NAME)?;
    require_success("dpkg purge", &purged)?;
    Ok((lane.steps, preserved))
}

fn host_observations() -> BTreeMap<String, String> {
    let mut host = BTreeMap::new();
    for (key, program, args) in [
        ("kernel", "uname", &["-srm"][..]),
        ("dpkg", "dpkg", &["--version"][..]),
        ("setpriv", "setpriv", &["--version"][..]),
    ] {
        let value = execute(program, &strings(args)).map_or_else(
            |error| format!("unavailable: {error}"),
            |(_, result)| {
                truncated(&result.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned()
            },
        );
        host.insert(key.to_owned(), value);
    }
    let os_release = fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                .map(|value| value.trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| "unavailable".to_owned());
    host.insert("osRelease".to_owned(), os_release);
    host
}

/// Runs both lanes and assembles the evidence report. The deb lane needs a disposable Linux root.
///
/// # Errors
///
/// Returns an error when either lane fails.
pub fn run_linux_delivery_qualification(
    config: &LinuxDebLifecycleConfig,
) -> Result<LinuxDeliveryEvidenceReport, RuntimeError> {
    let (mut steps, appimage_preserved) = run_linux_appimage_lifecycle(&config.root)?;
    let (deb_steps, deb_preserved) = run_linux_deb_lifecycle(config)?;
    steps.extend(deb_steps);
    Ok(LinuxDeliveryEvidenceReport {
        schema_version: 1,
        contract_id: "P2-DELIVERY-01".into(),
        baseline: LINUX_DELIVERY_BASELINE.into(),
        host: host_observations(),
        baseline_fixtures: baseline_fixture_digests(),
        appimage_corrupt_update_preserved_install: appimage_preserved,
        deb_corrupt_update_preserved_install: deb_preserved,
        steps,
        limitations: linux_delivery_limitations(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_and_rejects_each_mismatch() {
        let payload = fixture_program("1.0.0", true).unwrap().into_bytes();
        let manifest = UpdateManifest::describe("1.0.0", &payload);
        assert_eq!(UpdateManifest::parse(&manifest.render()).unwrap(), manifest);
        manifest.verify(&payload).unwrap();

        let mut damaged = payload.clone();
        damaged.push(b'x');
        assert_eq!(
            manifest.verify(&damaged).unwrap_err().to_string(),
            "update size mismatch"
        );
        let mut same_size = payload.clone();
        same_size[0] = b'?';
        assert_eq!(
            manifest.verify(&same_size).unwrap_err().to_string(),
            "update sha512 mismatch"
        );
        let other_version = UpdateManifest::describe("1.0.1", &payload);
        assert_eq!(
            other_version.verify(&payload).unwrap_err().to_string(),
            "update payload version does not match manifest"
        );
        let renamed = UpdateManifest {
            path: "Paseo-1.0.0-x64.AppImage".into(),
            ..manifest
        };
        assert_eq!(
            renamed.verify(&payload).unwrap_err().to_string(),
            "unexpected update artifact name"
        );
    }
}
