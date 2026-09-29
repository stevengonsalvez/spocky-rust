# UI renderer candidates

Captured on 2026-09-29. No renderer is selected.

| Candidate | Six-platform claim | Browser accessibility | Pilot decision |
|---|---|---|---|
| Dioxus 0.7 | Web, desktop, and mobile renderers | DOM-backed web renderer; system webview on desktop and mobile | Advance to compile pilot |
| Slint 1.17 | Desktop, Android, iOS, and WebAssembly | Official docs say screen readers are unavailable in its canvas-based web renderer | Reject for Paseo browser UI |

Dioxus remains only a candidate. Its official mobile guide requires Xcode for iOS
and Android SDK plus NDK for Android. Compilation without launch, screenshots,
accessibility trees, input evidence, and packaging artifacts cannot select it.

Primary sources:

- <https://dioxuslabs.com/learn/0.7/guides/platforms/>
- <https://dioxuslabs.com/learn/0.7/guides/platforms/mobile/>
- <https://dioxuslabs.com/learn/0.7/essentials/ui/>
- <https://docs.slint.dev/latest/docs/slint/guide/platforms/web/>
- <https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/ios/>
