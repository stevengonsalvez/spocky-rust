const fs = require("node:fs");
const path = require("node:path");
const { performance } = require("node:perf_hooks");

const relevantRequestTypes = new Set([
  "document",
  "stylesheet",
  "font",
  "image",
  "script",
  "xhr",
  "fetch",
]);

function captureByName(captures, name) {
  return captures.find((capture) => capture.name === name);
}

function valuesMatch(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function activationSelector(candidate) {
  return candidate ? ".action:nth-child(1)" : '[data-testid="open-project-submit"]';
}

function comparisonState(captures, visual = null) {
  const readiness = captures.map((capture) => ({
    name: capture.name,
    meaningfulRenderedText: (capture.guestStartup?.visibleText ?? "").trim().length > 0,
  }));
  const interaction = {};
  const accessibility = {};
  const offlineReload = {};
  for (const viewport of ["desktop", "mobile"]) {
    const original = captureByName(captures, `original-${viewport}`);
    const candidate = captureByName(captures, `candidate-${viewport}`);
    const originalActivation = original?.keyboardActivation ?? null;
    const candidateActivation = candidate?.keyboardActivation ?? null;
    interaction[viewport] = {
      original: originalActivation,
      candidate: candidateActivation,
      passes:
        originalActivation?.attempted === true &&
        originalActivation.changed === true &&
        candidateActivation?.attempted === true &&
        candidateActivation.changed === true &&
        originalActivation.after?.dialogs?.length === 1 &&
        valuesMatch(originalActivation.before, candidateActivation.before) &&
        valuesMatch(originalActivation.after, candidateActivation.after),
    };

    const originalAccessibility = original
      ? { reducedMotion: original.reducedMotion, keyboardFocus: original.keyboardFocus ?? null }
      : null;
    const candidateAccessibility = candidate
      ? { reducedMotion: candidate.reducedMotion, keyboardFocus: candidate.keyboardFocus ?? null }
      : null;
    accessibility[viewport] = {
      original: originalAccessibility,
      candidate: candidateAccessibility,
      passes:
        originalAccessibility?.reducedMotion === true &&
        candidateAccessibility?.reducedMotion === true &&
        originalAccessibility.keyboardFocus?.completed === true &&
        candidateAccessibility.keyboardFocus?.completed === true &&
        originalAccessibility.keyboardFocus.entries.length > 0 &&
        originalAccessibility.keyboardFocus.entries.every(
          (focus) => focus.tag && (focus.label || focus.text),
        ) &&
        valuesMatch(originalAccessibility.keyboardFocus, candidateAccessibility.keyboardFocus),
    };

    const originalOffline = original?.offlineReload ?? null;
    const candidateOffline = candidate?.offlineReload ?? null;
    let classification = "incomplete";
    if (originalOffline && candidateOffline) {
      if (!originalOffline.loaded && !candidateOffline.loaded) {
        classification = "shared-pinned-failure";
      } else if (originalOffline.loaded && candidateOffline.loaded) {
        classification = "shared-success";
      } else {
        classification = "divergent";
      }
    }
    offlineReload[viewport] = {
      original: originalOffline,
      candidate: candidateOffline,
      classification,
    };
  }
  const comparable = readiness.every((capture) => capture.meaningfulRenderedText);
  const accepted =
    comparable &&
    visual?.threshold?.metric === "different pixels" &&
    visual?.threshold?.maximum === 0 &&
    visual?.threshold?.normalization === "none" &&
    visual?.desktop?.differentPixels === 0 &&
    visual?.desktop?.passes === true &&
    visual?.mobile?.differentPixels === 0 &&
    visual?.mobile?.passes === true &&
    visual?.candidateStability?.samePage?.desktop?.differentPixels === 0 &&
    visual?.candidateStability?.samePage?.desktop?.passes === true &&
    visual?.candidateStability?.samePage?.mobile?.differentPixels === 0 &&
    visual?.candidateStability?.samePage?.mobile?.passes === true &&
    visual?.candidateStability?.freshContext?.desktop?.differentPixels === 0 &&
    visual?.candidateStability?.freshContext?.desktop?.passes === true &&
    visual?.candidateStability?.freshContext?.mobile?.differentPixels === 0 &&
    visual?.candidateStability?.freshContext?.mobile?.passes === true &&
    interaction.desktop.passes &&
    interaction.mobile.passes &&
    accessibility.desktop.passes &&
    accessibility.mobile.passes;
  return {
    accepted,
    comparable,
    readiness,
    visual,
    interaction,
    accessibility,
    offlineReload,
  };
}

if (process.argv[2] === "--validate-result") {
  const result = JSON.parse(fs.readFileSync(process.argv[3], "utf8"));
  const comparison = comparisonState(result.captures, result.comparison?.visual);
  process.stdout.write(`${JSON.stringify(comparison)}\n`);
  process.exit(comparison.accepted ? 0 : 2);
}

if (process.argv[2] === "--activation-selector") {
  const runtime = process.argv[3];
  if (!runtime || !["original", "candidate"].includes(runtime) || process.argv.length !== 4) {
    process.stderr.write("usage: browser-runtime-capture.cjs --activation-selector original|candidate\n");
    process.exit(2);
  }
  process.stdout.write(`${activationSelector(runtime === "candidate")}\n`);
  process.exit(0);
}

const [baselineUrl, candidateUrl, outputPath, screenshotDir, daemonPort] = process.argv.slice(2);
if (!baselineUrl || !candidateUrl || !outputPath || !screenshotDir || !daemonPort) {
  throw new Error(
    "usage: browser-runtime-capture.cjs BASELINE_URL CANDIDATE_URL OUTPUT SCREENSHOTS DAEMON_PORT",
  );
}

async function capture(
  browser,
  name,
  url,
  viewport,
  candidate,
  baselineDaemonPort,
  samePageStabilityName = null,
) {
  const context = await browser.newContext({
    viewport,
    reducedMotion: "reduce",
    serviceWorkers: "allow",
  });
  const page = await context.newPage();
  const instrumentation = createInstrumentation(page, name, candidate);
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
  try {
    instrumentation.mark("navigation:start", { phase: "initial" });
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 120_000 });
    await instrumentation.checkpoint("navigation:domcontentloaded", { phase: "initial" });
    await scheduleReadinessObservations(page);
    await instrumentation.checkpoint("fonts:status", { phase: "initial" });
    await waitForProductState(page, candidate);
    await instrumentation.checkpoint("meaningful-text:ready", { phase: "initial" });

    instrumentation.mark("keyboard-focus-scan:start");
    const keyboardFocus = await captureKeyboardFocusCycle(page);
    await instrumentation.checkpoint("keyboard-focus-scan:complete", {
      entries: keyboardFocus.entries.length,
      completed: keyboardFocus.completed,
    });
    await recordScheduledReadiness(page, instrumentation, "initial");

    const actionSelector = activationSelector(candidate);
    const action = page.locator(actionSelector);
    await action.waitFor({ state: "visible", timeout: 30_000 });
    await action.focus();
    const beforeActivation = await captureInteractionState(page);
    const fileChooser = page
      .waitForEvent("filechooser", { timeout: 750 })
      .then(() => true)
      .catch(() => false);
    instrumentation.mark("activation:dispatch", { control: actionSelector });
    await page.keyboard.press("Enter");
    const fileChooserOpened = await fileChooser;
    await page
      .waitForFunction(
        () =>
          document.querySelector('[role="dialog"]') !== null ||
          document.querySelector('[role="status"], [role="alert"]') !== null,
        null,
        { timeout: 2_000 },
      )
      .catch(() => {});
    const afterActivation = await captureInteractionState(page);
    const keyboardActivation = {
      attempted: true,
      changed: fileChooserOpened || !valuesMatch(beforeActivation, afterActivation),
      control: actionSelector,
      fileChooserOpened,
      before: beforeActivation,
      after: afterActivation,
    };
    await instrumentation.checkpoint("activation:result", {
      changed: keyboardActivation.changed,
      fileChooserOpened,
    });

    instrumentation.mark("navigation:start", { phase: "online-reload" });
    const onlineReload = await page
      .reload({ waitUntil: "domcontentloaded", timeout: 120_000 })
      .then(() => true)
      .catch(() => false);
    if (!onlineReload) throw new Error(`${name} online reload failed before screenshot`);
    await instrumentation.checkpoint("navigation:domcontentloaded", { phase: "online-reload" });
    await scheduleReadinessObservations(page);
    await instrumentation.checkpoint("fonts:status", { phase: "online-reload" });
    await waitForProductState(page, candidate);
    await instrumentation.checkpoint("meaningful-text:ready", { phase: "online-reload" });
    await page.evaluate(() => scrollTo(0, 0));
    const layoutGeometry = await captureLayoutGeometry(page, candidate);
    await instrumentation.checkpoint("screenshot:start");
    await page.screenshot({
      path: path.join(screenshotDir, `${name}.png`),
      fullPage: true,
    });
    if (samePageStabilityName) {
      await page.screenshot({
        path: path.join(screenshotDir, `${samePageStabilityName}.png`),
        fullPage: true,
      });
    }
    await instrumentation.checkpoint("screenshot:complete");
    await recordScheduledReadiness(page, instrumentation, "online-reload");
    const onlineUrl = page.url();
    const visibleText = await page.locator("body").innerText().catch(() => "");
    const reducedMotion = await page.evaluate(() =>
      matchMedia("(prefers-reduced-motion: reduce)").matches,
    );
    const serviceWorkerCount = await page.evaluate(async () =>
      "serviceWorker" in navigator ? (await navigator.serviceWorker.getRegistrations()).length : 0,
    );

    await context.setOffline(true);
    instrumentation.mark("navigation:start", { phase: "offline-reload" });
    const offlineReload = await page
      .reload({ waitUntil: "domcontentloaded", timeout: 8_000 })
      .then(() => ({ loaded: true, url: page.url() }))
      .catch((error) => ({ loaded: false, url: page.url(), error: error.message.split("\n")[0] }));
    instrumentation.mark("navigation:result", {
      phase: "offline-reload",
      loaded: offlineReload.loaded,
    });
    await context.setOffline(false);

    return {
      name,
      viewport,
      onlineUrl,
      onlineReload,
      offlineReload,
      reducedMotion,
      keyboardFocus,
      keyboardActivation,
      layoutGeometry,
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
      instrumentation: instrumentation.result(),
    };
  } catch (error) {
    await instrumentation.checkpoint("capture:failed", {
      message: error.message.split("\n")[0],
    }).catch(() => instrumentation.mark("capture:failed", { probeUnavailable: true }));
    error.captureFailure = {
      name,
      viewport,
      error: error.message.split("\n")[0],
      instrumentation: instrumentation.result(),
    };
    throw error;
  } finally {
    instrumentation.detach();
    await context.close();
  }
}

function createInstrumentation(page, name, candidate) {
  const startedAt = performance.now();
  const timeline = [];
  const requests = { completed: [], failed: [] };
  const pending = new Map();
  const monotonicMs = () => Number((performance.now() - startedAt).toFixed(3));
  const requestSummary = (request) => {
    let pathname = request.url();
    try {
      const parsed = new URL(request.url());
      pathname = `${parsed.pathname}${parsed.search}`;
    } catch {}
    return {
      method: request.method(),
      resourceType: request.resourceType(),
      pathname,
    };
  };
  const isRelevant = (request) => relevantRequestTypes.has(request.resourceType());
  const mark = (event, detail = {}) => {
    timeline.push({ event, monotonicMs: monotonicMs(), ...detail });
  };
  const onRequest = (request) => {
    if (isRelevant(request)) pending.set(request, requestSummary(request));
  };
  const onRequestFinished = (request) => {
    if (!isRelevant(request)) return;
    pending.delete(request);
    const entry = { ...requestSummary(request), monotonicMs: monotonicMs() };
    requests.completed.push(entry);
    mark("request:finished", entry);
  };
  const onRequestFailed = (request) => {
    if (!isRelevant(request)) return;
    pending.delete(request);
    const entry = {
      ...requestSummary(request),
      monotonicMs: monotonicMs(),
      error: request.failure()?.errorText ?? "unknown",
    };
    requests.failed.push(entry);
    mark("request:failed", entry);
  };
  page.on("request", onRequest);
  page.on("requestfinished", onRequestFinished);
  page.on("requestfailed", onRequestFailed);
  return {
    mark,
    async checkpoint(event, detail = {}) {
      const probe = await captureReadinessProbe(page);
      mark(event, {
        ...detail,
        probe: {
          ...probe,
          requests: { pending: [...pending.values()] },
        },
      });
    },
    result() {
      return {
        clock: "monotonic milliseconds from capture start",
        runtime: candidate ? "candidate" : "original",
        capture: name,
        timeline,
        requests: {
          completed: requests.completed,
          failed: requests.failed,
          pendingAtEnd: [...pending.values()],
        },
      };
    },
    detach() {
      page.off("request", onRequest);
      page.off("requestfinished", onRequestFinished);
      page.off("requestfailed", onRequestFailed);
    },
  };
}

async function captureReadinessProbe(page) {
  return page.evaluate(() => {
    const stylesheets = [...document.querySelectorAll('link[rel="stylesheet"]')];
    const images = [...document.images];
    const fontSet = document.fonts;
    const scheduled = globalThis.__spockyCaptureReadiness ?? null;
    return {
      documentReadyState: document.readyState,
      meaningfulTextLength: document.body?.innerText.trim().length ?? 0,
      stylesheets: {
        total: stylesheets.length,
        pending: stylesheets.filter((stylesheet) => !stylesheet.sheet).length,
        loaded: document.styleSheets.length,
      },
      fonts: {
        supported: Boolean(fontSet),
        status: fontSet?.status ?? "unsupported",
        pending: fontSet ? fontSet.status !== "loaded" : false,
        count: fontSet?.size ?? 0,
        readyObservedAtPageMonotonicMs: scheduled?.fontsReadyAt ?? null,
      },
      images: {
        total: images.length,
        pending: images.filter((image) => !image.complete).length,
        failed: images.filter((image) => image.complete && image.naturalWidth === 0).length,
      },
      animationFrames: {
        firstPageMonotonicMs: scheduled?.firstFrameAt ?? null,
        secondPageMonotonicMs: scheduled?.secondFrameAt ?? null,
      },
      activeElement: {
        tag: document.activeElement?.tagName.toLowerCase() ?? null,
        label: document.activeElement?.getAttribute("aria-label") ?? null,
        testId: document.activeElement?.getAttribute("data-testid") ?? null,
      },
    };
  });
}

async function scheduleReadinessObservations(page) {
  await page.evaluate(() => {
    const scheduled = {
      scheduledAt: performance.now(),
      fontsReadyAt: null,
      firstFrameAt: null,
      secondFrameAt: null,
    };
    globalThis.__spockyCaptureReadiness = scheduled;
    document.fonts?.ready.then(() => {
      scheduled.fontsReadyAt = performance.now();
    });
    requestAnimationFrame((firstFrameAt) => {
      scheduled.firstFrameAt = firstFrameAt;
      requestAnimationFrame((secondFrameAt) => {
        scheduled.secondFrameAt = secondFrameAt;
      });
    });
  });
}

async function recordScheduledReadiness(page, instrumentation, phase) {
  const probe = await captureReadinessProbe(page);
  instrumentation.mark("fonts:ready", {
    phase,
    observed: probe.fonts.readyObservedAtPageMonotonicMs !== null,
    probe,
  });
  instrumentation.mark("animation-frames:settled", {
    phase,
    observed: probe.animationFrames.secondPageMonotonicMs !== null,
    probe,
  });
}

async function captureInteractionState(page) {
  return page.evaluate(() => {
    const normalizedText = (element) => element.innerText?.replace(/\s+/g, " ").trim() ?? "";
    const semanticRole = (element) => {
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
        text: normalizedText(dialog),
        controls: [...dialog.querySelectorAll('button, a, input, select, textarea, [role="button"]')]
          .filter((element) => element.getClientRects().length > 0)
          .map((element) => ({
            tag: element.tagName.toLowerCase(),
            role: semanticRole(element),
            label: element.getAttribute("aria-label"),
            text: normalizedText(element),
            disabled:
              element.hasAttribute("disabled") || element.getAttribute("aria-disabled") === "true",
          })),
      })),
      status: [...document.querySelectorAll('[role="status"], [role="alert"]')]
        .map(normalizedText)
        .filter(Boolean),
    };
  });
}

async function captureKeyboardFocusCycle(page) {
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
      if (!active || active === document.body) {
        return { returnedToFirst: false, entry: null };
      }
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
          disabled:
            active.hasAttribute("disabled") || active.getAttribute("aria-disabled") === "true",
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

async function captureLayoutGeometry(page, candidate) {
  const selectors = candidate
    ? {
        menu: ".mobile-menu",
        logo: ".mark",
        addProject: ".action:nth-child(1)",
        importSession: ".action:nth-child(2)",
        setupProviders: ".action:nth-child(3)",
        communityStar: ".community a:nth-child(1)",
        communitySponsor: ".community a:nth-child(2)",
        communityChat: ".community a:nth-child(3)",
        sidebarEmpty: ".sidebar-empty",
        sidebarEmptyTitle: ".sidebar-empty-title",
        sidebarEmptyDetail: ".sidebar-empty-detail",
        sidebarAddProject: ".sidebar-empty-actions button:nth-child(1)",
        sidebarImportSession: ".sidebar-empty-actions button:nth-child(2)",
      }
    : {
        menu: '[data-testid="menu-button"]',
        logo: 'svg[viewBox="0 0 700 700"]',
        addProject: '[data-testid="open-project-submit"]',
        importSession: '[data-testid="open-project-import-session"]',
        setupProviders: '[data-testid="open-project-setup-providers"]',
        communityStar: '[data-testid="community-links-github-star"]',
        communitySponsor: '[data-testid="community-links-sponsor"]',
        communityChat: '[data-testid="community-links-discord"]',
        sidebarEmpty: '[data-testid="sidebar-project-empty-state"]',
        sidebarEmptyTitle: '[data-testid="sidebar-project-empty-state"] div:first-child > div:nth-child(1)',
        sidebarEmptyDetail: '[data-testid="sidebar-project-empty-state"] div:first-child > div:nth-child(2)',
        sidebarAddProject: '[data-testid="sidebar-project-empty-state"] button:nth-of-type(1)',
        sidebarImportSession: '[data-testid="sidebar-project-empty-state"] button:nth-of-type(2)',
      };
  return page.evaluate((entries) => {
    function snapshot(element) {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        rect: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        },
        style: {
          display: style.display,
          fontFamily: style.fontFamily,
          fontSize: style.fontSize,
          fontWeight: style.fontWeight,
          letterSpacing: style.letterSpacing,
          webkitFontSmoothing: style.webkitFontSmoothing,
          lineHeight: style.lineHeight,
          color: style.color,
          backgroundColor: style.backgroundColor,
          borderColor: style.borderColor,
          borderRadius: style.borderRadius,
          padding: style.padding,
          margin: style.margin,
          gap: style.gap,
        },
        directText: [...element.childNodes]
          .filter((node) => node.nodeType === Node.TEXT_NODE)
          .map((node) => node.textContent ?? "")
          .join(" ")
          .replace(/\s+/g, " ")
          .trim(),
        descendants: [...element.querySelectorAll("*")]
          .map((descendant) => ({
            tag: descendant.tagName.toLowerCase(),
            ...snapshotWithoutDescendants(descendant),
          }))
          .filter((descendant) => descendant.directText || descendant.tag === "svg")
          .slice(0, 20),
      };
    }
    function snapshotWithoutDescendants(element) {
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
        style: {
          fontSize: style.fontSize,
          fontFamily: style.fontFamily,
          fontWeight: style.fontWeight,
          letterSpacing: style.letterSpacing,
          webkitFontSmoothing: style.webkitFontSmoothing,
          lineHeight: style.lineHeight,
          color: style.color,
          margin: style.margin,
          gap: style.gap,
        },
        directText: [...element.childNodes]
          .filter((node) => node.nodeType === Node.TEXT_NODE)
          .map((node) => node.textContent ?? "")
          .join(" ")
          .replace(/\s+/g, " ")
          .trim(),
      };
    }
    return Object.fromEntries(
      Object.entries(entries).map(([key, selector]) => [
        key,
        snapshot(document.querySelector(selector)),
      ]),
    );
  }, selectors);
}

async function waitForProductState(page, candidate) {
  if (candidate) {
    await page.locator(".action:nth-child(1)").waitFor({
      state: "visible",
      timeout: 30_000,
    });
  } else {
    await page.locator('[data-testid="sidebar-project-empty-state"]').waitFor({
      state: "attached",
      timeout: 30_000,
    });
  }
  const first = await page.locator("body").innerText();
  await page.waitForTimeout(250);
  const second = await page.locator("body").innerText();
  if (first !== second) {
    await page.waitForTimeout(500);
  }
}

(async () => {
  const { chromium } = require("playwright");
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.mkdirSync(screenshotDir, { recursive: true });
  const executablePath = process.env.PASEO_CHROMIUM_EXECUTABLE || undefined;
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    const captures = [];
    let captureFailure = null;
    const result = () => ({
      schemaVersion: 1,
      baselineCommit: "5de45e208690b0efc51c59a585ae9729325a9204",
      candidate: "spocky-ui-renderer-pilot Dioxus 0.7.0 frozen release",
      captures,
      comparison: comparisonState(captures),
      ...(captureFailure ? { captureFailure } : {}),
      limitations: [
        "Original capture uses an isolated pinned daemon with an empty disposable home.",
        "Chromium does not exercise Electron webview guest APIs.",
        "Screenshots are observations, not pixel-parity acceptance.",
      ],
    });
    const writeCheckpoint = () =>
      fs.writeFileSync(outputPath, `${JSON.stringify(result(), null, 2)}\n`);
    writeCheckpoint();
    try {
      for (const [name, url, viewport, candidate, seededDaemonPort, samePageStabilityName] of [
        ["original-desktop", baselineUrl, { width: 1280, height: 800 }, false, daemonPort, null],
        ["original-mobile", baselineUrl, { width: 390, height: 844 }, false, daemonPort, null],
        ["original-repeat-desktop", baselineUrl, { width: 1280, height: 800 }, false, daemonPort, null],
        ["original-repeat-mobile", baselineUrl, { width: 390, height: 844 }, false, daemonPort, null],
        ["candidate-desktop", candidateUrl, { width: 1280, height: 800 }, true, null, "candidate-same-page-desktop"],
        ["candidate-mobile", candidateUrl, { width: 390, height: 844 }, true, null, "candidate-same-page-mobile"],
        ["candidate-fresh-desktop", candidateUrl, { width: 1280, height: 800 }, true, null, null],
        ["candidate-fresh-mobile", candidateUrl, { width: 390, height: 844 }, true, null, null],
      ]) {
        captures.push(
          await capture(
            browser,
            name,
            url,
            viewport,
            candidate,
            seededDaemonPort,
            samePageStabilityName,
          ),
        );
        writeCheckpoint();
      }
    } catch (error) {
      captureFailure = error.captureFailure ?? {
        error: error.message.split("\n")[0],
        instrumentation: null,
      };
      writeCheckpoint();
      throw error;
    }
    const comparison = comparisonState(captures);
    const completedResult = { ...result(), comparison };
    fs.writeFileSync(outputPath, `${JSON.stringify(completedResult, null, 2)}\n`);
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
