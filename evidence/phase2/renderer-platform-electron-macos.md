# Web renderer in pinned Electron 44.2.0 on macOS (not the desktop product)

Task P2-RENDERER-CEF-01, first macOS capture. This loads the Metro development web
build of the original into a bare Electron `44.2.0` window. It has no Paseo main
process, preload, window chrome mode, or `webviewTag`, and it is a development
bundle, so the app renders its plain-web path. It measures the web page in Chromium
`152.0.7977.76`. It is NOT the baseline for the CEF desktop gate. The gate baseline
is `renderer-platform-desktop-macos.md`, which captures the shipped desktop app.
This file stays as evidence of the web renderer only.

## What is captured

`scripts/phase2/renderer-platform-electron-capture.sh` serves the pinned Paseo
reference (`5de45e208690b0efc51c59a585ae9729325a9204`, package-lock SHA-256
`844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6`) with Metro
and an isolated disposable daemon on random non-6767 ports, as the browser gate
does. It opens the empty-project screen in Electron `44.2.0` (Chromium
`152.0.7977.76`, installed from the committed lockfile) and drives it over the
DevTools protocol with Playwright `1.58.2`. Each of the two captures runs in a
fresh Electron process.

The host is one frameless `1280x800` content-size window with no application
menu. The driver sets the viewport and scale factor 1 through Playwright page
emulation, light color scheme, reduced motion, and `en-US`, the same emulation
the Chrome baseline capture uses. Nothing is normalized, masked, or thresholded.

Environment: macOS `15.7.3` build `24G419`, Darwin `24.6.0`, x86_64
(`environment.json`).

## Result

Both fresh-process captures are the same file.

| capture | SHA-256 | size |
|---|---|---|
| `original-desktop.png` | `a5a8fed8116bcaf23026efcb247b770ad7aa9be0497e87c9e3aac08fd6893777` | 1280x800 |
| `original-repeat-desktop.png` | `a5a8fed8116bcaf23026efcb247b770ad7aa9be0497e87c9e3aac08fd6893777` | 1280x800 |

The membership set for macOS Electron `44.2.0` is therefore one hash. The focus
walk, both activations, and the accessibility tree are identical between the two
captures.

- Keyboard focus walk: 20 entries and the cycle completes. The entries equal the
  pinned Chrome browser record for the original app, field for field.
- Add project (Enter on `[data-testid="open-project-submit"]`): the Add project
  dialog opens with the three controls (`Search for directory`, `Clone from
  GitHub`, `New directory`). The dialog equals the pinned Chrome browser record.
- Plus (Enter on the `New workspace` control): the original navigates from
  `/open-project` to `/new`. The Chrome browser record never activated this
  control, so this is a new baseline observation. A candidate must navigate the
  same way.
- Accessibility tree: 170 nodes from the Chromium accessibility tree
  (`Accessibility.getFullAXTree`), with generated node IDs removed. This is the
  Chromium tree, not the macOS AX tree. The native AX comparison is a later step.

## Difference from the pinned Chrome images

The earlier browser gate pinned Google Chrome `154.0.8037.59` and two images
(`fad844b5...`, `59709577...`). Against them this Electron `152.0.7977.76`
capture differs, measured with the same unmasked method:

| image | different pixels | normalized RMSE | difference box |
|---|---|---|---|
| `original-desktop.png` (Chrome) | 12204 | 0.0189068 | 8,11,1026,781 |
| `original-repeat-desktop.png` (Chrome) | 12194 | 0.0189067 | 8,11,1026,781 |

So pixel output depends on the Chromium version even on the same OS. The
baseline for a CEF host is the Electron capture, not the Chrome images.

## Limits

- The harness loads the original renderer in Electron with no Paseo main
  process, preload, or `webviewTag`. It measures what the pinned Chromium renders
  for the original page, not the packaged Paseo desktop window.
- The window is frameless and has no menu. Native window chrome of the packaged
  app is not captured.
- Linux and Windows baselines are separate steps. CEF at the exact Chromium
  version does not exist (see `renderer-platform-cef-pin.md`).
- URLs inside the JSON contain the random Metro port, so the JSON files change
  between runs in the port number. The two JSON files in this run also differ in
  the capture name and screenshot file name. The PNG, focus walk, dialog, and
  tree content do not change.

Reproduce with:

```text
sh scripts/phase2/renderer-platform-electron-capture.test.sh
scripts/phase2/renderer-platform-electron-capture.sh
```

## Evidence files

| file | SHA-256 |
|---|---|
| `renderer-platform-electron-macos/original-desktop.png` | `a5a8fed8116bcaf23026efcb247b770ad7aa9be0497e87c9e3aac08fd6893777` |
| `renderer-platform-electron-macos/original-repeat-desktop.png` | `a5a8fed8116bcaf23026efcb247b770ad7aa9be0497e87c9e3aac08fd6893777` |
| `renderer-platform-electron-macos/original-desktop.json` | `4cd11a464eb150dffc4c3615c118d3d2a121ce92308a1a5bdae12d33e004c913` |
| `renderer-platform-electron-macos/original-repeat-desktop.json` | `40014395bb75104ec47b870415179ee368fad14edda7c6fc7fb1f2a884fdc674` |
| `renderer-platform-electron-macos/environment.json` | `e41f339833fc89d0b676910d979270ffd44731d4425bf29a4c71e7a7dc8a1ed3` |
