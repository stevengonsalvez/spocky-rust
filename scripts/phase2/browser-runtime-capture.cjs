const fs = require("node:fs");
const path = require("node:path");

function comparisonState(captures) {
  const readiness = captures.map((capture) => ({
    name: capture.name,
    meaningfulRenderedText: capture.guestStartup.visibleText.trim().length > 0,
  }));
  return {
    comparable: readiness.every((capture) => capture.meaningfulRenderedText),
    readiness,
  };
}

if (process.argv[2] === "--validate-result") {
  const result = JSON.parse(fs.readFileSync(process.argv[3], "utf8"));
  const comparison = comparisonState(result.captures);
  process.stdout.write(`${JSON.stringify(comparison)}\n`);
  process.exit(comparison.comparable ? 0 : 2);
}

const [baselineUrl, candidateUrl, outputPath, screenshotDir, daemonPort] = process.argv.slice(2);
if (!baselineUrl || !candidateUrl || !outputPath || !screenshotDir || !daemonPort) {
  throw new Error(
    "usage: browser-runtime-capture.cjs BASELINE_URL CANDIDATE_URL OUTPUT SCREENSHOTS DAEMON_PORT",
  );
}

async function capture(browser, name, url, viewport, candidate, baselineDaemonPort) {
  const context = await browser.newContext({
    viewport,
    reducedMotion: "reduce",
    serviceWorkers: "allow",
  });
  const page = await context.newPage();
  const consoleErrors = [];
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  await page.route(/:(6767)\b/, (route) => route.abort());
  await page.routeWebSocket(/:(6767)\b/, async (socket) => {
    await socket.close({ code: 1008, reason: "Blocked connection to port 6767 during parity capture." });
  });
  await page.addInitScript(
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
    { port: baselineDaemonPort },
  );
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 120_000 });
  await page
    .waitForFunction(() => document.body.innerText.trim().length > 0, null, { timeout: 30_000 })
    .catch(() => undefined);

  const keyboardFocus = [];
  for (let index = 0; index < 4; index += 1) {
    await page.keyboard.press("Tab");
    keyboardFocus.push(
      await page.evaluate(() => {
        const active = document.activeElement;
        return {
          tag: active?.tagName.toLowerCase() ?? null,
          label: active?.getAttribute("aria-label") ?? null,
          text: active?.textContent?.trim().replace(/\s+/g, " ").slice(0, 120) ?? null,
        };
      }),
    );
  }

  let keyboardActivation = { attempted: false, changed: false, status: null };
  if (candidate) {
    const reviewer = page.getByRole("button", { name: /Reviewer/ });
    await reviewer.focus();
    const before = await page.getByRole("status").textContent();
    await page.keyboard.press("Enter");
    const after = await page.getByRole("status").textContent();
    keyboardActivation = { attempted: true, changed: before !== after, status: after };
  }

  const onlineReload = await page
    .reload({ waitUntil: "domcontentloaded", timeout: 120_000 })
    .then(() => true)
    .catch(() => false);
  await page.screenshot({
    path: path.join(screenshotDir, `${name}.png`),
    fullPage: true,
  });
  const onlineUrl = page.url();
  const visibleText = await page.locator("body").innerText().catch(() => "");
  const reducedMotion = await page.evaluate(() =>
    matchMedia("(prefers-reduced-motion: reduce)").matches,
  );
  const serviceWorkerCount = await page.evaluate(async () =>
    "serviceWorker" in navigator ? (await navigator.serviceWorker.getRegistrations()).length : 0,
  );

  await context.setOffline(true);
  const offlineReload = await page
    .reload({ waitUntil: "domcontentloaded", timeout: 8_000 })
    .then(() => ({ loaded: true, url: page.url() }))
    .catch((error) => ({ loaded: false, url: page.url(), error: error.message.split("\n")[0] }));
  await context.setOffline(false);
  await context.close();

  return {
    name,
    viewport,
    onlineUrl,
    onlineReload,
    offlineReload,
    reducedMotion,
    keyboardFocus,
    keyboardActivation,
    guestStartup: {
      isolatedDaemonSeededBeforeNavigation: Boolean(baselineDaemonPort),
      visibleText: visibleText.replace(/\s+/g, " ").trim().slice(0, 500),
    },
    runtimeBoundary: {
      kind: "chromium-browser-context",
      embeddedGuestOrElectronWebview: false,
      serviceWorkerCount,
      port6767HttpAndWebSocketRoutesBlocked: true,
    },
    consoleErrors,
  };
}

(async () => {
  const { chromium } = require("playwright");
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.mkdirSync(screenshotDir, { recursive: true });
  const executablePath = process.env.PASEO_CHROMIUM_EXECUTABLE || undefined;
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    const captures = [];
    for (const [name, url, viewport, candidate, seededDaemonPort] of [
      ["original-desktop", baselineUrl, { width: 1280, height: 800 }, false, daemonPort],
      ["original-mobile", baselineUrl, { width: 390, height: 844 }, false, daemonPort],
      ["candidate-desktop", candidateUrl, { width: 1280, height: 800 }, true, null],
      ["candidate-mobile", candidateUrl, { width: 390, height: 844 }, true, null],
    ]) {
      captures.push(
        await capture(browser, name, url, viewport, candidate, seededDaemonPort),
      );
    }
    const comparison = comparisonState(captures);
    const result = {
      schemaVersion: 1,
      baselineCommit: "5de45e208690b0efc51c59a585ae9729325a9204",
      candidate: "paseo-ui-renderer-pilot Dioxus 0.7.0 frozen release",
      captures,
      comparison,
      limitations: [
        "Original capture uses an isolated pinned daemon with an empty disposable home.",
        "Chromium does not exercise Electron webview guest APIs.",
        "Screenshots are observations, not pixel-parity acceptance.",
      ],
    };
    fs.writeFileSync(outputPath, `${JSON.stringify(result, null, 2)}\n`);
    if (!comparison.comparable) {
      throw new Error("browser captures are not comparable: one or more runtimes rendered no meaningful text");
    }
  } finally {
    await browser.close();
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
