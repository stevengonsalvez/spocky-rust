# UI renderer candidates

Captured on 2026-09-29. No renderer is selected.

| Candidate | Six-platform claim | Browser accessibility | Pilot decision |
|---|---|---|---|
| Dioxus 0.7 | Web, desktop, and mobile renderers | DOM-backed web renderer; system webview on desktop and mobile | Advance to compile pilot |
| Slint 1.17 | Desktop, Android, iOS, and WebAssembly | Official docs say screen readers are unavailable in its canvas-based web renderer | Reject for Paseo browser UI |

Dioxus remains only a candidate. Its official mobile guide requires Xcode for iOS
and Android SDK plus NDK for Android. Compilation without launch, screenshots,
accessibility trees, input evidence, and packaging artifacts cannot select it.

## CEF host evidence

P2-RENDERER-CEF-01 measured Dioxus web bundle in CEF `152.0.7977.83` and in the
pinned Electron `44.2.0` shell against the shipped desktop app, with exact full-PNG
SHA-256 membership on three OSes. No renderer is selected.

| OS | CEF vs Electron | Candidate vs shipped app |
|---|---|---|
| macOS | identical, 0 px | 51738 px, outside membership |
| Linux | identical, 0 px | 29064 px, outside membership |
| Windows | identical, 0 px | 38397 px, outside membership |

CEF is not accepted: the candidate differs from the shipped app on every OS, and
the Dioxus web-mode pilot lacks the desktop titlebar, Pair device tile, and Plus
navigation. See `evidence/phase2/renderer-platform-cef-macos.md`,
`renderer-platform-linux-gate.md`, `renderer-platform-windows.md`, and the
retained-host entry in `porting/compatibility-exceptions.md`.

Primary sources:

- <https://dioxuslabs.com/learn/0.7/guides/platforms/>
- <https://dioxuslabs.com/learn/0.7/guides/platforms/mobile/>
- <https://dioxuslabs.com/learn/0.7/essentials/ui/>
- <https://docs.slint.dev/latest/docs/slint/guide/platforms/web/>
- <https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/ios/>
