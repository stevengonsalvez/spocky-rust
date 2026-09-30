# Dioxus browser runtime pilot

Dioxus `0.7.0` launched its WebAssembly candidate in Orca's embedded browser on
2026-09-30. This is candidate evidence, not renderer selection or parity evidence.

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

## Browser behavior matrix

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

No accepted pinned-baseline screenshot comparison, offline recovery, baseline
browser packaging, upgrade, rollback, or uninstall evidence exists.

## Pinned baseline comparison attempt

The bounded comparison harness archived pinned Paseo commit
`5de45e208690b0efc51c59a585ae9729325a9204`, installed its lockfile, built its
browser dependencies, built the frozen Dioxus candidate, and launched both on
random loopback ports in exact named tmux sessions. Google Chrome
`154.0.8037.59` captured desktop and mobile screenshots. Cleanup stopped both
exact sessions and left port `6767` untouched.

The capture is not parity evidence. Both original viewports remained on the
startup spinner without meaningful rendered text. The candidate rendered at
both viewports and passed keyboard activation. Both runtimes failed offline
reload. The harness now rejects a capture when either runtime has no meaningful
rendered text, preventing this partial observation from passing a comparison
gate.

| File | Bytes | SHA-256 |
|---|---:|---|
| `browser-runtime-comparison.json` | 6,856 | `99030d8f8f6175182dc83f1c53b02fb808235ecfa74f5716a66d0ec1f4b217da` |
| original desktop screenshot | 9,782 | `be29da211fafbed23c602e4919e7a682609bdda6c4aff7b17e028d9719007754` |
| original mobile screenshot | 7,492 | `92c16e584bbe20a1e26493e0088fcf7638aaa22fd87258921363d72f9f3d8013` |
| candidate desktop screenshot | 32,873 | `9fb1ee737dcc495a03eb02ebffcbc6d18bb59e3e317aa48a7633ded347486376` |
| candidate mobile screenshot | 27,824 | `011da07fccee978f8df0f9956b65f2ca8debb5aba100f8e1842ea9e7f13b8da5` |

The next comparison needs an isolated seeded baseline daemon and matching
candidate state. Electron guest APIs, pixel thresholds, accessibility tree
diffs, packaging, update, rollback, and uninstall remain open.
