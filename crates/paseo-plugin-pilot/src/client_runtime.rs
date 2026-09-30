use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{PluginError, run_bounded};

const CLIENT_RUNTIME_BRIDGE: &str = r#"
const readline = require("node:readline");
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
let cleanup;
let online = true;
let connectionGeneration = 1;
const contributions = [];
const commands = new Map();

function reply(value) { process.stdout.write(`${JSON.stringify(value)}\n`); }
function fail(error) {
  const message = error instanceof Error ? error.message : String(error);
  const code = message.startsWith("Paseo host is disconnected:") ? "disconnected" : "request";
  reply({ type: "error", code, message });
}
function requireId(value, label) {
  const id = String(value ?? "").trim();
  if (!/^[a-z][a-z0-9-]*$/.test(id)) throw new Error(`Invalid ${label}: ${value}`);
  return id;
}
function getPaseoClient(serverId) {
  if (!online) throw new Error(`Paseo host is disconnected: ${serverId}`);
  return { connectionGeneration };
}
function runtimeRequire(name) {
  if (name === "@getpaseo/plugin/client") return { getPaseoClient };
  throw new Error(`Module \"${name}\" is not available in plugin client code`);
}
function register(contribution) {
  contributions.push(contribution);
  return () => {};
}
const plugin = {
  paseo: {},
  rpc() { throw new Error("RPC unavailable in pilot"); },
  openSettings() {},
  openSurface() {},
  openPanel() {},
  addSurface(id, Component) {
    const normalized = requireId(id, "surface id");
    if (typeof Component !== "function") throw new Error(`Surface ${normalized} is not a component`);
    return register({ kind: "surface", id: normalized });
  },
  addSidebarItem(item) {
    const id = requireId(item.id, "sidebar item id");
    const surface = requireId(item.surface, "sidebar surface id");
    return register({ kind: "sidebarItem", id, surface });
  },
  addCommandCenterItem(item) {
    const id = requireId(item.id, "Command Center item id");
    if (typeof item.onSelect !== "function") throw new Error(`Command Center item ${id} has no callback`);
    commands.set(id, item.onSelect);
    return register({ kind: "commandCenterItem", id });
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
      reply({ type: "result", value: await command() });
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
    Surface { id: String },
    SidebarItem { id: String, surface: String },
    CommandCenterItem { id: String },
}

#[derive(Clone, Debug)]
pub struct CompiledPluginClient {
    bundle: String,
}

impl CompiledPluginClient {
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
        let mut child = Command::new("node")
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
        match self.request(&serde_json::json!({"type": "invoke", "id": id}), timeout)? {
            RuntimeResponse::Result { value } => Ok(value),
            RuntimeResponse::Error { code, message } => Err(map_runtime_error(&code, message)),
            _ => Err(PluginError::RuntimeProtocol),
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
        let line = self
            .lines
            .recv_timeout(timeout)
            .map_err(|_| PluginError::RuntimeTimedOut)??;
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
