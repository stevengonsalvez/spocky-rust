// macOS gate run, executed inside the GitHub Actions job after the builds. It
// captures the shipped Paseo desktop app, host A (CEF) and host B (Electron) with the
// shared CDP driver, then compares them. Processes are stopped by exact PID tree.
//
// env: SPOCKY_REF (built reference checkout), SPOCKY_CEF_HOST (spocky-cef-host binary
//      inside the .app), SPOCKY_APP (Paseo binary inside the packaged .app),
//      SPOCKY_HOSTB (dir with Electron node_modules), SPOCKY_BUNDLE, SPOCKY_OUT,
//      SPOCKY_SCRIPTS
const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");

const need = (name) => {
  if (!process.env[name]) throw new Error(`${name} is required`);
  return process.env[name];
};
const ref = need("SPOCKY_REF");
const cefHost = need("SPOCKY_CEF_HOST");
const paseoApp = need("SPOCKY_APP");
const hostB = need("SPOCKY_HOSTB");
const bundle = need("SPOCKY_BUNDLE");
const out = need("SPOCKY_OUT");
const scripts = need("SPOCKY_SCRIPTS");
const driver = path.join(scripts, "renderer-platform-cdp-capture.cjs");
const compare = path.join(scripts, "renderer-platform-runtime-compare.py");
process.env.NODE_PATH = path.join(ref, "node_modules");
fs.mkdirSync(out, { recursive: true });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => (port === 6767 ? reject(new Error("forbidden port 6767")) : resolve(port)));
    });
  });
}
async function waitCdp(port) {
  for (let i = 0; i < 180; i += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (response.ok) return;
    } catch {}
    await sleep(1000);
  }
  throw new Error(`DevTools not ready on ${port}`);
}
function stop(child, daemonPort) {
  // Exact PID for the host, exact listening port for the daemon the app started.
  if (child && child.pid) {
    try { process.kill(child.pid, "SIGTERM"); } catch {}
  }
  if (daemonPort) {
    const listeners = spawnSync("lsof", ["-ti", `tcp:${daemonPort}`, "-sTCP:LISTEN"], { encoding: "utf8" });
    for (const pid of listeners.stdout.split(/\s+/).filter(Boolean)) {
      try { process.kill(Number(pid), "SIGTERM"); } catch {}
    }
  }
}
function capture(cdpPort, url, name, mode) {
  const result = spawnSync(process.execPath, [driver, String(cdpPort), url, out, name, mode], { stdio: "inherit", timeout: 600_000 });
  if (result.status !== 0) throw new Error(`capture ${name} failed`);
}
// Host output is kept raw next to the evidence, never discarded.
fs.mkdirSync(path.join(out, "logs"), { recursive: true });
let logCount = 0;
async function withProcess(command, args, options, run, daemonPort) {
  const log = fs.openSync(path.join(out, "logs", `host-${(logCount += 1)}-${path.basename(command)}.log`), "w");
  const child = spawn(command, args, { stdio: ["ignore", log, log], ...options });
  try {
    await run();
  } finally {
    stop(child, daemonPort);
    await sleep(1500);
  }
}

(async () => {
  const httpPort = await freePort();
  const httpLog = fs.openSync(path.join(out, "logs", "http-server.log"), "w");
  const http = spawn("python3", ["-m", "http.server", String(httpPort), "--bind", "127.0.0.1", "--directory", bundle], { stdio: ["ignore", httpLog, httpLog] });
  try {
    await sleep(2000);
    const bundleUrl = `http://127.0.0.1:${httpPort}/`;

    for (const name of ["original-desktop", "original-repeat-desktop"]) {
      const home = fs.mkdtempSync(path.join(os.tmpdir(), "paseo-home-"));
      const daemonPort = await freePort();
      const cdpPort = await freePort();
      fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({
        version: 1,
        daemon: { listen: `127.0.0.1:${daemonPort}`, relay: { enabled: false }, mcp: { enabled: false, injectIntoAgents: false } },
      }));
      const env = {
        PATH: process.env.PATH, HOME: home, USERPROFILE: home, TMPDIR: os.tmpdir(),
        PASEO_HOME: home, PASEO_LISTEN: `127.0.0.1:${daemonPort}`, PASEO_ELECTRON_USER_DATA_DIR: path.join(home, "user-data"),
        PASEO_DISABLE_SINGLE_INSTANCE_LOCK: "1",
        PASEO_ELECTRON_FLAGS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${cdpPort} --lang=en-US`,
      };
      await withProcess(paseoApp, [], { env }, async () => {
        await waitCdp(cdpPort);
        capture(cdpPort, "-", name, "desktop");
      }, daemonPort);
    }
    for (const name of ["candidate-desktop", "candidate-repeat-desktop"]) {
      const cdpPort = await freePort();
      const cache = fs.mkdtempSync(path.join(os.tmpdir(), "cef-cache-"));
      await withProcess(cefHost, [`--spocky-cdp-port=${cdpPort}`, `--spocky-cache=${cache}`, "--spocky-bound-ms=600000", `--spocky-url=${bundleUrl}`], {}, async () => {
        await waitCdp(cdpPort);
        capture(cdpPort, bundleUrl, name, "candidate");
      });
    }
    for (const name of ["candidate-electron-desktop", "candidate-electron-repeat-desktop"]) {
      const cdpPort = await freePort();
      fs.copyFileSync(path.join(scripts, "renderer-platform-electron-host.cjs"), path.join(hostB, "renderer-platform-electron-host.cjs"));
      // The electron package exports the path of the binary it installed.
      const resolved = spawnSync(process.execPath, ["-p", "require('electron')"], { cwd: hostB, encoding: "utf8" });
      const electron = resolved.stdout.trim();
      if (resolved.status !== 0 || !fs.existsSync(electron)) {
        throw new Error(`Electron binary not found in ${hostB}: ${resolved.stderr.trim() || electron}`);
      }
      await withProcess(electron, ["renderer-platform-electron-host.cjs"], { cwd: hostB, env: { ...process.env, CDP_PORT: String(cdpPort), HOST_BOUND_MS: "600000" } }, async () => {
        await waitCdp(cdpPort);
        capture(cdpPort, bundleUrl, name, "candidate");
      });
    }
  } finally {
    stop(http);
  }
  const pairs = [
    ["original-desktop", "candidate-desktop", "compare-first"],
    ["original-desktop", "candidate-repeat-desktop", "compare-repeat"],
    ["original-desktop", "candidate-electron-desktop", "compare-electron-first"],
    ["candidate-electron-desktop", "candidate-desktop", "engine-electron-vs-cef"],
    ["original-desktop", "original-repeat-desktop", "original-stability"],
    ["candidate-desktop", "candidate-repeat-desktop", "candidate-stability"],
    ["candidate-electron-desktop", "candidate-electron-repeat-desktop", "electron-stability"],
  ];
  for (const [a, b, name] of pairs) {
    const r = spawnSync("python3", [compare, path.join(out, `${a}.json`), path.join(out, `${b}.json`), path.join(out, `${name}.json`)], { stdio: "inherit" });
    if (r.status !== 0) throw new Error(`compare ${name} failed`);
  }
  const inputs = spawnSync(process.execPath, [path.join(scripts, "renderer-platform-inputs.cjs")], {
    stdio: "inherit",
    env: {
      ...process.env, SPOCKY_INPUTS_OUT: path.join(out, "inputs.json"), SPOCKY_BUNDLE: bundle, SPOCKY_CEF_HOST: cefHost,
      SPOCKY_APP_ASAR: path.join(path.dirname(path.dirname(paseoApp)), "Resources", "app.asar"),
      SPOCKY_WEB_EXPORT: path.join(ref, "packages", "app", "dist"), SPOCKY_HOSTB: hostB,
    },
  });
  if (inputs.status !== 0) throw new Error("recording the run inputs failed");
  console.log("RENDERER_MACOS_GATE_OK");
})().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
