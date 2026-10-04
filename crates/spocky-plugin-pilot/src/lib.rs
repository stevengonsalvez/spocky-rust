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

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use spocky_contracts::text::js_trim;

mod client_runtime;
mod daemon_rpc;
pub mod managed_git;
mod protocol;
mod settings;

pub use client_runtime::{
    ClientContribution, ClientRuntimeSession, CompiledPluginClient, compile_plugin_client,
};

pub use daemon_rpc::{
    CatalogPayload, InspectPayload, ListPayload, LogsPayload, PluginDaemonRequest,
    PluginDaemonResponse, PluginInstallationWire, PluginLegacySource, PluginListItem,
    PluginLogEntry, PluginLogStream, PluginNotification, PluginNotificationPayload,
    PluginNpmInstallation, PluginPayload, PluginRpcError, PluginRpcErrorPayload,
    PluginRuntimeStatus, PluginSourceIdentityWire, PluginSourceStatusItem, PluginSourceUpdateItem,
    PluginUpdateExpected, PluginUpdatePreview, PluginUpdatePreviewOutcome, PluginUpdateProposal,
    PluginUpdateResult, PluginUpdateResultOutcome, PluginUpdateSelection, PluginUpdateTarget,
    RequestPayload, RpcInvokePayload, SourceStatusPayload, SourceUpdatePayload, UpdateApplyPayload,
    UpdatePreviewPayload,
};

pub use protocol::{
    HookKind, PluginProcessMessage, PluginProcessRequest, ProcessHooks, ProcessProviderMetadata,
    ProcessUsageSourceMetadata, ProviderCatalogOptions, ProviderConnectRequest, ProviderEvent,
    ProviderInput, RuntimeProtocolStep, decode_process_message, decode_process_request,
};
pub use settings::{
    PluginSettingsStore, SettingsDefinition, SettingsError, SettingsField, SettingsSchema,
    SettingsState, SettingsWriteState,
};

const fn node_program() -> &'static str {
    if cfg!(windows) { "node.exe" } else { "node" }
}

const fn npm_program() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

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
        .is_some_and(|value| js_trim(value).is_empty())
        || manifest
            .build
            .iter()
            .any(|command| command.is_empty() || command.iter().any(|arg| js_trim(arg).is_empty()))
    {
        return Err(PluginError::InvalidCandidate);
    }
    let mut manifest = manifest;
    manifest.description = manifest.description.map(|value| js_trim(&value).to_owned());
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

impl<'de> Deserialize<'de> for PluginCatalogEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireEntry {
            id: PluginId,
            client_bundle: String,
            #[serde(
                default,
                deserialize_with = "deserialize_optional_catalog_requirements"
            )]
            requirements: Option<PluginRequirements>,
        }

        fn deserialize_optional_catalog_requirements<'de, D>(
            deserializer: D,
        ) -> Result<Option<PluginRequirements>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            PluginRequirements::deserialize(deserializer).map(Some)
        }

        let wire = WireEntry::deserialize(deserializer)?;
        Ok(Self {
            id: wire.id,
            client_bundle: wire.client_bundle,
            paseo_requirement: wire.requirements.and_then(|value| value.paseo),
        })
    }
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
const worker = path.join(os.tmpdir(), `spocky-plugin-ipc-${process.pid}.cjs`);
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

const SELECTED_SERVER_WORKER: &str = r#"
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const handlers = new Map();
const providers = new Map();
const usageSources = new Map();
const hookHandlers = { event: new Map(), before: new Map() };
const hookRequests = new Map();
const settings = new Map();
const connections = new Map();
const pendingConnections = new Map();
const paseoRequests = new Map();
let paseoSequence = 0;
let settingsDirectory;
let cleanup;
let stopping = false;

const lifecycleEventNames = new Set([
  "agent.created", "agent.turn_started", "agent.turn_ended",
  "agent.permission_requested", "agent.permission_resolved", "agent.archived",
  "workspace.created", "workspace.archived",
]);
const beforeHookNames = new Set(["agent.create", "agent.session_open", "workspace.create"]);

function send(message) { process.send(message); }
function describe(error) { return error instanceof Error ? error.message : String(error); }
function jsonValue(value) {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new Error("Plugin value is not JSON-serializable");
  return JSON.parse(encoded);
}
function validId(value, label) {
  const id = String(value ?? "").trim();
  if (!/^[a-z][a-z0-9._-]*$/.test(id)) throw new Error(`Invalid ${label}: ${value}`);
  return id;
}
function defineRpc(definition) {
  return { ...definition, name: validId(definition.name, "plugin RPC method") };
}
function defineSettings(definition) {
  const id = validId(definition && definition.id, "settings ID");
  if (definition.scope !== "host" || !Number.isSafeInteger(definition.version) || definition.version < 1 ||
      !definition.schema || typeof definition.schema.parseAsync !== "function") {
    throw new Error(`Invalid settings definition: ${id}`);
  }
  return { ...definition, id };
}
function runtimeRequire(name) {
  if (name === "@getpaseo/plugin") return { defineRpc, defineSettings };
  if (name === "@getpaseo/plugin/server" || name === "@getpaseo/plugin/server/provider" ||
      name === "@getpaseo/plugin/server/acp" || name === "@getpaseo/plugin/server/usage") return {};
  throw new Error(`Module \"${name}\" is not available in selected plugin worker`);
}
function revision(raw) { return crypto.createHash("sha256").update(raw).digest("hex"); }
async function stored(definition) {
  if (!settingsDirectory) return { raw: null, revision: "missing" };
  try {
    const raw = await fs.promises.readFile(path.join(settingsDirectory, `${definition.id}.json`), "utf8");
    return { raw, revision: revision(raw) };
  } catch (error) {
    if (error && error.code === "ENOENT") return { raw: null, revision: "missing" };
    throw error;
  }
}
function settingsEnvelope(raw) {
  const envelope = JSON.parse(raw);
  if (!envelope || typeof envelope !== "object" || !Number.isSafeInteger(envelope.version) ||
      envelope.version < 1 || !("values" in envelope)) {
    throw new Error("Invalid settings envelope");
  }
  jsonValue(envelope.values);
  return envelope;
}
async function readSettings(current) {
  const saved = await stored(current.definition);
  try {
    const envelope = saved.raw === null ? null : settingsEnvelope(saved.raw);
    let values = envelope?.values ?? {};
    if (envelope && envelope.version !== current.definition.version) {
      if (envelope.version > current.definition.version) {
        throw new Error("Settings were saved by a newer plugin version");
      }
      if (typeof current.definition.migrate !== "function") {
        throw new Error(`Settings version ${envelope.version} requires a migration`);
      }
      values = await current.definition.migrate(values, envelope.version);
    }
    values = jsonValue(await current.definition.schema.parseAsync(values));
    const migrated = envelope !== null && envelope.version !== current.definition.version;
    const nextRevision = migrated ? await persistSettings(current, values) : saved.revision;
    const state = { status: "ready", revision: nextRevision, values };
    if (migrated) {
      for (const listener of current.listeners) await listener(structuredClone(state));
      send({ type: "settings.changed", settingsId: current.definition.id });
    }
    return state;
  } catch (error) {
    return { status: "invalid", revision: saved.revision, error: describe(error) };
  }
}
async function persistSettings(current, values) {
  if (!settingsDirectory) throw new Error("Plugin settings storage is unavailable");
  await fs.promises.mkdir(settingsDirectory, { recursive: true });
  const target = path.join(settingsDirectory, `${current.definition.id}.json`);
  const temporary = `${target}.${crypto.randomUUID()}.tmp`;
  const raw = JSON.stringify({ version: current.definition.version, values });
  try {
    await fs.promises.writeFile(temporary, raw, { mode: 0o600 });
    await fs.promises.rename(temporary, target);
  } finally {
    await fs.promises.rm(temporary, { force: true });
  }
  return revision(raw);
}
async function writeSettings(current, input, reset) {
  const saved = await stored(current.definition);
  if (saved.revision !== input.revision) {
    return { status: "conflict", error: "Settings changed on another client. Reload before saving again." };
  }
  try {
    if (!reset && saved.raw !== null) {
      const envelope = settingsEnvelope(saved.raw);
      if (envelope.version !== current.definition.version) {
        throw new Error("Reload or reset settings before saving a different schema version");
      }
    }
    const values = jsonValue(await current.definition.schema.parseAsync(reset ? {} : input.values));
    const nextRevision = await persistSettings(current, values);
    const state = { status: "ready", revision: nextRevision, values };
    for (const listener of current.listeners) await listener(structuredClone(state));
    send({ type: "settings.changed", settingsId: current.definition.id });
    return { status: "saved", revision: nextRevision, values };
  } catch (error) {
    return { status: "invalid", error: describe(error) };
  }
}
function registerHandler(contract, handler) {
  const method = validId(contract && contract.name, "plugin RPC method");
  if (handlers.has(method)) throw new Error(`Duplicate plugin RPC method: ${method}`);
  if (typeof handler !== "function") throw new Error(`Plugin RPC ${method} must provide a handler`);
  handlers.set(method, { contract, handler });
}
function registerSettings(definition) {
  if (!settingsDirectory) throw new Error("Plugin settings storage is unavailable");
  definition = defineSettings(definition);
  if (settings.has(definition.id)) throw new Error(`Duplicate settings: ${definition.id}`);
  const current = { definition, listeners: new Set() };
  settings.set(definition.id, current);
  registerHandler({ name: `settings.${definition.id}.read` }, () => readSettings(current));
  registerHandler({ name: `settings.${definition.id}.write` }, (input) => writeSettings(current, input, false));
  registerHandler({ name: `settings.${definition.id}.reset` }, (input) => writeSettings(current, input, true));
  return {
    read: () => readSettings(current),
    subscribe(listener) {
      current.listeners.add(listener);
      return () => current.listeners.delete(listener);
    },
  };
}
function addHook(kind, name, handler) {
  if (typeof handler !== "function") throw new Error(`Invalid ${kind} hook: ${name}`);
  const normalized = String(name);
  const supported = kind === "event" ? lifecycleEventNames : beforeHookNames;
  if (!supported.has(normalized)) {
    throw new Error(kind === "event" ? `Unknown lifecycle event: ${normalized}` : `Unknown before hook: ${normalized}`);
  }
  const entries = hookHandlers[kind].get(normalized) ?? new Set();
  entries.add(handler);
  hookHandlers[kind].set(normalized, entries);
  hooksChanged();
  return () => {
    if (!entries.delete(handler)) return;
    if (entries.size === 0) hookHandlers[kind].delete(normalized);
    hooksChanged();
  };
}
function hookCatalog() {
  return { events: [...hookHandlers.event.keys()], before: [...hookHandlers.before.keys()] };
}
function hooksChanged() {
  send({ type: "hooks.changed", hooks: hookCatalog() });
}
function assertObject(value, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`Invalid ${name} hook request`);
  return value;
}
function validateBeforeRequest(name, value) {
  const request = assertObject(value, name);
  if (name === "workspace.create") {
    const source = assertObject(request.source, name);
    if (source.kind === "directory") {
      if (typeof source.path !== "string") throw new Error("Invalid workspace.create hook request");
    } else if (source.kind !== "worktree") {
      throw new Error("Invalid workspace.create hook request");
    }
  } else if (name === "agent.session_open") {
    if (typeof request.agentId !== "string" ||
        !(request.workspaceId === null || typeof request.workspaceId === "string") ||
        typeof request.provider !== "string" || typeof request.cwd !== "string" ||
        !["create", "resume", "refresh", "import"].includes(request.reason) ||
        !["interactive", "history"].includes(request.purpose) ||
        !request.env || typeof request.env !== "object" || Array.isArray(request.env) ||
        Object.values(request.env).some((item) => typeof item !== "string")) {
      throw new Error("Invalid agent.session_open hook request");
    }
  } else if (name === "agent.create") {
    const config = assertObject(request.config, name);
    if (typeof config.cwd !== "string") throw new Error("Invalid agent.create hook request");
  }
  return request;
}
function validateBeforeResult(name, previous, output) {
  const next = validateBeforeRequest(name, output);
  if (name === "agent.session_open" &&
      ["agentId", "workspaceId", "provider", "cwd", "reason", "purpose"].some((key) => previous[key] !== next[key])) {
    throw new Error("agent.session_open hooks can only change env");
  }
  if (name === "agent.create" && previous.config.cwd !== next.config.cwd) {
    throw new Error("agent.create hooks cannot change the workspace directory");
  }
  return next;
}
function paseoRequest(method, input) {
  const requestId = `paseo-${++paseoSequence}`;
  send({ type: "paseo_frame", data: JSON.stringify({ type: "request", requestId, method, input }), isBinary: false });
  return new Promise((resolve, reject) => paseoRequests.set(requestId, { resolve, reject }));
}
const paseo = {
  request: paseoRequest,
  sessions: { list: (input) => paseoRequest("sessions.list", input) },
  agents: { list: (input) => paseoRequest("agents.list", input) },
};
function serverContext() {
  return {
    handle: registerHandler,
    registerProvider(provider) {
      const id = validId(provider && provider.id, "plugin provider ID");
      if (!String(provider.label ?? "").trim() || typeof provider.connect !== "function" ||
          (provider.getCatalogCacheKey !== undefined && typeof provider.getCatalogCacheKey !== "function")) {
        throw new Error(`Invalid plugin provider: ${id}`);
      }
      if (providers.has(id)) throw new Error(`Duplicate plugin provider ID: ${id}`);
      providers.set(id, provider);
    },
    registerUsageSource(source) {
      const id = validId(source && source.id, "usage source ID");
      if (!String(source.label ?? "").trim() || typeof source.fetch !== "function" ||
          !source.input || typeof source.input.parseAsync !== "function") {
        throw new Error(`Invalid usage source: ${id}`);
      }
      if (usageSources.has(id)) throw new Error(`Duplicate usage source: ${id}`);
      usageSources.set(id, source);
    },
    registerSettings,
    on: (name, handler) => addHook("event", name, handler),
    before: (name, handler) => addHook("before", name, handler),
  };
}

async function initialize(message) {
  settingsDirectory = message.settingsDirectory;
  const evaluate = globalThis.eval;
  const factory = evaluate(message.bundle);
  if (typeof factory !== "function") throw new Error("Plugin server bundle is not executable");
  const exports = factory(runtimeRequire);
  const contribute = exports && typeof exports === "object" ? exports.default : undefined;
  if (typeof contribute !== "function") throw new Error("Plugin server bundle must default export a function");
  cleanup = contribute(serverContext());
  if (typeof cleanup !== "function") throw new Error("Plugin contribution must return a cleanup function");
  send({
    type: "ready",
    methods: [...handlers.keys()].sort(),
    hooks: hookCatalog(),
    providers: [...providers.entries()].sort().map(([id, value]) => ({
      id,
      label: value.label,
      description: value.description,
      iconPath: value.icon,
      hasCatalogCacheKey: value.getCatalogCacheKey === undefined ? undefined : true,
    })),
    usageSources: [...usageSources.entries()].sort().map(([id, value]) => ({ id, label: value.label, discover: typeof value.discover === "function" })),
  });
}

async function invokeHook(message) {
  const controller = new AbortController();
  hookRequests.set(message.requestId, controller);
  try {
    if (message.kind === "before" && !beforeHookNames.has(message.name)) {
      throw new Error(`Unknown before hook: ${message.name}`);
    }
    const registered = [...(hookHandlers[message.kind]?.get(message.name) ?? [])];
    if (message.kind === "before") {
      let request = validateBeforeRequest(message.name, message.input);
      for (const handler of registered) {
        controller.signal.throwIfAborted();
        const current = await handler(
          { request: structuredClone(request) },
          { paseo, signal: controller.signal },
        );
        if (current !== undefined) request = validateBeforeResult(message.name, request, current);
      }
      send({ type: "result", requestId: message.requestId, output: jsonValue(request) });
      return;
    }
    for (const handler of registered) {
      controller.signal.throwIfAborted();
      try {
        await handler(structuredClone(message.input), { paseo, signal: controller.signal });
      } catch (error) {
        console.error(`Lifecycle hook ${message.name} failed`, error);
      }
    }
    send({ type: "result", requestId: message.requestId, output: null });
  } catch (error) {
    send({ type: "error", requestId: message.requestId, error: describe(error) });
  } finally {
    hookRequests.delete(message.requestId);
  }
}

async function connectProvider(message) {
  if (stopping) throw new Error("Plugin is stopping");
  const provider = providers.get(message.providerId);
  if (!provider) throw new Error(`Unknown plugin provider: ${message.providerId}`);
  if (connections.has(message.connectionId) || pendingConnections.has(message.connectionId)) {
    throw new Error(`Duplicate provider connection: ${message.connectionId}`);
  }
  const pending = { tombstoned: false };
  pendingConnections.set(message.connectionId, pending);
  let connection;
  try {
    connection = await provider.connect(message.request);
  } catch (error) {
    pendingConnections.delete(message.connectionId);
    if (pending.tombstoned || stopping) return;
    throw error;
  }
  pendingConnections.delete(message.connectionId);
  if (pending.tombstoned || stopping) {
    await connection.close().catch(() => undefined);
    return;
  }
  let unsubscribe = () => {};
  unsubscribe = connection.onEvent((event) => {
    try {
      send({ type: "provider.event", connectionId: message.connectionId, event: jsonValue(event) });
    } catch (error) {
      unsubscribe();
      void connection.close();
      connections.delete(message.connectionId);
      send({ type: "provider.closed", connectionId: message.connectionId, error: describe(error) });
    }
  });
  connections.set(message.connectionId, { connection, unsubscribe });
  send({
    type: "provider.connected",
    connectionId: message.connectionId,
    version: connection.version,
    capabilities: connection.capabilities,
  });
}

async function closeProvider(connectionId) {
  const current = connections.get(connectionId);
  if (!current) return;
  if (current.closing) return current.closing;
  const closing = (async () => {
    current.unsubscribe();
    try {
      await current.connection.close();
      send({ type: "provider.closed", connectionId });
    } catch (error) {
      send({ type: "provider.closed", connectionId, error: describe(error) });
    } finally {
      connections.delete(connectionId);
    }
  })();
  current.closing = closing;
  return closing;
}

process.on("message", (message) => {
  void (async () => {
    if (message.type === "initialize") return initialize(message);
    if (message.type === "provider.catalog_key") {
      const provider = providers.get(message.providerId);
      if (!provider) throw new Error(`Unknown provider: ${message.providerId}`);
      const output = await provider.getCatalogCacheKey?.(message.options);
      if (output !== undefined && typeof output !== "string") throw new Error("Invalid catalogue key");
      send({ type: "result", requestId: message.requestId, output });
      return;
    }
    if (message.type === "invoke") {
      const registered = handlers.get(message.method);
      if (!registered) throw new Error(`Unknown RPC method: ${message.method}`);
      const input = registered.contract.input?.parseAsync ?
        await registered.contract.input.parseAsync(message.input) : message.input;
      const output = await registered.handler(input, { paseo });
      send({ type: "result", requestId: message.requestId, output });
      return;
    }
    if (message.type === "usage.identify" || message.type === "usage.fetch" || message.type === "usage.discover") {
      const source = usageSources.get(message.sourceId);
      if (!source) throw new Error(`Unknown usage source: ${message.sourceId}`);
      const output = message.type === "usage.discover" ? await source.discover?.() ?? [] :
        message.type === "usage.identify" ? await source.identify(await source.input.parseAsync(message.input)) :
        await source.fetch(await source.input.parseAsync(message.input));
      send({ type: "result", requestId: message.requestId, output: jsonValue(output) });
      return;
    }
    if (message.type === "provider.connect") {
      try {
        await connectProvider(message);
      } catch (error) {
        if (stopping) return;
        send({ type: "provider.connect_failed", connectionId: message.connectionId, error: describe(error) });
      }
      return;
    }
    if (message.type === "provider.send") {
      const current = connections.get(message.connectionId);
      if (!current) throw new Error(`Unknown provider connection: ${message.connectionId}`);
      if (current.closing) throw new Error("Provider connection is closing");
      try {
        await current.connection.send(message.input);
        send({ type: "provider.accepted", connectionId: message.connectionId, acceptanceId: message.acceptanceId });
      } catch (error) {
        if (stopping) return;
        send({ type: "provider.rejected", connectionId: message.connectionId, acceptanceId: message.acceptanceId, error: describe(error) });
      }
      return;
    }
    if (message.type === "provider.close") return closeProvider(message.connectionId);
    if (message.type === "hook") { void invokeHook(message); return; }
    if (message.type === "hook.cancel") { hookRequests.get(message.requestId)?.abort(); return; }
    if (message.type === "paseo_frame") {
      let encoded;
      if (message.isBinary) {
        if (!(message.data instanceof Uint8Array)) throw new Error("Binary Paseo frame must be bytes");
        encoded = Buffer.from(message.data).toString("utf8");
      } else {
        if (typeof message.data !== "string") throw new Error("Text Paseo frame must be a string");
        encoded = message.data;
      }
      const frame = JSON.parse(encoded);
      const pending = paseoRequests.get(frame.requestId);
      if (pending) {
        paseoRequests.delete(frame.requestId);
        if (frame.error !== undefined) pending.reject(new Error(String(frame.error)));
        else pending.resolve(frame.output);
      }
      return;
    }
    if (message.type === "paseo_close") {
      for (const pending of paseoRequests.values()) pending.reject(new Error("Paseo transport closed"));
      paseoRequests.clear();
      return;
    }
    if (message.type === "shutdown") {
      stopping = true;
      for (const controller of hookRequests.values()) controller.abort();
      hookHandlers.event.clear();
      hookHandlers.before.clear();
      for (const pending of pendingConnections.values()) pending.tombstoned = true;
      await cleanup?.();
      await Promise.all([...connections.keys()].map(closeProvider));
      send({ type: "paseo_close" });
      process.disconnect();
      return;
    }
    throw new Error(`Unsupported selected worker request: ${message.type}`);
  })().catch((error) => {
    if (message.requestId) send({ type: "error", requestId: message.requestId, error: describe(error) });
    else { send({ type: "fatal", error: describe(error) }); process.disconnect(); }
  });
});
"#;

const SELECTED_NODE_FORK_BRIDGE: &str = r#"
const { fork } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const readline = require("node:readline");
const source = process.argv[1];
const worker = path.join(os.tmpdir(), `spocky-selected-plugin-${process.pid}.cjs`);
fs.writeFileSync(worker, source, { mode: 0o600 });
const child = fork(worker, [], {
  serialization: "advanced",
  stdio: ["ignore", "ignore", "inherit", "ipc"],
});
function encodeMessage(message) {
  if (message?.type !== "paseo_frame" || !message.isBinary) return message;
  if (!(message.data instanceof Uint8Array)) throw new Error("Binary Paseo frame must be bytes");
  return { ...message, data: Array.from(message.data) };
}
function decodeMessage(message) {
  if (message?.type !== "paseo_frame" || !message.isBinary) return message;
  if (!Array.isArray(message.data) ||
      message.data.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
    throw new Error("Binary Paseo frame must be bytes");
  }
  return { ...message, data: Uint8Array.from(message.data) };
}
child.on("message", (message) => {
  try { process.stdout.write(`${JSON.stringify(encodeMessage(message))}\n`); }
  catch { process.exit(1); }
});
child.on("error", () => process.exit(1));
child.on("exit", (code, signal) => {
  try { fs.unlinkSync(worker); } catch {}
  process.exit(code ?? (signal ? 1 : 0));
});
const lines = readline.createInterface({ input: process.stdin });
lines.on("close", () => { if (!child.killed) child.kill("SIGKILL"); });
lines.on("line", (line) => {
  if (!child.connected) process.exit(1);
  let message;
  try { message = decodeMessage(JSON.parse(line)); }
  catch { process.exit(1); return; }
  child.send(message, (error) => { if (error) process.exit(1); });
});
"#;

#[derive(Clone, Debug)]
pub struct CompiledPluginServer {
    bundle: String,
}

impl CompiledPluginServer {
    #[must_use]
    pub fn from_bundle(bundle: impl Into<String>) -> Self {
        Self {
            bundle: bundle.into(),
        }
    }

    pub fn run(
        &self,
        plugin_id: &str,
        plugin_directory: &str,
        timeout: Duration,
    ) -> Result<SelectedServerRun, PluginError> {
        self.run_inner(plugin_id, plugin_directory, None, None, &[], timeout)
    }

    pub fn run_and_invoke(
        &self,
        plugin_id: &str,
        plugin_directory: &str,
        method: &str,
        input: serde_json::Value,
        timeout: Duration,
    ) -> Result<SelectedServerRun, PluginError> {
        self.run_inner(
            plugin_id,
            plugin_directory,
            None,
            Some(&(method, input)),
            &[],
            timeout,
        )
    }

    pub fn run_with_protocol_steps(
        &self,
        plugin_id: &str,
        plugin_directory: &str,
        settings_directory: &Path,
        protocol_steps: &[RuntimeProtocolStep],
        timeout: Duration,
    ) -> Result<SelectedServerRun, PluginError> {
        self.run_inner(
            plugin_id,
            plugin_directory,
            Some(settings_directory),
            None,
            protocol_steps,
            timeout,
        )
    }

    fn run_inner(
        &self,
        plugin_id: &str,
        plugin_directory: &str,
        settings_directory: Option<&Path>,
        invocation: Option<&(&str, serde_json::Value)>,
        protocol_steps: &[RuntimeProtocolStep],
        timeout: Duration,
    ) -> Result<SelectedServerRun, PluginError> {
        PluginId::new(plugin_id)?;
        if invocation.is_some_and(|(method, _)| method.is_empty()) {
            return Err(PluginError::RuntimeProtocol);
        }
        let mut child = Command::new(node_program())
            .arg("-e")
            .arg(SELECTED_NODE_FORK_BRIDGE)
            .arg("--")
            .arg(SELECTED_SERVER_WORKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let settings_directory = settings_directory.map(Path::to_string_lossy);
        let result = exchange_runtime(
            &mut child,
            &RuntimeInitialize::new(
                plugin_id,
                &self.bundle,
                "0.8.0-pilot",
                plugin_directory,
                settings_directory.as_deref(),
            ),
            invocation.map(|(method, input)| (*method, input.clone())),
            protocol_steps,
            timeout,
        );
        if result.is_err() {
            terminate_runtime(&mut child);
        }
        let (contributions, traffic, invocation_output) = result?;
        Ok(SelectedServerRun {
            contributions,
            traffic,
            invocation_output,
            worker_exited: true,
        })
    }
}

pub fn compile_plugin_server(
    entry: &Path,
    esbuild: &Path,
    timeout: Duration,
) -> Result<CompiledPluginServer, PluginError> {
    let output = run_bounded(
        Command::new(esbuild).arg(entry).args([
            "--bundle",
            "--format=cjs",
            "--jsx=automatic",
            "--platform=node",
            "--target=node20",
            "--external:@getpaseo/plugin",
            "--external:@getpaseo/plugin/*",
            "--external:zod",
            "--log-level=warning",
        ]),
        timeout,
    )
    .map_err(|error| match error {
        PluginError::CommandFailed(message) => PluginError::ServerCompileFailed(message),
        other => other,
    })?;
    let code = String::from_utf8(output.stdout).map_err(|_| PluginError::InvalidCommandOutput)?;
    Ok(CompiledPluginServer::from_bundle(format!(
        "(function(require) {{\nconst module = {{ exports: {{}} }};\nconst exports = module.exports;\n{code}\nreturn module.exports;\n}})"
    )))
}

#[derive(Debug)]
pub struct SelectedServerRun {
    contributions: Vec<Contribution>,
    traffic: Vec<RuntimeTraffic>,
    invocation_output: Option<serde_json::Value>,
    worker_exited: bool,
}

impl SelectedServerRun {
    #[must_use]
    pub const fn invocation_output(&self) -> Option<&serde_json::Value> {
        self.invocation_output.as_ref()
    }

    #[must_use]
    pub fn contribution_labels(&self) -> Vec<String> {
        self.contributions
            .iter()
            .map(|contribution| match contribution {
                Contribution::Rpc(id) => format!("rpc:{id}"),
                Contribution::Provider(id) => format!("provider:{id}"),
                Contribution::UsageSource(id) => format!("usage:{id}"),
                Contribution::HookEvent(id) => format!("hook:event:{id}"),
                Contribution::HookBefore(id) => format!("hook:before:{id}"),
                Contribution::Surface(id) => format!("surface:{id}"),
                Contribution::SettingsScreen(id) => format!("settings:{id}"),
            })
            .collect()
    }

    #[must_use]
    pub fn traffic(&self) -> &[RuntimeTraffic] {
        &self.traffic
    }

    #[must_use]
    pub const fn worker_exited(&self) -> bool {
        self.worker_exited
    }
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
        let mut command = Command::new(node_program());
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
                None,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_directory: Option<&'a str>,
}

impl<'a> RuntimeInitialize<'a> {
    fn new(
        plugin_id: &'a str,
        bundle: &'a str,
        app_version: &'a str,
        plugin_directory: &'a str,
        settings_directory: Option<&'a str>,
    ) -> Self {
        Self {
            kind: "initialize",
            plugin_id,
            bundle,
            app_version,
            plugin_directory,
            settings_directory,
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
        let actual_revision = js_trim(
            &String::from_utf8(output.stdout).map_err(|_| PluginError::InvalidCommandOutput)?,
        )
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
            Command::new(npm_program())
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
    let output = run_bounded_raw(command, timeout)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(PluginError::CommandFailed(
        js_trim(&String::from_utf8_lossy(&output.stderr)).to_owned(),
    ))
}

/// [`run_bounded`] without the exit status check: the output of a command that
/// exited, whatever its status; a timeout is still an error.
fn run_bounded_raw(command: &mut Command, timeout: Duration) -> Result<Output, PluginError> {
    #[cfg(unix)]
    command.process_group(0);
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
            return Ok(Output {
                status,
                stdout: join_reader(stdout_reader)?,
                stderr: join_reader(stderr_reader)?,
            });
        }
        if Instant::now() >= deadline {
            terminate_command_tree(&mut child)?;
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(PluginError::CommandTimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn terminate_command_tree(child: &mut Child) -> Result<(), PluginError> {
    let process_group = format!("-{}", child.id());
    let killed = Command::new("/bin/kill")
        .args(["-KILL", "--", &process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if killed.success() {
        Ok(())
    } else {
        child.kill().map_err(PluginError::Io)
    }
}

#[cfg(windows)]
fn terminate_command_tree(child: &mut Child) -> Result<(), PluginError> {
    let status = Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        child.kill().map_err(PluginError::Io)
    }
}

#[cfg(all(not(unix), not(windows)))]
fn terminate_command_tree(child: &mut Child) -> Result<(), PluginError> {
    child.kill().map_err(PluginError::Io)
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
    let mut traffic = vec![RuntimeTraffic {
        direction: "host_to_plugin",
        message: initialize.clone(),
    }];
    let ready = loop {
        let line = receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => PluginError::RuntimeTimedOut,
                mpsc::RecvTimeoutError::Disconnected => PluginError::RuntimeProtocol,
            })??;
        traffic.push(RuntimeTraffic {
            direction: "plugin_to_host",
            message: line.clone(),
        });
        let message: serde_json::Value = serde_json::from_str(&line)?;
        match message.get("type").and_then(serde_json::Value::as_str) {
            Some("hooks.changed") => {
                decode_process_message(&line).map_err(|_| PluginError::RuntimeProtocol)?;
            }
            Some("ready") => break serde_json::from_str::<ReadyMessage>(&line)?,
            Some("fatal") => {
                let error = message
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Plugin initialization failed")
                    .to_owned();
                return Err(PluginError::RuntimeFatal(error));
            }
            _ => return Err(PluginError::RuntimeProtocol),
        }
    };
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
                    .map_err(|error| match error {
                        mpsc::RecvTimeoutError::Timeout => PluginError::RuntimeTimedOut,
                        mpsc::RecvTimeoutError::Disconnected => PluginError::RuntimeProtocol,
                    })??;
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
    ClientCompileFailed(String),
    ServerCompileFailed(String),
    ClientEvaluationFailed(String),
    ClientDisconnected,
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

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests {
    use super::{node_program, npm_program, run_bounded};
    use std::fs;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[test]
    fn platform_runtime_programs_are_native_executables() {
        #[cfg(windows)]
        {
            assert_eq!(node_program(), "node.exe");
            assert_eq!(npm_program(), "npm.cmd");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(node_program(), "node");
            assert_eq!(npm_program(), "npm");
        }
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn bounded_command_timeout_reaps_descendants() {
        let root = std::env::temp_dir().join(format!(
            "spocky-plugin-command-timeout-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create timeout fixture");
        let child_pid = root.join("child.pid");
        let script = format!(
            "sleep 30 </dev/null >/dev/null 2>&1 & child=$!; printf %s $child > '{}'; wait $child",
            child_pid.display()
        );

        assert!(matches!(
            run_bounded(
                Command::new("/bin/sh").args(["-c", &script]),
                Duration::from_millis(150),
            ),
            Err(super::PluginError::CommandTimedOut)
        ));
        let pid = fs::read_to_string(&child_pid).expect("child pid");
        let probe = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe child");
        assert!(!probe.success(), "timed-out descendant must be reaped");
        fs::remove_dir_all(&root).expect("remove timeout fixture");
    }

    #[cfg(windows)]
    #[test]
    fn bounded_command_timeout_reaps_windows_descendants() {
        let root = std::env::temp_dir().join(format!(
            "spocky-plugin-command-timeout-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create timeout fixture");
        let child_pid = root.join("child.pid");
        let script = format!(
            "$child = Start-Process -PassThru powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30'; Set-Content -NoNewline -Path '{}' -Value $child.Id; Wait-Process -Id $child.Id",
            child_pid.display()
        );

        assert!(matches!(
            run_bounded(
                Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]),
                Duration::from_millis(500),
            ),
            Err(super::PluginError::CommandTimedOut)
        ));
        let pid = fs::read_to_string(&child_pid).expect("child pid");
        let probe = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "if (Get-Process -Id {} -ErrorAction SilentlyContinue) {{ exit 1 }} else {{ exit 0 }}",
                    pid.trim()
                ),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe child");
        assert!(
            probe.success(),
            "timed-out Windows descendant must be reaped"
        );
        fs::remove_dir_all(&root).expect("remove timeout fixture");
    }
}
