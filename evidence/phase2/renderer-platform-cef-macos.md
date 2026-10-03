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

- macOS only. The engine comparison on Linux and Windows is still open, and a
  mismatch on any OS rules CEF out in favor of a retained Electron shell.
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
| `renderer-platform-cef-macos/candidate-desktop.json` | `8ef04660eb2d546096c7d7a916ecb021abecd5b9004736182d27f8dbf62fa806` |
| `renderer-platform-cef-macos/candidate-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-electron-desktop.json` | `0a222ef1978c148f0f84f10baf11935a031364d9fd09759fe5e6cdd25ac54070` |
| `renderer-platform-cef-macos/candidate-electron-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-electron-repeat-desktop.json` | `e395e151258b13b9008aff2544ae595362dc86b1c1e4d912fa1c80e105f84496` |
| `renderer-platform-cef-macos/candidate-electron-repeat-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-repeat-desktop.json` | `215eb9b7bf50ea5d2bc2c8ca8a1706ed57a20b14eb7d9cbbbd465ffc901b93b0` |
| `renderer-platform-cef-macos/candidate-repeat-desktop.png` | `72a51470ad9f1e53b0bf5e0262704acb184119b6ce75eb4814188f7b5d3908fe` |
| `renderer-platform-cef-macos/candidate-stability.json` | `7877b7aee1ec750b327dde6eba6d4658e652cb6e43844730a226817217aeec31` |
| `renderer-platform-cef-macos/compare-electron-first.json` | `558c967e1b8901a04ac95fd8ef8e169ad420fbc82f7389a02845266c81dcf258` |
| `renderer-platform-cef-macos/compare-first.json` | `00d92eed274bdb00f9c35e2a151d8487f4b5f8d21d1022fce4b6f12be39c3351` |
| `renderer-platform-cef-macos/compare-repeat.json` | `10ade96aea4528d14a053abd841980fbd9834546b357cdf3e40458261c456436` |
| `renderer-platform-cef-macos/electron-stability.json` | `2bf9ec96b82c9a15979189e489c0afa8a663bd35d70c74b9138d9267637ee47b` |
| `renderer-platform-cef-macos/engine-electron-vs-cef.json` | `d4d5f05a78a69135a0a8662977c527f697a73b526f2286163d2595fff4e00e45` |
