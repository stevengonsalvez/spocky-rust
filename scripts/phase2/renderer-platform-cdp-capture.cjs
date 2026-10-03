// Capture one runtime (Electron, CEF) over the Chrome DevTools Protocol.
//
// The page logic mirrors scripts/phase2/browser-runtime-capture.cjs (same seeded
// daemon registry, readiness wait, Tab focus cycle, activation state shape,
// full-page screenshot) so original and candidate captures are comparable. Two
// things differ because the host owns its window: the window size comes from the
// host, the viewport (scale factor 1) and media features are set through
// Playwright page emulation, and the default CDP page is reused because Electron
// and CEF refuse Target.createBrowserContext.
//
// usage: node renderer-platform-cdp-capture.cjs CDP_PORT URL OUT_DIR NAME MODE [DAEMON_PORT]
//   MODE is "desktop" (the packaged original app: no seeded storage, the page the
//   app opened is the start page), "original" (a Metro web build in a bare window;
//   seeds the isolated daemon registry; not the desktop product), or "candidate".
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const [cdpPort, urlArgument, outDir, name, mode, daemonPort] = process.argv.slice(2);
if (!cdpPort || !urlArgument || !outDir || !name || !["desktop", "original", "candidate"].includes(mode)) {
  process.stderr.write("usage: renderer-platform-cdp-capture.cjs CDP_PORT URL|- OUT_DIR NAME desktop|original|candidate [DAEMON_PORT]\n");
  process.exit(2);
}
if (mode === "original" && !daemonPort) {
  process.stderr.write("original mode requires DAEMON_PORT\n");
  process.exit(2);
}
if ([cdpPort, daemonPort].includes("6767")) {
  process.stderr.write("refusing port 6767\n");
  process.exit(2);
}
const candidate = mode === "candidate";
let url = urlArgument;
const viewport = { width: 1280, height: 800 };

// The first load of the original app waits for Metro to bundle, so the bound is
// generous. A timeout reports where the page is and what it shows.
const productStateTimeoutMs = 240_000;
async function waitForProductState(page) {
  try {
    if (candidate) {
      await page.locator(".action:nth-child(1)").waitFor({ state: "visible", timeout: productStateTimeoutMs });
    } else {
      await page.locator('[data-testid="sidebar-project-empty-state"]').waitFor({
        state: "attached",
        timeout: productStateTimeoutMs,
      });
    }
  } catch (error) {
    const text = await page.locator("body").innerText().catch(() => "");
    throw new Error(`product state not reached at ${page.url()}: ${text.replace(/\s+/g, " ").slice(0, 300)}`);
  }
  if (mode === "desktop") {
    // The desktop app renders the Pair device tile only after it has resolved its local
    // daemon id (open-project-screen.tsx). Capturing earlier is a race in the original.
    await page.locator('[data-testid="open-project-pair-device"]').waitFor({
      state: "attached",
      timeout: productStateTimeoutMs,
    });
  }
  const first = await page.locator("body").innerText();
  await page.waitForTimeout(250);
  const second = await page.locator("body").innerText();
  if (first !== second) await page.waitForTimeout(500);
}

async function settle(page) {
  return page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    return { fontsStatus: document.fonts.status, fontCount: document.fonts.size };
  });
}

async function interactionState(page) {
  return page.evaluate(() => {
    const text = (element) => element.innerText?.replace(/\s+/g, " ").trim() ?? "";
    const role = (element) => {
      const explicit = element.getAttribute("role");
      if (explicit) return explicit;
      if (element.tagName === "BUTTON") return "button";
      if (element.tagName === "A") return "link";
      if (element.tagName === "INPUT") return "textbox";
      return null;
    };
    return {
      dialogs: [...document.querySelectorAll('[role="dialog"]')].map((dialog) => ({
        role: "dialog",
        label: dialog.getAttribute("aria-label"),
        text: text(dialog),
        controls: [...dialog.querySelectorAll('button, a, input, select, textarea, [role="button"]')]
          .filter((element) => element.getClientRects().length > 0)
          .map((element) => ({
            tag: element.tagName.toLowerCase(),
            role: role(element),
            label: element.getAttribute("aria-label"),
            text: text(element),
            disabled: element.hasAttribute("disabled") || element.getAttribute("aria-disabled") === "true",
          })),
      })),
      status: [...document.querySelectorAll('[role="status"], [role="alert"]')].map(text).filter(Boolean),
    };
  });
}

async function focusCycle(page) {
  await page.evaluate(() => {
    globalThis.__spockyFirstFocusedElement = null;
    document.activeElement?.blur();
  });
  const entries = [];
  let completed = false;
  for (let index = 0; index < 64; index += 1) {
    await page.keyboard.press("Tab");
    const focused = await page.evaluate(() => {
      const active = document.activeElement;
      if (!active || active === document.body) return { returnedToFirst: false, entry: null };
      if (globalThis.__spockyFirstFocusedElement === null) {
        globalThis.__spockyFirstFocusedElement = active;
      } else if (active === globalThis.__spockyFirstFocusedElement) {
        return { returnedToFirst: true, entry: null };
      }
      const tag = active.tagName.toLowerCase();
      const role =
        active.getAttribute("role") ??
        (tag === "button" ? "button" : tag === "a" ? "link" : tag === "input" ? "textbox" : null);
      return {
        returnedToFirst: false,
        entry: {
          tag,
          role,
          label: active.getAttribute("aria-label"),
          text: active.innerText?.trim().replace(/\s+/g, " ").slice(0, 120) ?? null,
          disabled: active.hasAttribute("disabled") || active.getAttribute("aria-disabled") === "true",
        },
      };
    });
    if (focused.returnedToFirst) {
      completed = true;
      break;
    }
    if (focused.entry) entries.push(focused.entry);
  }
  return { entries, completed };
}

async function activate(page, selector) {
  const control = page.locator(selector);
  try {
    await control.waitFor({ state: "visible", timeout: 30_000 });
  } catch (error) {
    const text = await page.locator("body").innerText().catch(() => "");
    await page.screenshot({ path: path.join(outDir, `${name}-failure.png`) }).catch(() => {});
    throw new Error(`activation control ${selector} not visible at ${page.url()}: ${text.replace(/\s+/g, " ").slice(0, 300)}`);
  }
  await control.focus();
  const urlBefore = page.url();
  const before = await interactionState(page);
  await page.keyboard.press("Enter");
  await page
    .waitForFunction(
      () =>
        document.querySelector('[role="dialog"]') !== null ||
        document.querySelector('[role="status"], [role="alert"]') !== null,
      null,
      { timeout: 2_000 },
    )
    .catch(() => {});
  const after = await interactionState(page);
  const urlAfter = page.url();
  return {
    control: selector,
    changed: urlBefore !== urlAfter || JSON.stringify(before) !== JSON.stringify(after),
    urlBefore,
    urlAfter,
    before,
    after,
  };
}

// Keep roles, names, and properties. Drop the generated node and backend IDs.
function simplifyAxTree(nodes) {
  const byId = new Map(nodes.map((node) => [node.nodeId, node]));
  const simplify = (node) => ({
    role: node.role?.value ?? null,
    name: node.name?.value ?? null,
    ignored: node.ignored ?? false,
    properties: (node.properties ?? []).map((property) => [property.name, property.value?.value ?? null]),
    children: (node.childIds ?? []).map((id) => simplify(byId.get(id))),
  });
  return simplify(nodes[0]);
}

(async () => {
  const { chromium } = require("playwright-core");
  fs.mkdirSync(outDir, { recursive: true });
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
  try {
    const context = browser.contexts()[0];
    const page = context.pages()[0];
    const session = await context.newCDPSession(page);
    // Playwright viewport and media emulation, as the Chrome baseline capture uses.
    await page.setViewportSize(viewport);
    await page.emulateMedia({ colorScheme: "light", reducedMotion: "reduce" });
    await page.route(/:(6767)\b/, (route) => route.abort());
    await page.routeWebSocket(/:(6767)\b/, async (socket) => {
      await socket.close({ code: 1008, reason: "Blocked connection to port 6767 during parity capture." });
    });
    // Only the web-in-bare-window baseline seeds storage. The desktop app and the
    // candidate run with no seeded storage on either side.
    if (mode === "original") await page.addInitScript(
      ({ port }) => {
        localStorage.clear();
        if (!port) return;
        const now = new Date(0).toISOString();
        const endpoint = `127.0.0.1:${port}`;
        const connection = { id: `direct:${endpoint}`, type: "directTcp", endpoint };
        localStorage.setItem("@paseo:e2e", "1");
        localStorage.setItem(
          "@paseo:daemon-registry",
          JSON.stringify([
            {
              serverId: "browser-baseline-daemon",
              label: "isolated-baseline",
              connections: [connection],
              preferredConnectionId: connection.id,
              createdAt: now,
              updatedAt: now,
            },
          ]),
        );
      },
      { port: daemonPort },
    );
    const version = await session.send("Browser.getVersion");
    if (mode === "desktop") {
      // The packaged app opens its own page. Wait for it to be the app, then reuse its URL.
      const deadline = Date.now() + 120_000;
      while (!page.url().startsWith("paseo://") && Date.now() < deadline) await page.waitForTimeout(500);
      await waitForProductState(page);
      url = page.url();
    } else {
      await page.goto(url, { waitUntil: "domcontentloaded", timeout: 120_000 });
      await waitForProductState(page);
    }
    const environment = await page.evaluate(() => ({
      innerWidth,
      innerHeight,
      devicePixelRatio,
      lang: navigator.language,
      platform: navigator.platform,
      colorSchemeLight: matchMedia("(prefers-color-scheme: light)").matches,
      reducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches,
    }));
    const keyboardFocus = await focusCycle(page);

    // Same order as the Chrome baseline: activate Add project right after the focus
    // walk, when the page is fully interactive. Then reload and activate the Plus
    // control (the original navigates away on Plus, so reload before the screenshot).
    const addProjectSelector = candidate ? ".action:nth-child(1)" : '[data-testid="open-project-submit"]';
    const addProjectActivation = await activate(page, addProjectSelector);
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 120_000 });
    await waitForProductState(page);
    await page.waitForTimeout(2_000);
    const plusSelector = "[aria-label='New workspace']";
    const plusActivation = await activate(page, plusSelector);
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 120_000 });
    await waitForProductState(page);

    await page.reload({ waitUntil: "domcontentloaded", timeout: 120_000 });
    await waitForProductState(page);
    await page.evaluate(() => scrollTo(0, 0));
    const readiness = await settle(page);
    const screenshotPath = path.join(outDir, `${name}.png`);
    await page.screenshot({ path: screenshotPath, fullPage: true });
    const png = fs.readFileSync(screenshotPath);
    const axTree = simplifyAxTree((await session.send("Accessibility.getFullAXTree")).nodes);
    const result = {
      name,
      mode,
      url,
      browser: { product: version.product, userAgent: version.userAgent },
      viewport,
      storageSeeding: mode === "original" ? "@paseo:e2e flag and isolated daemon registry" : "none",
      // Fixed gate conditions, shared by every runtime. The shipped app runs with the
      // user's own settings, so these are pins of the gate, not masking.
      gateConditions: { colorScheme: "light", reducedMotion: "reduce", viewport, deviceScaleFactor: 1 },
      environment,
      readiness,
      screenshot: {
        file: `${name}.png`,
        sha256: crypto.createHash("sha256").update(png).digest("hex"),
        bytes: png.length,
        width: png.readUInt32BE(16),
        height: png.readUInt32BE(20),
      },
      keyboardFocus,
      plusActivation,
      addProjectActivation,
      axTree,
    };
    fs.writeFileSync(path.join(outDir, `${name}.json`), `${JSON.stringify(result, null, 2)}\n`);
    process.stdout.write(`${name} ${result.screenshot.sha256} ${environment.innerWidth}x${environment.innerHeight}@${environment.devicePixelRatio}\n`);
  } finally {
    await browser.close();
  }
})().catch((error) => {
  process.stderr.write(`capture failed: ${error.message.split("\n")[0]}\n`);
  process.exit(1);
});
