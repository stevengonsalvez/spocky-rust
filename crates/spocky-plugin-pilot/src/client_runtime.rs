use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{PluginError, node_program, run_bounded};

const CLIENT_RUNTIME_BRIDGE: &str = r#"
const readline = require("node:readline");
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
let cleanup;
let online = true;
let connectionGeneration = 1;
const contributions = [];
const commands = new Map();
const slashCommands = new Map();
const transformers = new Map();
const buttons = new Map();

function reply(value) { process.stdout.write(`${JSON.stringify(value)}\n`); }
function fail(error) {
  const message = error instanceof Error ? error.message : String(error);
  const code = message.startsWith("Spocky host is disconnected:") ? "disconnected" : "request";
  reply({ type: "error", code, message });
}
function requireId(value, label) {
  const id = String(value ?? "").trim();
  if (!/^[a-z][a-z0-9-]*$/.test(id)) throw new Error(`Invalid ${label}: ${value}`);
  return id;
}
function getPaseoClient(serverId) {
  if (!online) throw new Error(`Spocky host is disconnected: ${serverId}`);
  return { connectionGeneration };
}
function runtimeRequire(name) {
  if (name === "@getpaseo/plugin/client") return { getPaseoClient };
  if (name === "@getpaseo/plugin" || name === "@getpaseo/plugin/client/ui" ||
      name === "@getpaseo/plugin/client/react-native" || name === "react" ||
      name === "react/jsx-runtime" || name === "react-native" ||
      name === "@tanstack/react-query" || name === "zod") return {};
  throw new Error(`Module \"${name}\" is not available in plugin client code`);
}
function requireText(value, label) {
  const text = String(value ?? "").trim();
  if (!text) throw new Error(`${label} is required`);
  return text;
}
function requireContext(value, allowed, label) {
  if (!allowed.includes(value)) throw new Error(`${label} has invalid context`);
  return value;
}
function register(contribution, release = () => {}) {
  contributions.push(contribution);
  let active = true;
  return () => {
    if (!active) return;
    active = false;
    const index = contributions.indexOf(contribution);
    if (index !== -1) contributions.splice(index, 1);
    release();
  };
}
function registerButton(kind, item) {
  const id = requireId(item.id, `${kind} id`);
  requireText(item.workspaceId, `${kind} workspaceId`);
  if (kind === "composerPill") requireText(item.agentId, `${kind} agentId`);
  const behavior = item.button && item.button.behavior;
  if (!behavior || !["action", "menu", "popover"].includes(behavior.kind)) {
    throw new Error(`${kind} ${id} has invalid behavior`);
  }
  const key = `${kind}:${id}`;
  if (buttons.has(key)) throw new Error(`Duplicate ${kind}: ${id}`);
  buttons.set(key, item.button);
  const contribution = kind === "composerPill"
    ? { kind, id, workspaceId: item.workspaceId, agentId: item.agentId, behavior: behavior.kind }
    : { kind, id, workspaceId: item.workspaceId, behavior: behavior.kind };
  const removeContribution = register(contribution, () => buttons.delete(key));
  let active = true;
  return {
    update(patch) {
      if (!active) return;
      const button = buttons.get(key);
      buttons.set(key, { ...button, ...patch });
    },
    remove() {
      if (!active) return;
      active = false;
      removeContribution();
    },
  };
}
const plugin = {
  paseo: {},
  rpc() { throw new Error("RPC unavailable in pilot"); },
  openSettings() {},
  openSurface() {},
  openPanel() {},
  addSettingsScreen(item) {
    const id = requireId(item.id, "settings screen id");
    requireText(item.title, `Settings screen ${id} title`);
    requireText(item.icon, `Settings screen ${id} icon`);
    if (typeof item.Component !== "function") throw new Error(`Settings screen ${id} is not a component`);
    return register({ kind: "settingsScreen", id });
  },
  addSurface(id, Component) {
    const normalized = requireId(id, "surface id");
    if (typeof Component !== "function") throw new Error(`Surface ${normalized} is not a component`);
    return register({ kind: "surface", id: normalized });
  },
  addSidebarItem(item) {
    const id = requireId(item.id, "sidebar item id");
    const surface = requireId(item.surface, "sidebar surface id");
    requireText(item.title, `Sidebar item ${id} title`);
    requireText(item.icon, `Sidebar item ${id} icon`);
    return register({ kind: "sidebarItem", id, surface });
  },
  addWorkspacePanel(item) {
    const id = requireId(item.id, "workspace panel id");
    const context = requireContext(item.context, ["workspace", "agent"], `Workspace panel ${id}`);
    requireText(item.title, `Workspace panel ${id} title`);
    requireText(item.icon, `Workspace panel ${id} icon`);
    if (typeof item.Component !== "function") throw new Error(`Workspace panel ${id} is not a component`);
    const locations = item.locations === undefined ? ["workspace"] : item.locations;
    if (!Array.isArray(locations) || locations.length === 0 ||
        locations.some((location) => !["workspace", "explorer"].includes(location)) ||
        new Set(locations).size !== locations.length) {
      throw new Error(`Workspace panel ${id} has invalid locations`);
    }
    return register({ kind: "workspacePanel", id, context, locations });
  },
  addCommandCenterItem(item) {
    const id = requireId(item.id, "Command Center item id");
    const context = requireContext(item.context, ["global", "workspace", "agent"], `Command Center item ${id}`);
    requireText(item.title, `Command Center item ${id} title`);
    requireText(item.icon, `Command Center item ${id} icon`);
    if (typeof item.onSelect !== "function") throw new Error(`Command Center item ${id} has no callback`);
    commands.set(id, item.onSelect);
    return register({ kind: "commandCenterItem", id, context }, () => commands.delete(id));
  },
  addSlashCommand(item) {
    const name = requireId(item.name, "client slash command name");
    const context = requireContext(item.context, ["workspace", "agent"], `Client slash command ${name}`);
    requireText(item.description, `Client slash command ${name} description`);
    if (typeof item.onSubmit !== "function") throw new Error(`Client slash command ${name} has no callback`);
    slashCommands.set(name, item.onSubmit);
    return register({ kind: "slashCommand", name, context }, () => slashCommands.delete(name));
  },
  addAttachmentSource(item) {
    const id = requireId(item.id, "attachment source id");
    requireText(item.title, `Attachment source ${id} title`);
    requireText(item.icon, `Attachment source ${id} icon`);
    requireText(item.pickerTitle, `Attachment source ${id} pickerTitle`);
    requireText(item.searchPlaceholder, `Attachment source ${id} searchPlaceholder`);
    const method = requireText(item.search && item.search.name, `Attachment source ${id} search RPC`);
    return register({ kind: "attachmentSource", id, method });
  },
  addTheme(item) {
    const id = requireId(item.id, "theme id");
    requireText(item.name, `Theme ${id} name`);
    if (!["light", "dark"].includes(item.appearance)) throw new Error(`Theme ${id} has invalid appearance`);
    if (!item.colors || typeof item.colors !== "object") throw new Error(`Theme ${id} has no colors`);
    return register({ kind: "theme", id, appearance: item.appearance });
  },
  addTimelineTransformer(item) {
    const id = requireId(item.id, "timeline transformer id");
    const itemType = item.query && item.query.itemType;
    if (!["user_message", "assistant_message", "reasoning", "tool_call", "todo", "error", "compaction"].includes(itemType)) {
      throw new Error(`Timeline transformer ${id} has invalid item type: ${itemType}`);
    }
    if (typeof item.transform !== "function") throw new Error(`Timeline transformer ${id} has no transform`);
    transformers.set(id, item.transform);
    return register({ kind: "timelineTransformer", id, itemType }, () => transformers.delete(id));
  },
  addTimelineRenderer(item) {
    const kind = requireId(item.kind, "timeline renderer kind");
    if (!Number.isInteger(item.version) || item.version < 1) throw new Error(`Timeline renderer ${kind} has invalid version`);
    if (!item.schema || typeof item.schema.safeParse !== "function") throw new Error(`Timeline renderer ${kind}/${item.version} has no schema`);
    if (typeof item.Component !== "function") throw new Error(`Timeline renderer ${kind}/${item.version} is not a component`);
    return register({ kind: "timelineRenderer", kindId: kind, version: item.version });
  },
  addHeaderButton(item) {
    return registerButton("headerButton", item);
  },
  addComposerPill(item) {
    return registerButton("composerPill", item);
  },
};

async function handle(message) {
  if (message.type === "start") {
    try {
      const evaluate = globalThis.eval;
      const factory = evaluate(message.bundle);
      if (typeof factory !== "function") throw new Error("client bundle is not executable");
      const exports = factory(runtimeRequire);
      const setup = exports && typeof exports === "object" ? exports.default : undefined;
      if (typeof setup !== "function") throw new Error("client bundle must default export a function");
      cleanup = setup(plugin);
      if (typeof cleanup !== "function") throw new Error("client contribution must return a cleanup function");
      const surfaces = new Set(contributions.filter((item) => item.kind === "surface").map((item) => item.id));
      for (const item of contributions) {
        if (item.kind === "sidebarItem" && !surfaces.has(item.surface)) {
          throw new Error(`Sidebar item ${item.id} references missing surface ${item.surface}`);
        }
      }
      reply({ type: "ready", contributions });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      reply({ type: "error", code: "evaluation", message });
    }
    return;
  }
  if (message.type === "host") {
    if (message.online && !online) connectionGeneration += 1;
    online = message.online;
    reply({ type: "ok" });
    return;
  }
  if (message.type === "invoke") {
    try {
      const command = commands.get(message.id);
      if (!command) throw new Error(`Unknown command: ${message.id}`);
      reply({ type: "result", value: await command(message.context) });
    } catch (error) { fail(error); }
    return;
  }
  if (message.type === "slash") {
    try {
      const command = slashCommands.get(message.name);
      if (!command) throw new Error(`Unknown slash command: ${message.name}`);
      reply({ type: "result", value: await command({ ...message.context, args: message.args }) });
    } catch (error) { fail(error); }
    return;
  }
  if (message.type === "transform") {
    try {
      const transform = transformers.get(message.id);
      if (!transform) throw new Error(`Unknown timeline transformer: ${message.id}`);
      reply({ type: "result", value: await transform({ item: message.item, phase: message.phase }) });
    } catch (error) { fail(error); }
    return;
  }
  if (message.type === "press") {
    try {
      const button = buttons.get(`${message.kind}:${message.id}`);
      if (!button) throw new Error(`Unknown button: ${message.kind}:${message.id}`);
      if (button.behavior.kind !== "action") throw new Error(`Button is not an action: ${message.kind}:${message.id}`);
      reply({ type: "result", value: await button.behavior.onPress() });
    } catch (error) { fail(error); }
    return;
  }
  if (message.type === "shutdown") {
    try { if (cleanup) await cleanup(); reply({ type: "ok" }); }
    catch (error) { fail(error); }
    lines.close();
    return;
  }
  fail(new Error(`Unknown client runtime request: ${message.type}`));
}

let pending = Promise.resolve();
lines.on("line", (line) => {
  pending = pending.then(() => handle(JSON.parse(line))).catch(fail);
});
"#;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClientContribution {
    SettingsScreen {
        id: String,
    },
    Surface {
        id: String,
    },
    SidebarItem {
        id: String,
        surface: String,
    },
    WorkspacePanel {
        id: String,
        context: String,
        locations: Vec<String>,
    },
    CommandCenterItem {
        id: String,
    },
    SlashCommand {
        name: String,
        context: String,
    },
    AttachmentSource {
        id: String,
        method: String,
    },
    Theme {
        id: String,
        appearance: String,
    },
    TimelineTransformer {
        id: String,
        #[serde(rename = "itemType")]
        item_type: String,
    },
    #[serde(rename = "timelineRenderer")]
    TimelineRenderer {
        #[serde(rename = "kindId")]
        kind: String,
        version: u64,
    },
    HeaderButton {
        id: String,
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        behavior: String,
    },
    ComposerPill {
        id: String,
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
        behavior: String,
    },
}

#[derive(Clone, Debug)]
pub struct CompiledPluginClient {
    bundle: String,
}

impl CompiledPluginClient {
    #[must_use]
    pub fn from_bundle(bundle: impl Into<String>) -> Self {
        Self {
            bundle: bundle.into(),
        }
    }

    #[must_use]
    pub fn bundle(&self) -> &str {
        &self.bundle
    }

    pub fn start(&self, timeout: Duration) -> Result<ClientRuntimeSession, PluginError> {
        ClientRuntimeSession::start(&self.bundle, timeout)
    }
}

pub fn compile_plugin_client(
    entry: &Path,
    esbuild: &Path,
    timeout: Duration,
) -> Result<CompiledPluginClient, PluginError> {
    let output = run_bounded(
        Command::new(esbuild).arg(entry).args([
            "--bundle",
            "--format=cjs",
            "--jsx=automatic",
            "--platform=neutral",
            "--target=es2020",
            "--supported:async-await=false",
            "--external:@getpaseo/plugin",
            "--external:@getpaseo/plugin/*",
            "--external:@tanstack/react-query",
            "--external:react",
            "--external:react/jsx-runtime",
            "--external:react-native",
            "--external:zod",
            "--log-level=warning",
        ]),
        timeout,
    )
    .map_err(|error| match error {
        PluginError::CommandFailed(message) => PluginError::ClientCompileFailed(message),
        other => other,
    })?;
    let code = String::from_utf8(output.stdout).map_err(|_| PluginError::InvalidCommandOutput)?;
    let code = code.replace("get: () => from[key]", "value: from[key]");
    Ok(CompiledPluginClient {
        bundle: format!(
            "(function(require) {{\nconst module = {{ exports: {{}} }};\nconst exports = module.exports;\n{code}\nreturn module.exports;\n}})"
        ),
    })
}

type LineReceiver = mpsc::Receiver<Result<String, std::io::Error>>;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum RuntimeResponse {
    Ready {
        contributions: Vec<ClientContribution>,
    },
    Ok,
    Result {
        value: serde_json::Value,
    },
    Error {
        code: String,
        message: String,
    },
}

pub struct ClientRuntimeSession {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    lines: LineReceiver,
    contributions: Vec<ClientContribution>,
    stopped: bool,
}

impl std::fmt::Debug for ClientRuntimeSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientRuntimeSession")
            .field("contributions", &self.contributions)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl ClientRuntimeSession {
    fn start(bundle: &str, timeout: Duration) -> Result<Self, PluginError> {
        let mut child = Command::new(node_program())
            .args(["-e", CLIENT_RUNTIME_BRIDGE])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = BufWriter::new(child.stdin.take().ok_or(PluginError::RuntimeProtocol)?);
        let stdout = child.stdout.take().ok_or(PluginError::RuntimeProtocol)?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut runtime = Self {
            child,
            stdin,
            lines,
            contributions: Vec::new(),
            stopped: false,
        };
        match runtime.request(
            &serde_json::json!({"type": "start", "bundle": bundle}),
            timeout,
        ) {
            Ok(RuntimeResponse::Ready { contributions }) => {
                runtime.contributions = contributions;
                Ok(runtime)
            }
            Ok(RuntimeResponse::Error { message, .. }) => {
                runtime.stop_exact_child();
                Err(PluginError::ClientEvaluationFailed(message))
            }
            Ok(_) => {
                runtime.stop_exact_child();
                Err(PluginError::RuntimeProtocol)
            }
            Err(error) => {
                runtime.stop_exact_child();
                Err(error)
            }
        }
    }

    #[must_use]
    pub fn contributions(&self) -> &[ClientContribution] {
        &self.contributions
    }

    pub fn set_host_online(&mut self, online: bool, timeout: Duration) -> Result<(), PluginError> {
        match self.request(
            &serde_json::json!({"type": "host", "online": online}),
            timeout,
        )? {
            RuntimeResponse::Ok => Ok(()),
            RuntimeResponse::Error { code, message } => Err(map_runtime_error(&code, message)),
            _ => Err(PluginError::RuntimeProtocol),
        }
    }

    pub fn invoke_command(
        &mut self,
        id: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, PluginError> {
        self.invoke_command_with_context(id, &serde_json::json!({}), timeout)
    }

    pub fn invoke_command_with_context(
        &mut self,
        id: &str,
        context: &serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, PluginError> {
        match self.request(
            &serde_json::json!({"type": "invoke", "id": id, "context": context}),
            timeout,
        )? {
            RuntimeResponse::Result { value } => Ok(value),
            RuntimeResponse::Error { code, message } => Err(map_runtime_error(&code, message)),
            _ => Err(PluginError::RuntimeProtocol),
        }
    }

    pub fn invoke_slash_command(
        &mut self,
        name: &str,
        args: &str,
        context: &serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, PluginError> {
        match self.request(
            &serde_json::json!({"type": "slash", "name": name, "args": args, "context": context}),
            timeout,
        )? {
            RuntimeResponse::Result { value } => Ok(value),
            RuntimeResponse::Error { code, message } => Err(map_runtime_error(&code, message)),
            _ => Err(PluginError::RuntimeProtocol),
        }
    }

    pub fn transform_timeline(
        &mut self,
        id: &str,
        item: &serde_json::Value,
        phase: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, PluginError> {
        match self.request(
            &serde_json::json!({"type": "transform", "id": id, "item": item, "phase": phase}),
            timeout,
        )? {
            RuntimeResponse::Result { value } => Ok(value),
            RuntimeResponse::Error { code, message } => Err(map_runtime_error(&code, message)),
            _ => Err(PluginError::RuntimeProtocol),
        }
    }

    pub fn press_button(
        &mut self,
        kind: &str,
        id: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, PluginError> {
        match self.request(
            &serde_json::json!({"type": "press", "kind": kind, "id": id}),
            timeout,
        )? {
            RuntimeResponse::Result { value } => Ok(value),
            RuntimeResponse::Error { code, message } => Err(map_runtime_error(&code, message)),
            _ => Err(PluginError::RuntimeProtocol),
        }
    }

    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        self.stopped
    }

    #[must_use]
    pub fn active_registration_count(&self) -> usize {
        if self.stopped {
            0
        } else {
            self.contributions.len()
        }
    }

    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), PluginError> {
        if self.stopped {
            return Ok(());
        }
        match self.request(&serde_json::json!({"type": "shutdown"}), timeout)? {
            RuntimeResponse::Ok => {}
            RuntimeResponse::Error { code, message } => {
                return Err(map_runtime_error(&code, message));
            }
            _ => return Err(PluginError::RuntimeProtocol),
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                self.stopped = true;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.stop_exact_child();
        Err(PluginError::RuntimeTimedOut)
    }

    fn request(
        &mut self,
        request: &serde_json::Value,
        timeout: Duration,
    ) -> Result<RuntimeResponse, PluginError> {
        if self.stopped {
            return Err(PluginError::RuntimeProtocol);
        }
        serde_json::to_writer(&mut self.stdin, &request)?;
        writeln!(self.stdin)?;
        self.stdin.flush()?;
        let line = if let Ok(line) = self.lines.recv_timeout(timeout) {
            line?
        } else {
            self.stop_exact_child();
            return Err(PluginError::RuntimeTimedOut);
        };
        Ok(serde_json::from_str(&line)?)
    }

    fn stop_exact_child(&mut self) {
        if self.stopped {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
    }
}

impl Drop for ClientRuntimeSession {
    fn drop(&mut self) {
        self.stop_exact_child();
    }
}

fn map_runtime_error(code: &str, message: String) -> PluginError {
    if code == "disconnected" {
        PluginError::ClientDisconnected
    } else {
        PluginError::ClientEvaluationFailed(message)
    }
}
