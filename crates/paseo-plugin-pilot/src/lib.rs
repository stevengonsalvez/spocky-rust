//! Contract pilot for managed plugin lifecycle and restart behavior.

#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

mod protocol;
mod settings;

pub use protocol::{
    HookKind, PluginProcessMessage, PluginProcessRequest, ProcessHooks, ProcessProviderMetadata,
    ProcessUsageSourceMetadata, ProviderCatalogOptions, ProviderConnectRequest,
    RuntimeProtocolStep, decode_process_message, decode_process_request,
};
pub use settings::{
    PluginSettingsStore, SettingsDefinition, SettingsError, SettingsField, SettingsState,
    SettingsWriteState,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PluginId(String);

impl<'de> Deserialize<'de> for PluginId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

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
    Provider(String),
    UsageSource(String),
    HookEvent(String),
    HookBefore(String),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRequirements {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paseo: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: PluginId,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub requirements: PluginRequirements,
    #[serde(default)]
    pub build: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedManifest {
    pub manifest: PluginManifest,
    pub client_entry: Option<PathBuf>,
    pub server_entry: Option<PathBuf>,
}

pub fn load_manifest(directory: &Path) -> Result<LoadedManifest, PluginError> {
    let manifest: PluginManifest =
        serde_json::from_slice(&fs::read(directory.join("paseo-plugin.json"))?)?;
    if manifest
        .description
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
        || manifest
            .build
            .iter()
            .any(|command| command.is_empty() || command.iter().any(|arg| arg.trim().is_empty()))
    {
        return Err(PluginError::InvalidCandidate);
    }
    let mut manifest = manifest;
    manifest.description = manifest.description.map(|value| value.trim().to_owned());
    let client_entry = find_entry(directory, &["index.client.ts", "index.client.tsx"]);
    let server_entry = find_entry(directory, &["index.server.ts", "index.server.tsx"]);
    if client_entry.is_none() && server_entry.is_none() {
        return Err(PluginError::PluginEntryPointsMissing);
    }
    Ok(LoadedManifest {
        manifest,
        client_entry,
        server_entry,
    })
}

fn find_entry(directory: &Path, names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .map(|name| directory.join(name))
        .find(|entry| entry.is_file())
}

pub struct PluginCatalogGetRequest {
    pub request_id: String,
}

impl Serialize for PluginCatalogGetRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        json_object([
            ("type", serde_json::json!("plugin.catalog.get.request")),
            ("requestId", serde_json::json!(self.request_id)),
        ])
        .serialize(serializer)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCatalogEntry {
    pub id: PluginId,
    pub client_bundle: String,
    pub paseo_requirement: Option<String>,
}

impl Serialize for PluginCatalogEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut value = serde_json::json!({
            "id": self.id,
            "clientBundle": self.client_bundle,
        });
        if let Some(requirement) = &self.paseo_requirement {
            value["requirements"] = serde_json::json!({"paseo": requirement});
        }
        value.serialize(serializer)
    }
}

pub struct PluginCatalogGetResponse {
    pub request_id: String,
    pub plugins: Vec<PluginCatalogEntry>,
}

impl Serialize for PluginCatalogGetResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serde_json::json!({
            "type": "plugin.catalog.get.response",
            "payload": {"requestId": self.request_id, "plugins": self.plugins},
        })
        .serialize(serializer)
    }
}

pub struct PluginRpcInvokeRequest {
    pub request_id: String,
    pub plugin_id: PluginId,
    pub method: String,
    pub input: serde_json::Value,
}

impl Serialize for PluginRpcInvokeRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serde_json::json!({
            "type": "plugin.rpc.invoke.request",
            "requestId": self.request_id,
            "pluginId": self.plugin_id,
            "method": self.method,
            "input": self.input,
        })
        .serialize(serializer)
    }
}

pub struct PluginRpcInvokeResponse {
    pub request_id: String,
    pub output: serde_json::Value,
}

impl Serialize for PluginRpcInvokeResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serde_json::json!({
            "type": "plugin.rpc.invoke.response",
            "payload": {"requestId": self.request_id, "output": self.output},
        })
        .serialize(serializer)
    }
}

pub enum PluginStatusPayload {
    CatalogChanged {
        plugin_id: PluginId,
    },
    SettingsChanged {
        plugin_id: PluginId,
        settings_id: String,
    },
}

pub struct PluginStatus {
    pub payload: PluginStatusPayload,
}

impl Serialize for PluginStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let payload = match &self.payload {
            PluginStatusPayload::CatalogChanged { plugin_id } => serde_json::json!({
                "status": "plugin_catalog_changed", "pluginId": plugin_id,
            }),
            PluginStatusPayload::SettingsChanged {
                plugin_id,
                settings_id,
            } => serde_json::json!({
                "status": "plugin_settings_changed", "pluginId": plugin_id, "settingsId": settings_id,
            }),
        };
        serde_json::json!({"type": "status", "payload": payload}).serialize(serializer)
    }
}

fn json_object<const N: usize>(entries: [(&str, serde_json::Value); N]) -> serde_json::Value {
    serde_json::Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
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

#[derive(Clone, Copy)]
enum RuntimeTransport {
    Stdio,
    NodeForkIpc,
}

const NODE_FORK_BRIDGE: &str = r#"
const { fork } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const readline = require("node:readline");
const entry = process.argv[1];
const worker = path.join(os.tmpdir(), `paseo-plugin-ipc-${process.pid}.cjs`);
fs.writeFileSync(worker, fs.readFileSync(entry));
const child = fork(worker, [], {
  cwd: path.dirname(entry),
  serialization: "advanced",
  stdio: ["ignore", "ignore", "inherit", "ipc"],
});
child.on("message", (message) => {
  process.stdout.write(`${JSON.stringify(message)}\n`);
});
child.on("error", () => process.exit(1));
child.on("exit", (code, signal) => {
  try { fs.unlinkSync(worker); } catch {}
  process.exit(code ?? (signal ? 1 : 0));
});
const lines = readline.createInterface({ input: process.stdin });
lines.on("close", () => {
  if (!child.killed) child.kill("SIGKILL");
});
lines.on("line", (line) => {
  if (!child.connected) process.exit(1);
  child.send(JSON.parse(line), (error) => {
    if (error) process.exit(1);
  });
});
"#;

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
        self.load_inner(None, &[], timeout, RuntimeTransport::Stdio)
    }

    pub fn load_and_invoke(
        self,
        method: &str,
        input: serde_json::Value,
        timeout: Duration,
    ) -> Result<LoadedPlugin, PluginError> {
        if method.is_empty() {
            return Err(PluginError::RuntimeProtocol);
        }
        self.load_inner(Some((method, input)), &[], timeout, RuntimeTransport::Stdio)
    }

    pub fn load_via_node_fork_and_invoke(
        self,
        method: &str,
        input: serde_json::Value,
        timeout: Duration,
    ) -> Result<LoadedPlugin, PluginError> {
        if method.is_empty() {
            return Err(PluginError::RuntimeProtocol);
        }
        self.load_inner(
            Some((method, input)),
            &[],
            timeout,
            RuntimeTransport::NodeForkIpc,
        )
    }

    pub fn load_with_protocol_steps(
        self,
        steps: &[RuntimeProtocolStep],
        timeout: Duration,
    ) -> Result<LoadedPlugin, PluginError> {
        self.load_inner(None, steps, timeout, RuntimeTransport::Stdio)
    }

    fn load_inner(
        self,
        invocation: Option<(&str, serde_json::Value)>,
        protocol_steps: &[RuntimeProtocolStep],
        timeout: Duration,
        transport: RuntimeTransport,
    ) -> Result<LoadedPlugin, PluginError> {
        let loaded_manifest = load_manifest(&self.directory)?;
        let id = loaded_manifest.manifest.id.clone();
        let entry = loaded_manifest
            .server_entry
            .ok_or(PluginError::PluginServerEntryMissing)?;
        let bundle = fs::read_to_string(&entry)?;
        let mut command = Command::new("node");
        match transport {
            RuntimeTransport::Stdio => {
                command.args(["-e", &bundle]).stderr(Stdio::null());
            }
            RuntimeTransport::NodeForkIpc => {
                command
                    .arg("-e")
                    .arg(NODE_FORK_BRIDGE)
                    .arg(&entry)
                    .stderr(Stdio::null());
            }
        }
        let mut child = command
            .current_dir(&self.directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let result = exchange_runtime(
            &mut child,
            &RuntimeInitialize::new(
                id.as_str(),
                &bundle,
                "0.8.0-pilot",
                &self.directory.to_string_lossy(),
            ),
            invocation,
            protocol_steps,
            timeout,
        );
        if result.is_err() {
            terminate_runtime(&mut child);
        }
        let (contributions, traffic, invocation_output) = result?;
        Ok(LoadedPlugin {
            id,
            candidate: Candidate::new(self.identity, Some(self.revision), contributions.clone()),
            contributions,
            traffic,
            client_bundle: loaded_manifest
                .client_entry
                .map(fs::read_to_string)
                .transpose()?
                .unwrap_or_default(),
            invocation_output,
        })
    }
}

fn terminate_runtime(child: &mut Child) {
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Debug)]
pub struct LoadedPlugin {
    id: PluginId,
    candidate: Candidate,
    contributions: Vec<Contribution>,
    traffic: Vec<RuntimeTraffic>,
    client_bundle: String,
    invocation_output: Option<serde_json::Value>,
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
    pub fn client_bundle(&self) -> &str {
        &self.client_bundle
    }

    #[must_use]
    pub const fn invocation_output(&self) -> Option<&serde_json::Value> {
        self.invocation_output.as_ref()
    }

    #[must_use]
    pub fn into_candidate(self) -> Candidate {
        self.candidate
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyMessage {
    r#type: String,
    methods: Vec<String>,
    providers: Vec<ProviderMetadata>,
    #[serde(default, rename = "usageSources")]
    usage_sources: Vec<UsageSourceMetadata>,
    #[serde(default, rename = "hooks")]
    hooks: HooksMetadata,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderMetadata {
    id: String,
    #[serde(rename = "label")]
    _label: String,
    #[serde(default)]
    _description: Option<String>,
    #[serde(default, rename = "iconPath")]
    _icon_path: Option<String>,
    #[serde(default, rename = "hasCatalogCacheKey")]
    _has_catalog_cache_key: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageSourceMetadata {
    id: String,
    #[serde(rename = "label")]
    _label: String,
    #[serde(default)]
    _icon: Option<String>,
    #[serde(rename = "discover")]
    _discover: bool,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HooksMetadata {
    #[serde(default, rename = "events")]
    events: Vec<String>,
    #[serde(default, rename = "before")]
    before: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeInitialize<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    plugin_id: &'a str,
    bundle: &'a str,
    app_version: &'a str,
    plugin_directory: &'a str,
}

impl<'a> RuntimeInitialize<'a> {
    fn new(
        plugin_id: &'a str,
        bundle: &'a str,
        app_version: &'a str,
        plugin_directory: &'a str,
    ) -> Self {
        Self {
            kind: "initialize",
            plugin_id,
            bundle,
            app_version,
            plugin_directory,
        }
    }
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
    if checkout.exists() {
        return Err(PluginError::InvalidCandidate);
    }
    let staging = staging_path(&checkout)?;
    remove_stale_staging(&staging)?;
    fs::create_dir_all(checkout.parent().unwrap_or_else(|| Path::new(".")))?;
    let relative_plugin_path = if plugin_path == "." {
        PathBuf::new()
    } else {
        safe_relative_path(plugin_path)?
    };
    let acquired = (|| {
        run_bounded(
            Command::new("git")
                .args(["clone", "--no-checkout", "--", remote])
                .arg(&staging),
            timeout,
        )?;
        run_bounded(
            Command::new("git")
                .args(["checkout", "--detach", reviewed_revision])
                .current_dir(&staging),
            timeout,
        )?;
        let output = run_bounded(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&staging),
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
        let staged_directory = staging.join(&relative_plugin_path);
        assert_contained_directory(&staging, &staged_directory)?;
        fs::rename(&staging, &checkout)?;
        Ok(AcquiredPlugin {
            directory: checkout.join(&relative_plugin_path),
            identity: PluginSourceIdentity::Git {
                remote: remote.to_owned(),
                plugin_path: plugin_path.to_owned(),
            },
            revision: reviewed_revision.to_owned(),
        })
    })();
    if acquired.is_err() {
        let _ = remove_stale_staging(&staging);
    }
    acquired
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
    if installation.exists() {
        return Err(PluginError::InvalidCandidate);
    }
    let staging = staging_path(&installation)?;
    remove_stale_staging(&staging)?;
    fs::create_dir_all(installation.parent().unwrap_or_else(|| Path::new(".")))?;
    fs::create_dir_all(&staging)?;
    let acquired = (|| {
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
                .arg(&staging)
                .arg(&archive),
            timeout,
        )?;
        let mut package_root = staging.join("node_modules");
        for segment in package_segments {
            package_root.push(segment);
        }
        let package: NpmPackageManifest =
            serde_json::from_slice(&fs::read(package_root.join("package.json"))?)?;
        if package.name != package_name || package.version.is_empty() {
            return Err(PluginError::InvalidCandidate);
        }
        let staged_directory = package_root.join(&relative_plugin_path);
        assert_contained_directory(&package_root, &staged_directory)?;
        fs::rename(&staging, &installation)?;
        let mut final_package_root = installation.join("node_modules");
        for segment in npm_package_segments(package_name)? {
            final_package_root.push(segment);
        }
        Ok(AcquiredPlugin {
            directory: final_package_root.join(&relative_plugin_path),
            identity: PluginSourceIdentity::Npm {
                package_name: package_name.to_owned(),
                plugin_path: plugin_path.to_owned(),
            },
            revision: package.version,
        })
    })();
    if acquired.is_err() {
        let _ = remove_stale_staging(&staging);
    }
    acquired
}

fn staging_path(destination: &Path) -> Result<PathBuf, PluginError> {
    let file_name = destination
        .file_name()
        .ok_or(PluginError::InvalidCandidate)?;
    let mut staging_name = OsString::from(".");
    staging_name.push(file_name);
    staging_name.push(".staging");
    Ok(destination
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(staging_name))
}

fn remove_stale_staging(staging: &Path) -> Result<(), PluginError> {
    match fs::symlink_metadata(staging) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(staging)?;
        }
        Ok(_) => fs::remove_dir_all(staging)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
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
    if path.starts_with(['/', '\\'])
        || path.as_bytes().get(1) == Some(&b':')
        || path
            .split(['/', '\\'])
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(PluginError::InvalidCandidate);
    }
    let normalized = path.replace('\\', "/");
    let path = Path::new(&normalized);
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

fn assert_contained_directory(root: &Path, directory: &Path) -> Result<(), PluginError> {
    let root = root.canonicalize()?;
    let directory = directory.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PluginError::PluginNotFound
        } else {
            PluginError::Io(error)
        }
    })?;
    if !directory.is_dir() || !directory.starts_with(root) {
        return Err(PluginError::InvalidCandidate);
    }
    Ok(())
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
    initialize: &RuntimeInitialize<'_>,
    invocation: Option<(&str, serde_json::Value)>,
    protocol_steps: &[RuntimeProtocolStep],
    timeout: Duration,
) -> Result<RuntimeExchange, PluginError> {
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
    let initialize = serde_json::to_string(initialize)?;
    writeln!(stdin, "{initialize}")?;
    stdin.flush()?;
    let ready_line = receiver
        .recv_timeout(timeout)
        .map_err(|_| PluginError::RuntimeTimedOut)??;
    let ready: ReadyMessage = serde_json::from_str(&ready_line)?;
    if ready.r#type != "ready" {
        return Err(PluginError::RuntimeProtocol);
    }
    let methods = ready.methods;
    let mut contributions = methods
        .iter()
        .cloned()
        .map(Contribution::Rpc)
        .collect::<Vec<_>>();
    contributions.extend(
        ready
            .providers
            .into_iter()
            .map(|provider| Contribution::Provider(provider.id)),
    );
    contributions.extend(
        ready
            .usage_sources
            .into_iter()
            .map(|source| Contribution::UsageSource(source.id)),
    );
    contributions.extend(ready.hooks.events.into_iter().map(Contribution::HookEvent));
    contributions.extend(ready.hooks.before.into_iter().map(Contribution::HookBefore));
    let mut traffic = vec![
        RuntimeTraffic {
            direction: "host_to_plugin",
            message: initialize.clone(),
        },
        RuntimeTraffic {
            direction: "plugin_to_host",
            message: ready_line,
        },
    ];
    let invocation_output = perform_invocation(
        stdin,
        &receiver,
        &methods,
        invocation,
        timeout,
        &mut traffic,
    )?;
    perform_protocol_steps(stdin, &receiver, protocol_steps, timeout, &mut traffic)?;
    let shutdown = r#"{"type":"shutdown"}"#;
    writeln!(stdin, "{shutdown}")?;
    stdin.flush()?;
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
    traffic.push(RuntimeTraffic {
        direction: "host_to_plugin",
        message: shutdown.to_owned(),
    });
    Ok((contributions, traffic, invocation_output))
}

fn perform_protocol_steps(
    stdin: &mut impl Write,
    receiver: &RuntimeLineReceiver,
    steps: &[RuntimeProtocolStep],
    timeout: Duration,
    traffic: &mut Vec<RuntimeTraffic>,
) -> Result<(), PluginError> {
    for step in steps {
        match step {
            RuntimeProtocolStep::Send(request) => {
                if matches!(
                    request,
                    PluginProcessRequest::Initialize { .. } | PluginProcessRequest::Shutdown {}
                ) {
                    return Err(PluginError::RuntimeProtocol);
                }
                let encoded = serde_json::to_string(request)?;
                decode_process_request(&encoded).map_err(|_| PluginError::RuntimeProtocol)?;
                writeln!(stdin, "{encoded}")?;
                stdin.flush()?;
                traffic.push(RuntimeTraffic {
                    direction: "host_to_plugin",
                    message: encoded,
                });
            }
            RuntimeProtocolStep::Receive(expected) => {
                let encoded = receiver
                    .recv_timeout(timeout)
                    .map_err(|_| PluginError::RuntimeTimedOut)??;
                let actual =
                    decode_process_message(&encoded).map_err(|_| PluginError::RuntimeProtocol)?;
                traffic.push(RuntimeTraffic {
                    direction: "plugin_to_host",
                    message: encoded,
                });
                if &actual != expected {
                    return match actual {
                        PluginProcessMessage::Fatal { error } => {
                            Err(PluginError::RuntimeFatal(error))
                        }
                        _ => Err(PluginError::RuntimeProtocol),
                    };
                }
            }
        }
    }
    Ok(())
}

type RuntimeLineReceiver = mpsc::Receiver<Result<String, std::io::Error>>;
type RuntimeExchange = (
    Vec<Contribution>,
    Vec<RuntimeTraffic>,
    Option<serde_json::Value>,
);

fn perform_invocation(
    stdin: &mut impl Write,
    receiver: &RuntimeLineReceiver,
    methods: &[String],
    invocation: Option<(&str, serde_json::Value)>,
    timeout: Duration,
    traffic: &mut Vec<RuntimeTraffic>,
) -> Result<Option<serde_json::Value>, PluginError> {
    let Some((method, input)) = invocation else {
        return Ok(None);
    };
    if !methods.iter().any(|candidate| candidate == method) {
        return Err(PluginError::RuntimeProtocol);
    }
    let request = serde_json::to_string(&serde_json::json!({
        "type": "invoke",
        "requestId": "pilot-1",
        "method": method,
        "input": input,
    }))?;
    writeln!(stdin, "{request}")?;
    stdin.flush()?;
    traffic.push(RuntimeTraffic {
        direction: "host_to_plugin",
        message: request,
    });
    let response_line = receiver
        .recv_timeout(timeout)
        .map_err(|_| PluginError::RuntimeTimedOut)??;
    match decode_process_message(&response_line).map_err(|_| PluginError::RuntimeProtocol)? {
        PluginProcessMessage::Result { request_id, output } if request_id == "pilot-1" => {
            traffic.push(RuntimeTraffic {
                direction: "plugin_to_host",
                message: response_line,
            });
            Ok(Some(output))
        }
        PluginProcessMessage::Error { request_id, error } if request_id == "pilot-1" => {
            Err(PluginError::RuntimeRequest(error))
        }
        PluginProcessMessage::Fatal { error } => Err(PluginError::RuntimeFatal(error)),
        _ => Err(PluginError::RuntimeProtocol),
    }
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
    PluginEntryPointsMissing,
    PluginServerEntryMissing,
    LocalDirectoryCannotUpdate,
    ReviewedStateChanged,
    CandidateDoesNotMatchReview,
    ActivationFailed,
    InvalidCommandOutput,
    CommandFailed(String),
    CommandTimedOut,
    RuntimeProtocol,
    RuntimeTimedOut,
    RuntimeRequest(String),
    RuntimeFatal(String),
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
