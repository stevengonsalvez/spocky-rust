# Dioxus browser runtime pilot

Dioxus `0.7.0` launched its WebAssembly candidate in Orca's embedded browser on
2026-09-30. The later pinned Chromium contract provides accepted browser parity
evidence for the empty-project state. It does not select the renderer.

## Build and launch

The repository-local Dioxus CLI built the browser target with the repository-local
Rust toolchain:

```text
.tools/bin/dx build --web -p paseo-ui-renderer-pilot \
  --bin paseo-ui-web --no-default-features --features web \
  --bundle web --verbose
```

An exact tmux session, `dev-paseo-ui-web-1790724591`, served the output on
`127.0.0.1:3179`. The browser tab, server, and exact tmux session were stopped
after capture. Port 6767 was untouched.

## Runtime and interaction

The document reached `complete` at a 1049 by 914 CSS-pixel viewport with device
pixel ratio 2. Navigation duration was 503 ms. No console messages were captured.
The accessibility snapshot exposed main, navigation, region, heading, list,
button, and status semantics. Clicking Reviewer changed status to:
`Selected agent: Reviewer. Status: Waiting.`

The debug bundle retained a visible Dioxus overlay saying `Your app is being
rebuilt.` This overlay survived a click. It is a selection defect until a
production bundle proves it absent. The debug WASM response transferred
17,445,606 bytes and the generated WASM file is 17,445,306 bytes. The page also
requested Inter CSS from Google Fonts. Bundle size, offline behavior, and external
font ownership remain open.

## Frozen release bundle

A `--release --frozen` build removed the rebuild overlay and external font
request. The WebAssembly file fell to 528,846 bytes, with a 529,146-byte browser
transfer. Navigation completed in 256.5 ms at the same 1049 by 914 CSS-pixel
viewport. Runtime semantics and the Reviewer state transition passed with no
console messages. This closes the debug-overlay, external-font, and debug-size
defects. Offline behavior and parity thresholds remain open.

## Historical prototype behavior matrix

The frozen release bundle was rechecked with `agent-browser` in an isolated session against an exact named tmux server on port `8288`.

- A `1280x800` viewport rendered without horizontal or vertical overflow.
- A `390x844` viewport rendered the navigation above the agent list without horizontal or vertical overflow.
- Tab focus visited workspace, Implementer, then Reviewer buttons. Enter on Reviewer produced `Selected agent: Reviewer. Status: Waiting.`
- Chromium reported `prefers-reduced-motion: reduce`; the Implementer interaction still completed.
- Offline reload failed at `chrome-error://chromewebdata/`. No offline shell or service-worker recovery exists.

The exact browser session, tmux session, and port were stopped after capture. Port `6767` was untouched.

## Retained raw evidence

Raw files remain ignored under `evidence/raw/phase2/`; build outputs remain under
ignored `target/`.

| File | Bytes | SHA-256 |
|---|---:|---|
| `dioxus-web/reviewer.png` | 73,175 | `8d47af4dfed33778a5ccb671a997265980c2cf8cdc35c10d390ab67df33e8a39` |
| `dioxus-web-server.log` | 858 | `2ea8e298153f7a1ab768470429cfbcc557f4fe5a0de6319ad5b7596bb08bd61d` |
| `index.html` | 7,365 | `c7774eb5daca637585476dc7296588839900f04b295feca4da6eeca973132771` |
| `paseo-ui-web.js` | 66,984 | `bbd902fb46e0f6e993e574639682a7004242f15f72d1096c0a5983ba192577c9` |
| `paseo-ui-web_bg.wasm` | 17,445,306 | `3d8833b6b9569f6ad570d579d291a69ab2423b06f75735f6d40efeadad7191a4` |
| `dioxus-web-release/reviewer.png` | 73,175 | `8d47af4dfed33778a5ccb671a997265980c2cf8cdc35c10d390ab67df33e8a39` |
| `dioxus-web-release-server.log` | 531 | `baddb79793331d8c7a3688979a91cb9055b411aab689efc9d22cf187c01ea474` |
| release `index.html` | 525 | `bfba25ed3e0b6b9aa15fef03671b70f5e907331c097696c5f47362d00fe4caf4` |
| release JavaScript | 62,992 | `4f1cea41295acc8878f922f83db40674cc95fbe12f119dd7888ec389322f5415` |
| release WebAssembly | 528,846 | `4bd9f8aa5949bb305eda5e4eaaba7b71999b1953fdc382c9abca4854c743a087` |
| matrix desktop screenshot | 32,873 | `9fb1ee737dcc495a03eb02ebffcbc6d18bb59e3e317aa48a7633ded347486376` |
| matrix mobile screenshot | 27,824 | `011da07fccee978f8df0f9956b65f2ca8debb5aba100f8e1842ea9e7f13b8da5` |
| matrix server log | 739 | `47e36d832d2f5d31c41c21fb350d51f88f9ed1adc6eb897ebe1050aee29773b6` |

The retained files above predate the accepted pinned-baseline comparison.
Offline recovery, browser packaging, upgrade, rollback, and uninstall evidence
remain open.

## Executable pinned baseline comparison

The bounded comparison harness archived pinned Paseo commit
`5de45e208690b0efc51c59a585ae9729325a9204`, installed its lockfile, built its
browser and server dependencies, built the frozen Dioxus candidate, and launched
both renderers plus an isolated pinned daemon on random loopback ports in exact
named tmux sessions. The original received a seeded direct host registry backed
by a disposable daemon home. HTTP and WebSocket routes to port `6767` were
blocked before navigation. Google Chrome `154.0.8037.59` captured desktop and
mobile screenshots. Cleanup stopped all three exact sessions and removed the
disposable daemon. Port `6767` remained untouched.

Both sides render the same open-project state. Desktop and mobile focus order,
element types, labels, and text match. Keyboard activation opens the same Add
Project dialog with the same label, visible text, and semantic controls. The
recorded accessibility gate compares reduced motion and the complete captured
focus cycle; it does not claim general accessibility conformance.

The candidate uses the pinned logo and vector icons, matched light tokens,
system font stack, antialiasing, layout geometry, desktop sidebar shell,
baseline text wrappers, RNW flex-wrap tiles, and scroll-content ancestry. The
accepted attempt `20261001T010653Z-80481` ran at candidate and harness commit
`500d4bcb450ba8ca8aaa05f33da46b6752b3b2e5`. Its canonical comparison SHA-256
is `72405ffc95d2918b25e0cd5e46625b694337c9e8af46d0b5a34c915ef3d037ce`.
It passes exact candidate full-PNG membership, same-page and fresh-context hash
stability, interaction, and the recorded accessibility gate in both viewports.

Screenshot comparison uses original PNG bytes without pixel normalization,
masking, RMSE, or a nonzero threshold. Desktop accepts only the two complete
upstream hashes `fad844b5...480f` and `59709577...f4dc7d`; their 19 differing
pixels are inside the History icon row. Mobile accepts only
`37ff2c27...99075`. A third hash or candidate instability fails. Both runtimes
still share the pinned offline-reload failure.

| File | Bytes | SHA-256 |
|---|---:|---|
| accepted canonical comparison JSON | not recorded | `72405ffc95d2918b25e0cd5e46625b694337c9e8af46d0b5a34c915ef3d037ce` |
| desktop accepted mode A | 49,262 | `fad844b57077bcdbed0c93db7de03e5811243049ef7b6b284dbb2a8286a6480f` |
| desktop accepted mode B | 49,270 | `597095777e1d610387667c732b7c08624e4f135a6064e1b1b739ec1342f4dc7d` |
| mobile accepted image | 27,128 | `37ff2c272ad311efe1fc2e22df94ecb75af3a5f74a47b2ee6c7b356e58d99075` |

Earlier disposable-daemon runs started default local speech-model downloads;
cleanup removed them with the daemon home. The harness now disables unrelated
speech paths before launch. Electron guest APIs, broader accessibility audits,
packaging, update, rollback, and uninstall remain open.
