# CEF host A and Electron host B against the shipped app on Windows

Task P2-RENDERER-CEF-01. Same measurement as `renderer-platform-cef-macos.md`: host A
is CEF `152.0.7+g83ffcba` (Chromium `152.0.7977.83`), host B is the pinned Electron
`44.2.0` shell (Chromium `152.0.7977.76`), both hosting the same Dioxus 0.7.0 web
bundle, compared with the shipped Paseo desktop app (Electron `44.2.0`). This is
measured evidence, not a selection. CEF is not accepted: the hashes differ.

## Run

- Workflow: `.github/workflows/renderer-platform-windows.yml`, run https://github.com/stevengonsalvez/spocky-rust/actions/runs/37207775778, commit `4c654f2b7fef0f0ff6527ebd3bec47276f139ef1`, conclusion `success`.
- Artifact digest: `sha256:2536cde91c7cd97e857af46e0dd0e1ad5a70392997f76747a52d6407279dc2c7`.
- Runner: GitHub Actions `windows-2022` runner, Node.js 22.20.0.
- Original shipped app is `packages/desktop` built unpacked and unsigned on the runner.
- Gate conditions, recorded per capture in each JSON: light color scheme, reduced
  motion, `en-US`, `1280x800`, scale factor 1, pointer parked at 1279,799.
- The driver parks the page pointer at 1279,799. The OS pointer is not parked on Windows. The X pointer fixes (`4c654f2b`, `0f1a9e38`) apply to Linux only.
- Node.js: 22.20.0 on the Windows runner; 26.7.0 on the macOS and Linux lanes.
- Nothing is masked or normalized. Raw console messages, page errors, and the raw
  accessibility tree (`*.ax.json`) are recorded unfiltered.

## Engine equivalence: same bundle, CEF `.83` against Electron `.76`

| measure | result |
|---|---|
| PNG, CEF and Electron | `b946ee5359c52326b95b96a8469a7a00f7ac82a5ae4d767a09eca76644f5b601` |
| exact membership | yes, 0 different pixels, RMSE 0.0 |
| focus walk | equal, 20 entries |
| accessibility tree | equal, 124 nodes |
| stability (fresh process pairs) | b946ee5359c5=b946ee5359c5 (0 px), b946ee5359c5=b946ee5359c5 (0 px), 79d7dc8f171a=79d7dc8f171a (0 px) |

## CEF against the shipped desktop app

| measure | shipped app | candidate (both hosts) |
|---|---|---|
| PNG SHA-256 | `79d7dc8f171a391d3a3a7b36d1fec36d3af9cfca79002c795fa905863e5c2d65` | `b946ee5359c52326b95b96a8469a7a00f7ac82a5ae4d767a09eca76644f5b601` |
| exact membership | | no |
| different pixels / RMSE | | 38397 / 0.0306389 |
| difference box | | 574,13,1261,611 |
| focus walk | 24 entries | 20 entries, first difference at index 17 |
| accessibility nodes | 200 | 124 |

Electron host B gives the same numbers as CEF host A (`compare-electron-first.json`),
so the difference is the pilot's web-mode content, not the engine. Do not call CEF
accepted on Windows: it is outside exact membership of the shipped app.

## Limits

- The accessibility record is the Chromium tree, not the native Windows accessibility tree.
- Hover record: on Electron host B the page reports `:hover` on `html`, `body`, `div`, `main.shell` and `section.workspace` at the parked pointer; on CEF host A and the shipped app it reports none. These are non-interactive containers, the PNGs are byte-identical, so no pixel effect is measured, but the host difference is real and unexplained.
- One run per OS, each with two fresh-process captures per host.
- The pilot has no desktop platform variant (titlebar, Pair device tile, Plus
  navigation, host label); these gaps are content, tracked separately.

## Evidence files

| file | SHA-256 |
|---|---|
| `renderer-platform-windows/candidate-desktop.ax.json` | `d5e6c36c2b7074957278714e96b992e01b3cf9c2926edb1d4edba800352bae19` |
| `renderer-platform-windows/candidate-desktop.json` | `d933973e613117d7b89ac5edb3c55116057b953d3bc7efaf076707fedf375853` |
| `renderer-platform-windows/candidate-desktop.png` | `b946ee5359c52326b95b96a8469a7a00f7ac82a5ae4d767a09eca76644f5b601` |
| `renderer-platform-windows/candidate-electron-desktop.ax.json` | `1914458181352565a6fac4cecd6887db246550583890eb267e9d970ee18ffba1` |
| `renderer-platform-windows/candidate-electron-desktop.json` | `40619c3414f085f25cb76187773216df70134ba6f3f2993d9e01160bf6b22370` |
| `renderer-platform-windows/candidate-electron-desktop.png` | `b946ee5359c52326b95b96a8469a7a00f7ac82a5ae4d767a09eca76644f5b601` |
| `renderer-platform-windows/candidate-electron-repeat-desktop.ax.json` | `3d8af0969d88d70d395297b357f7c8f0534203345e8ff75ed5e7aedf1e247ef9` |
| `renderer-platform-windows/candidate-electron-repeat-desktop.json` | `f8de7ecaca9a1a764eb99046b245a3749b1257a5d28749939ae6d8ecdbe9dbea` |
| `renderer-platform-windows/candidate-electron-repeat-desktop.png` | `b946ee5359c52326b95b96a8469a7a00f7ac82a5ae4d767a09eca76644f5b601` |
| `renderer-platform-windows/candidate-repeat-desktop.ax.json` | `478f82807416652e3563cb9f24168291dd525793f34468d3639994b7b6a8fd8e` |
| `renderer-platform-windows/candidate-repeat-desktop.json` | `d773a0640d7f45bad4afaed90ce26e0ea0b0dbbf68ac3c71755fab2353f9dded` |
| `renderer-platform-windows/candidate-repeat-desktop.png` | `b946ee5359c52326b95b96a8469a7a00f7ac82a5ae4d767a09eca76644f5b601` |
| `renderer-platform-windows/candidate-stability.json` | `4fe764bae34c872e9e47212d9947e83f76b8454a77f09be5517d31b93a5227b2` |
| `renderer-platform-windows/compare-electron-first.json` | `adbfa2450a57865d139f77384a3a6ebe872a8a6d54f9f17f71b8c345a7067745` |
| `renderer-platform-windows/compare-first.json` | `ede46c64f36cd276dbc844d0addd1201d5fda5fbf381a963db604cb4bff4cc31` |
| `renderer-platform-windows/compare-repeat.json` | `8bf4c131d038cb44483088aece9f8627ab71418e4f6ad1f86eb284f99b5589ea` |
| `renderer-platform-windows/electron-stability.json` | `f260e9a0e52724eac29a732a2b6661a3be40c2defdd74d3e211aebe4b792bee6` |
| `renderer-platform-windows/engine-electron-vs-cef.json` | `8284139caa094fd22150d606906c9d5c88c7b8ab1e44f083253265f995034a22` |
| `renderer-platform-windows/original-desktop.ax.json` | `d11547b97b1a673bea56d81557510c2f3b87070bd6f07a379dafb03063ed2672` |
| `renderer-platform-windows/original-desktop.json` | `f09527a671a071fcee52762c187ef28e37d9373012d502009913558ef4e24438` |
| `renderer-platform-windows/original-desktop.png` | `79d7dc8f171a391d3a3a7b36d1fec36d3af9cfca79002c795fa905863e5c2d65` |
| `renderer-platform-windows/original-repeat-desktop.ax.json` | `66ed1d1284bc1f6e4fc263461e9195cfa680c95f70c7b5f5f36e011812f4ca73` |
| `renderer-platform-windows/original-repeat-desktop.json` | `d50301577ce89e81e8b405cd926bc1d39960cd3de42e3d3750ed2cff209c7bad` |
| `renderer-platform-windows/original-repeat-desktop.png` | `79d7dc8f171a391d3a3a7b36d1fec36d3af9cfca79002c795fa905863e5c2d65` |
| `renderer-platform-windows/original-stability.json` | `08e0ebfe626e355cfd1d1ad638a6685f08cb14bbf59acf463b663c4b5ebdbdff` |
| `renderer-platform-windows/logs/host-1-Paseo.exe.log` | `2f00a72e72b0539edceceb1fdb82f51202b0d548e4644ca4df9e407d7b2bac4c` |
| `renderer-platform-windows/logs/host-2-Paseo.exe.log` | `8113c936faa9cb2ca9b68880e1e27ca9eec1b124a070549886762242abb10523` |
| `renderer-platform-windows/logs/host-3-spocky-cef-host.exe.log` | `191c219e1cfdecb722cac130d04544677a9e3f64615e10bdbcde771c5c26754b` |
| `renderer-platform-windows/logs/host-4-spocky-cef-host.exe.log` | `641af903c4459cac5fa9fc68466e4aa64f10b1109e7a9444de604b425cf549ef` |
| `renderer-platform-windows/logs/host-5-electron.exe.log` | `d6f602ff7d4afef29688273603fd765d3d454400093f817eb819b0e5a5c24c1b` |
| `renderer-platform-windows/logs/host-6-electron.exe.log` | `14eb11cd18e5dad7e9728d857c3403abd828c3103ab12c3a87778f50a639ed7e` |
| `renderer-platform-windows/logs/http-server.log` | `5e90c7fbd5b69be0f53e65cf5dc8497dd20e27f5d34a0269e2bf571cd6d6db53` |
