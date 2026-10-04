# CEF host A and Electron host B on macOS

Task P2-RENDERER-CEF-01. This measures two ways to host the Dioxus web bundle on
desktop: host A is CEF `152.0.7+g83ffcba` (Chromium `152.0.7977.83`), host B is the
pinned Electron `44.2.0` shell (Chromium `152.0.7977.76`). There is no CEF build at
`.76` (see `renderer-platform-cef-pin.md`), so the question is whether the version
gap changes the rendering. This is measured evidence, not a selection.

## Hosts

Host A is `scripts/phase2/renderer-platform-cef/`: a small Views application built
from the pinned macOS archive (SHA-256
`c4c07276991f64004201282bc2237c8679444b6a38788253f10e77d72911ddd5`). It opens one
frameless `1280x800` window with Alloy runtime style, so there is no tab strip,
toolbar, or omnibox, and it exposes a loopback DevTools port. It exits on its own
after its bound. Host B is the menu-less Electron host from the earlier baseline.
Both load the same Dioxus 0.7.0 release web bundle, served from a random loopback
port (never 6767). Neither side seeds storage.

`scripts/phase2/renderer-platform-cef-capture-macos.sh` captures each host twice
in fresh processes with the shared CDP driver and compares with
`scripts/phase2/renderer-platform-runtime-compare.py` (exact full-PNG SHA-256
membership, unmasked RMSE, field-for-field focus walk, activations, and
accessibility tree). Environment: macOS 15.7.3, x86_64, `1280x800`, scale factor 1.

## Engine equivalence: same bundle, CEF `.83` against Electron `.76`

| measure | result |
|---|---|
| PNG, CEF | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| PNG, Electron | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| exact membership | yes, 0 different pixels, RMSE 0 |
| focus walk | equal, 20 entries |
| accessibility tree | equal, 124 nodes |
| stability | two fresh CEF hosts equal, two fresh Electron hosts equal |

On macOS, CEF at Chromium `.83` renders this screen byte-identically to Electron at
`.76`. The missing exact CEF version does not change the pixels here. Linux and
Windows are not measured yet.

## Candidate against the shipped desktop app

Both hosts differ from the shipped app (`renderer-platform-desktop-macos.md`) by the
same amount, so the difference is the pilot's content, not the engine.

| measure | shipped app | candidate (both hosts) |
|---|---|---|
| PNG SHA-256 | `4475943215a36a9ce5471ad4eefe8d929781cc9c30ccf92a5ad7d8f003577c39` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| exact membership | | no |
| different pixels / RMSE | | 51738 / 0.0362863 |
| difference box | | 0,11,1026,781 |
| focus walk | 21 entries | 20 entries, first difference at index 13 |
| Plus activation | navigates to `/new` | no change |
| Add project dialog | DOM dialog naming the machine host | DOM dialog naming `isolated-baseline`, not equal |
| accessibility nodes | 189 | 124 |

The candidate is the web-mode Dioxus pilot. It has no desktop platform variant:
no macOS titlebar inset, no Pair device tile, no Plus navigation, and a fixed host
label. Those are content gaps in the Dioxus UI, which stays renderer-neutral. They
do not say anything about CEF. CEF is judged on engine equivalence per OS; the
content gaps are a separate, still open requirement for exact parity.

## Limits

- Engine equivalence holds on macOS, Linux and Windows, yet the candidate is outside
  exact membership of the shipped app on every OS, so CEF is not accepted.
- The accessibility record is the Chromium tree, not the native macOS AX tree.
- The shipped app's Add project dialog contains the machine host name, so it is
  machine-specific.

Reproduce with:

```text
sh scripts/phase2/renderer-platform-cef.test.sh
scripts/phase2/renderer-platform-cef-capture-macos.sh
```

## Evidence files

| file | SHA-256 |
|---|---|
| `renderer-platform-cef-macos/candidate-desktop.json` | `917779be43552d6d310c83ee3db17bd0aba249d21c06dc1645f022018af55765` |
| `renderer-platform-cef-macos/candidate-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-electron-desktop.json` | `5be828d0b682e298608f0599d7722e43df6438b0608504d29ae91d06572909f7` |
| `renderer-platform-cef-macos/candidate-electron-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-electron-repeat-desktop.json` | `bef8e12f44c1609244e5913feb39e6f4ec40e1c4b8f653a62a00217c7ca757f7` |
| `renderer-platform-cef-macos/candidate-electron-repeat-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-repeat-desktop.json` | `8ddff8f92ddd76203bbc883175e91ef836bee0fe71e214709cfc1d169c4cdf11` |
| `renderer-platform-cef-macos/candidate-repeat-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-stability.json` | `7877b7aee1ec750b327dde6eba6d4658e652cb6e43844730a226817217aeec31` |
| `renderer-platform-cef-macos/compare-electron-first.json` | `b2b67f18a46a1f88534d3bc141a6f352b9e838ee2c3d5349e9482207a3e41280` |
| `renderer-platform-cef-macos/compare-first.json` | `499dfad86719f36370a763cafc471e129ff2d23a4be300292dae38d6d2accaee` |
| `renderer-platform-cef-macos/compare-repeat.json` | `56c77714b7c262b39bc057d4c2af0318b44343cd8216973b239b0a0a4f26e624` |
| `renderer-platform-cef-macos/electron-stability.json` | `2bf9ec96b82c9a15979189e489c0afa8a663bd35d70c74b9138d9267637ee47b` |
| `renderer-platform-cef-macos/engine-electron-vs-cef.json` | `d4d5f05a78a69135a0a8662977c527f697a73b526f2286163d2595fff4e00e45` |
| `renderer-platform-cef-macos/candidate-desktop.ax.json` | `9b08211aad335c8d5d91aee99b7705a6d9032b90cdc2972428709d50dd2069ee` |
| `renderer-platform-cef-macos/candidate-electron-desktop.ax.json` | `e1e8c6676a366bdb526bd897cc038c13ee7f5bb96d5173b3fdc2728594f100c4` |
| `renderer-platform-cef-macos/candidate-electron-repeat-desktop.ax.json` | `cdfbd94e0feda144dc42aba195c107741a05131a58a1b69b81f8f1dfea1c4a02` |
| `renderer-platform-cef-macos/candidate-repeat-desktop.ax.json` | `a9e5c0da0436cb78ef050e3f6db83722ac9b919675fc243bb05237a2521701b5` |
