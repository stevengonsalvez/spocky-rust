# CEF pin for the Dioxus web renderer gate

Task P2-RENDERER-CEF-01, step 1. The renderer gate hosts the Dioxus web bundle
in a pinned Chromium Embedded Framework (CEF) build and compares it with the
original Paseo desktop app. The original desktop app is Electron `44.2.0`
(`packages/desktop/package.json`, Paseo reference
`5de45e208690b0efc51c59a585ae9729325a9204`).

## Electron Chromium version

Electron `44.2.0` ships Chromium `152.0.7977.76`.

- Electron release feed (`https://releases.electronjs.org/releases.json`):
  `44.2.0`, `chrome 152.0.7977.76`, `node 24.20.0`, released 2026-09-04.
- Runtime check: `electron@44.2.0` installed from npm
  (`dist.shasum d7d2fd50ee86e62777e348b551c450e4d4ba6014`) on macOS `15.7.3`
  x86_64 printed `process.versions.chrome = 152.0.7977.76`.

## No CEF build exists at that exact version

The Spotify CEF index (`https://cef-builds.spotifycdn.com/index.json`, read
2026-10-03) has no build for Chromium `152.0.7977.76`. The 152 builds on the
stable channel are:

| CEF | Chromium |
|---|---|
| `152.0.5+gb129680` | `152.0.7977.54` |
| `152.0.6+g708dc14` | `152.0.7977.83` |
| `152.0.7+g83ffcba` | `152.0.7977.83` |
| `152.0.8+g1ce985c` | `152.0.7977.134` |
| `152.0.9+g07f67cd` | `152.0.7977.134` |
| `152.0.10+g82a832e` | `152.0.7977.140` |
| `152.0.11+g026c1f4` | `152.0.7977.149` |

The nearest build above Electron is CEF `152.0.7+g83ffcba` at Chromium
`152.0.7977.83`. It is the same Chromium 152.0.7977 branch, 7 patch builds
newer. The version gap is a measured difference, not an exact match. The gate
decides by exact full-PNG hash membership, so the gap matters only if the
renders differ.

## Pinned builds

Minimal binary distributions of `152.0.7+g83ffcba+chromium-152.0.7977.83`.
Each archive matches the SHA-1 published in the CEF index.

| platform | archive | size (bytes) | SHA-256 |
|---|---|---|---|
| macosx64 | `cef_binary_152.0.7+g83ffcba+chromium-152.0.7977.83_macosx64_minimal.tar.bz2` | 137175511 | `c4c07276991f64004201282bc2237c8679444b6a38788253f10e77d72911ddd5` |
| linux64 | `cef_binary_152.0.7+g83ffcba+chromium-152.0.7977.83_linux64_minimal.tar.bz2` | 321502974 | `a75d8956901e1f91bad4f7151af1dc31aaa2d46888cd6af51adb0ecaa77f860f` |
| windows64 | `cef_binary_152.0.7+g83ffcba+chromium-152.0.7977.83_windows64_minimal.tar.bz2` | 171608842 | `2dd1b66b43af048d123341c5c2e92c2fabe3e126f9859b2b3a2a69e05826af00` |

Download base URL: `https://cef-builds.spotifycdn.com/`. The index SHA-1 values
are `94b0b83d235028d1878f2f727a7766c3aac4d238` (macosx64),
`0f64d01e1a5fe811a59f585176b45fe8d907aac8` (linux64), and
`8dc875ed92e199b1c2e2c95cabd64eb56daf4fc2` (windows64).

## Open

Original Electron captures per OS, the CEF host, and the exact-membership,
focus-walk and accessibility comparisons are later steps. CEF is not selected
by this record.
