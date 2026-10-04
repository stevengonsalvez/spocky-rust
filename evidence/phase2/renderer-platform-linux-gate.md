# CEF host A and Electron host B against the shipped app on Linux

Task P2-RENDERER-CEF-01. Same measurement as `renderer-platform-cef-macos.md`: host A
is CEF `152.0.7+g83ffcba` (Chromium `152.0.7977.83`), host B is the pinned Electron
`44.2.0` shell (Chromium `152.0.7977.76`), both hosting the same Dioxus 0.7.0 web
bundle, compared with the shipped Paseo desktop app (Electron `44.2.0`). This is
measured evidence, not a selection. CEF is not accepted: the hashes differ.

## Run

- Workflow: `.github/workflows/renderer-platform-linux.yml`, run https://github.com/stevengonsalvez/spocky-rust/actions/runs/37210490125, commit `0f1a9e38073b79b72811fa7c36e8bac39b773551`, conclusion `success`.
- Artifact digest: `sha256:eee144615be5e86719ce2d0e3934812e3d2a0c1385f1448e2d0e34fda12a9557`.
- Runner: GitHub Actions `ubuntu-24.04` runner, pinned Debian container run with `--network none`, 2 CPUs, 3g memory and swap (`container-state.json` HostConfig), Xvfb display.
- Original shipped app is `packages/desktop` built unpacked in the pinned container.
- Gate conditions, recorded per capture in each JSON: light color scheme, reduced
  motion, `en-US`, `1280x800`, scale factor 1, pointer parked at 1279,799.
- The real X pointer is warped to the bottom-right corner after each host window is mapped (`0f1a9e38`), so no element is hovered at capture. An earlier Linux Electron capture differed only by that hover.
- Node.js: 22.20.0 on the Windows runner; 26.7.0 on the macOS and Linux lanes.
- Nothing is masked or normalized. Raw console messages, page errors, and the raw
  accessibility tree (`*.ax.json`) are recorded unfiltered.

## Engine equivalence: same bundle, CEF `.83` against Electron `.76`

| measure | result |
|---|---|
| PNG, CEF and Electron | `673081c3c368de8094b7c91861f3dd96e0fd318de6c8e44b2842ea4b7f9dfbda` |
| exact membership | yes, 0 different pixels, RMSE 0.0 |
| focus walk | equal, 20 entries |
| accessibility tree | equal, 124 nodes |
| stability (fresh process pairs) | 673081c3c368=673081c3c368 (0 px), 673081c3c368=673081c3c368 (0 px), a1838d6efd65=a1838d6efd65 (0 px) |

## CEF against the shipped desktop app

| measure | shipped app | candidate (both hosts) |
|---|---|---|
| PNG SHA-256 | `a1838d6efd651c0670167bf33d45a1eb058d44d64e6f0acfce9f9458916703ed` | `673081c3c368de8094b7c91861f3dd96e0fd318de6c8e44b2842ea4b7f9dfbda` |
| exact membership | | no |
| different pixels / RMSE | | 29064 / 0.0158492 |
| difference box | | 806,13,1266,608 |
| focus walk | 24 entries | 20 entries, first difference at index 17 |
| accessibility nodes | 200 | 124 |

Electron host B gives the same numbers as CEF host A (`compare-electron-first.json`),
so the difference is the pilot's web-mode content, not the engine. Do not call CEF
accepted on Linux: it is outside exact membership of the shipped app.

## Limits

- The accessibility record is the Chromium tree, not the native Linux accessibility tree.
- Hover record: on Electron host B the page reports `:hover` on `html`, `body`, `div`, `main.shell` and `section.workspace` at the parked pointer; on CEF host A and the shipped app it reports none. These are non-interactive containers, the PNGs are byte-identical, so no pixel effect is measured, but the host difference is real and unexplained.
- One run per OS, each with two fresh-process captures per host.
- The pilot has no desktop platform variant (titlebar, Pair device tile, Plus
  navigation, host label); these gaps are content, tracked separately.

## Evidence files

| file | SHA-256 |
|---|---|
| `renderer-platform-linux-gate/candidate-desktop.ax.json` | `e1c1678982ed8a4049e4c21cfdac9f65f0072667c11e0256136f4c967dc98518` |
| `renderer-platform-linux-gate/candidate-desktop.json` | `bba04ab65d0fb876dd3b355ad5014905629f13a096d5791607e549080a37c3db` |
| `renderer-platform-linux-gate/candidate-desktop.png` | `673081c3c368de8094b7c91861f3dd96e0fd318de6c8e44b2842ea4b7f9dfbda` |
| `renderer-platform-linux-gate/candidate-electron-desktop.ax.json` | `1f269d0645ac31a788498c1385f2cc5abf4ee771311559078ef49901a42f65c4` |
| `renderer-platform-linux-gate/candidate-electron-desktop.json` | `61c690d1864d652552f37c92942d143ac72efc41c05349cf3f46d88c778621b0` |
| `renderer-platform-linux-gate/candidate-electron-desktop.png` | `673081c3c368de8094b7c91861f3dd96e0fd318de6c8e44b2842ea4b7f9dfbda` |
| `renderer-platform-linux-gate/candidate-electron-repeat-desktop.ax.json` | `9454b76a9fd18f0ae20d9fffe0ce6d655c1aa2cf8c6e9348115f20151214b114` |
| `renderer-platform-linux-gate/candidate-electron-repeat-desktop.json` | `0034eeae10141616f749d55ec1f83073b81eb6ae56de2affaf58878e34908326` |
| `renderer-platform-linux-gate/candidate-electron-repeat-desktop.png` | `673081c3c368de8094b7c91861f3dd96e0fd318de6c8e44b2842ea4b7f9dfbda` |
| `renderer-platform-linux-gate/candidate-repeat-desktop.ax.json` | `762abfff9562793a7ec0cd02ced581c162f19281134d368b473895959ba3686e` |
| `renderer-platform-linux-gate/candidate-repeat-desktop.json` | `39a69b6263435d2411ad2077559860b03ba9b77c8d4dc567bd9250a258e1b5ea` |
| `renderer-platform-linux-gate/candidate-repeat-desktop.png` | `673081c3c368de8094b7c91861f3dd96e0fd318de6c8e44b2842ea4b7f9dfbda` |
| `renderer-platform-linux-gate/candidate-stability.json` | `93f3006810ce70613d1dafd8294bbb6edae30af303aac451a8b091ad67ba70e8` |
| `renderer-platform-linux-gate/compare-electron-first.json` | `d5567f041e82ff0668431f71a0aac5b5b376b87f2c34a4218249039e888a8dde` |
| `renderer-platform-linux-gate/compare-first.json` | `b3227063f20be1138cf2bde54c769227684b7f9cf824df28bff1ba518c03d316` |
| `renderer-platform-linux-gate/compare-repeat.json` | `a09228834d84654939c0c555892a23d454c787917ed7d219e6a5913a6ffe45fd` |
| `renderer-platform-linux-gate/container-state.json` | `8b3e7420cdc8913b910e7b6ba19194e15290258aa7d8b8682f0cfe084c33e190` |
| `renderer-platform-linux-gate/container.log` | `7744883bb6f987981ec25a403f1e733863498d086db0d5a4d277e8b35af73732` |
| `renderer-platform-linux-gate/electron-stability.json` | `c9798bb9d1fe0685fd93f61d628aa4dc19e6c167c416102838a21faabbc71de0` |
| `renderer-platform-linux-gate/engine-electron-vs-cef.json` | `bcfc6f3e23f18bbc2c47b4859abc2a431b60374dda13ef48f65702618b96143f` |
| `renderer-platform-linux-gate/original-desktop.ax.json` | `029df3a708f1678534c14f3fa92b22a085d65481680c8c04dde1402a65ce3136` |
| `renderer-platform-linux-gate/original-desktop.json` | `cc1025f5cdd91b125ca273ec3de5e792d155f61bc80605fdc8872fc9d97d3e3e` |
| `renderer-platform-linux-gate/original-desktop.png` | `a1838d6efd651c0670167bf33d45a1eb058d44d64e6f0acfce9f9458916703ed` |
| `renderer-platform-linux-gate/original-repeat-desktop.ax.json` | `5f5de58da4a12ab69821f77394356c080c875cdbe8acf3dbc8c4897c5188e7b0` |
| `renderer-platform-linux-gate/original-repeat-desktop.json` | `1ef28aeca87e54f94d2280a01efc05eefa69ac858c41c5b0bad9100d98cf87f5` |
| `renderer-platform-linux-gate/original-repeat-desktop.png` | `a1838d6efd651c0670167bf33d45a1eb058d44d64e6f0acfce9f9458916703ed` |
| `renderer-platform-linux-gate/original-stability.json` | `c87b745dc41a39fc4fcb25cbd505ae72a2ace668f23d08a2b7f21d1cf3b10419` |
| `renderer-platform-linux-gate/logs/cef-candidate-desktop.log` | `5c627e98f1e6d49c2d0d1ab8a1dd4e143a4a6701c32e0f9b3dd50b3ed92e818d` |
| `renderer-platform-linux-gate/logs/cef-candidate-repeat-desktop.log` | `9d65cfb3b5621af48d6ba057cad2cf5c4a25133593042de886fa43f6a47cd1c8` |
| `renderer-platform-linux-gate/logs/electron-candidate-electron-desktop.log` | `2b2dd273cbae77455b20cfeba36d423b8ae2be0d87e9f469e0ad6010a4fc8e29` |
| `renderer-platform-linux-gate/logs/electron-candidate-electron-repeat-desktop.log` | `5c86502890d4998b8e16c52df452155529fa50f0515ddb3ddd345fc9ddc85d7b` |
| `renderer-platform-linux-gate/logs/http.log` | `7d7ce3b633f8f3a02862c9abd4d82d274547e1907b8c8ca9bc7dd9445e5947a9` |
| `renderer-platform-linux-gate/logs/shipped-original-desktop.log` | `76defcb144bae06c9549af17131c24779d10cc1c5616feef2ba2f9d6b656567b` |
| `renderer-platform-linux-gate/logs/shipped-original-repeat-desktop.log` | `666f6bf587235c887dc99a80f40156cbef19a3f4eb0d9bfb14bd8dd4c770cbb3` |
| `renderer-platform-linux-gate/logs/xvfb.log` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
