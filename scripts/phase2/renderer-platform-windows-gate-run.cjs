// Windows gate run, executed inside the GitHub Actions job after the builds. It
// captures the shipped Paseo desktop app, host A (CEF) and host B (Electron) with the
// shared CDP driver, then compares them. Processes are stopped by exact PID tree.
//
// env: SPOCKY_REF (built reference checkout), SPOCKY_CEF_HOST (spocky-cef-host.exe),
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
function stop(child) {
  // Exact PID and its tree.
  if (child && child.pid) spawnSync("taskkill", ["/pid", String(child.pid), "/t", "/f"], { stdio: "ignore" });
}
function capture(cdpPort, url, name, mode) {
  const result = spawnSync(process.execPath, [driver, String(cdpPort), url, out, name, mode], { stdio: "inherit", timeout: 600_000 });
  if (result.status !== 0) throw new Error(`capture ${name} failed`);
}
async function withProcess(command, args, options, run) {
  const child = spawn(command, args, { stdio: "ignore", windowsHide: false, ...options });
  try {
    await run();
  } finally {
    stop(child);
  }
}

(async () => {
  const httpPort = await freePort();
  const http = spawn("python", ["-m", "http.server", String(httpPort), "--bind", "127.0.0.1", "--directory", bundle], { stdio: "ignore" });
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
        PATH: process.env.PATH, SystemRoot: process.env.SystemRoot, TEMP: os.tmpdir(), TMP: os.tmpdir(),
        HOME: home, USERPROFILE: home, APPDATA: path.join(home, "AppData"), LOCALAPPDATA: path.join(home, "Local"),
        PASEO_HOME: home, PASEO_LISTEN: `127.0.0.1:${daemonPort}`, PASEO_ELECTRON_USER_DATA_DIR: path.join(home, "user-data"),
        PASEO_DISABLE_SINGLE_INSTANCE_LOCK: "1",
        PASEO_ELECTRON_FLAGS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${cdpPort} --lang=en-US`,
      };
      const exe = path.join(ref, "packages", "desktop", "release", "win-unpacked", "Paseo.exe");
      await withProcess(exe, [], { env }, async () => {
        await waitCdp(cdpPort);
        capture(cdpPort, "-", name, "desktop");
      });
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
      const electron = path.join(hostB, "node_modules", "electron", "dist", "electron.exe");
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
    const r = spawnSync("python", [compare, path.join(out, `${a}.json`), path.join(out, `${b}.json`), path.join(out, `${name}.json`)], { stdio: "inherit" });
    if (r.status !== 0) throw new Error(`compare ${name} failed`);
  }
  console.log("RENDERER_WINDOWS_GATE_OK");
})().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
