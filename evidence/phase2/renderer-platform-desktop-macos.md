# Original Paseo desktop app as shipped, on macOS (baseline)

Task P2-RENDERER-CEF-01, step 2 (macOS). This is the baseline for the CEF gate. It
supersedes the bare-window capture in `renderer-platform-electron-macos.md`, which
is a web renderer in pinned Chromium and stays out of the desktop gate.

## What is captured

`scripts/phase2/renderer-platform-desktop-capture-macos.sh` builds `packages/desktop`
of the pinned Paseo reference (`5de45e208690b0efc51c59a585ae9729325a9204`) as the
reference ships it: `build:app-deps:clean`, the production web export with
`PASEO_WEB_PLATFORM=electron`, and `npm run build --workspace=@getpaseo/desktop`,
restricted to an unpacked, unsigned directory (`--dir --publish never`, no
notarization). It launches the packaged `Paseo.app`, so the main process, preload
desktop bridge, window chrome mode, `webviewTag`, and the `paseo://app` production
bundle are all present.

Isolation follows the reference packaged-app smoke test
(`packages/desktop/e2e/packaged-app-smoke.js`): a disposable `PASEO_HOME` with a
`config.json`, `PASEO_LISTEN` on a random loopback port (never 6767), a disposable
Electron userData directory and `HOME`, no relay and no MCP, and a loopback
DevTools port. The app starts its own daemon. The locale is forced to `en-US`
with `--lang=en-US`, because the machine locale is `en-GB`. The driver does not
seed storage; the app opens its own page. Each of the two captures is a fresh app
launch.

The observed browser is `Chrome/152.0.7977.76` with
`Paseo/0.10.0 ... Electron/44.2.0` in the user agent. Environment: macOS 15.7.3
build 24G419, Darwin 24.6.0, x86_64.

## Result

Both fresh launches give the same PNG, focus walk, activation records, and
accessibility tree.

| capture | SHA-256 | size |
|---|---|---|
| `original-desktop.png` | `4475943215a36a9ce5471ad4eefe8d929781cc9c30ccf92a5ad7d8f003577c39` | 1280x800 |
| `original-repeat-desktop.png` | `4475943215a36a9ce5471ad4eefe8d929781cc9c30ccf92a5ad7d8f003577c39` | 1280x800 |

The membership set for the shipped macOS app is one hash.

- Desktop mode differs from the web render in the pinned browser baseline. The
  content sits about 28 pixels lower (macOS titlebar inset), the sidebar toggle
  sits at the traffic-light position, and the open-project screen has four tiles:
  Add a project, Import session, Setup providers, and Pair device.
- Keyboard focus walk: 21 entries and the cycle completes. It includes the
  desktop-only Pair device tile and ends on `Close menu`.
- Add project (Enter on `[data-testid="open-project-submit"]`): a DOM dialog opens
  whose text names the machine host (`Stevens-MacBook-Pro-5.local`). That host
  label is machine-specific and is recorded as measured.
- Plus (Enter on `New workspace`): the app navigates from `/open-project` to `/new`.
- Accessibility tree: 189 Chromium accessibility nodes (generated node IDs removed).
  This is the Chromium tree, not the macOS AX tree.

## A race in the original

The Pair device tile renders only after the app has resolved its local daemon ID
(`packages/app/src/screens/open-project-screen.tsx`, `localServerId`). An earlier
capture that did not wait for it produced a 21-entry walk in one launch and a
20-entry walk in the next. The driver now waits for
`[data-testid="open-project-pair-device"]` in desktop mode before it captures. The
baseline is deterministic with that wait, and the wait is a readiness condition of
the gate, not masking.

## Gate conditions

The light color scheme, reduced motion, `en-US`, a `1280x800` viewport, and scale
factor 1 are fixed by the gate and shared by every runtime (Playwright page
emulation and `--lang=en-US`). The shipped app normally runs with the user's own
settings. These pins are gate conditions, not masking.

## Limits

- The accessibility record is the Chromium tree. The native macOS AX tree is not
  compared yet.
- The window is captured as web contents at `1280x800`. Native window chrome and
  the traffic lights are not part of the PNG.
- Linux and Windows baselines are in `renderer-platform-linux-gate.md` and
  `renderer-platform-windows.md`.

Reproduce with:

```text
sh scripts/phase2/renderer-platform-cef.test.sh
scripts/phase2/renderer-platform-electron-capture.sh
scripts/phase2/renderer-platform-desktop-capture-macos.sh
```

The first runner prepares the reference checkout that the second reuses.

## Evidence files

| file | SHA-256 |
|---|---|
| `renderer-platform-desktop-macos/environment.json` | `cbb2c94bfbe7f6afeded6c77b7d29ff984beb5a92c9bcf1a8917efedfaf82c21` |
| `renderer-platform-desktop-macos/original-desktop.json` | `b69f036623a633fa5554ebd65d79ffa3adbdb26784ae97a4b231f5b1ebce52e0` |
| `renderer-platform-desktop-macos/original-desktop.png` | `4475943215a36a9ce5471ad4eefe8d929781cc9c30ccf92a5ad7d8f003577c39` |
| `renderer-platform-desktop-macos/original-repeat-desktop.json` | `8d6d71e1abcbdb5cec33642a340de82b28132a0ac50e4d39a73f0d18431c8a12` |
| `renderer-platform-desktop-macos/original-repeat-desktop.png` | `4475943215a36a9ce5471ad4eefe8d929781cc9c30ccf92a5ad7d8f003577c39` |
| `renderer-platform-desktop-macos/original-desktop.ax.json` | `53a05f25cded0c8970e7f19f95c920337612b43e0c76daa63e75fab423e75931` |
| `renderer-platform-desktop-macos/original-repeat-desktop.ax.json` | `5ee9ef0dfbb68166fd5b8180f9552fde2a27d919f8204958740bd943cff91d65` |
