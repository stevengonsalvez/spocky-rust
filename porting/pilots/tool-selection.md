# Phase 2 Tool-Selection Pilots

No implementation tool is selected until its applicable cases pass. A retained JavaScript, browser, or native runtime requires a recorded compatibility exception.

## Host snapshot

Captured on 2026-09-29:

```text
rustc 1.94.0 (4a4ef493e 2026-03-02)
cargo 1.94.0 (85eff7c80 2026-01-15)
repository-local Rust targets: aarch64-apple-ios, aarch64-linux-android,
wasm32-unknown-unknown, x86_64-apple-darwin, x86_64-pc-windows-msvc,
x86_64-unknown-linux-gnu
Xcode iOS SDK: unavailable
Java 17: available through repository-command-local JAVA_HOME
Android SDK: platforms 35 and 36, NDK 27.1.12297006
Android AVD: ainb-api35, shutdown at capture time
adb and emulator: available through explicit SDK paths
cargo-xwin: unavailable
cross: unavailable
wasm-pack: unavailable
node: v26.7.0
npm: 11.19.0
```

These absences are test-environment facts, not compatibility exceptions or evidence that a candidate cannot work.

The platform bridge and UI pilot compile for every repository-local target above.
This proves target compilation only. It does not satisfy launch, visual,
accessibility, native adapter, packaging, or update cases.

Dioxus Android candidate evidence now includes an unsigned debug APK, AVD
launch, touch state transition, screenshots, and accessibility trees. Cold-start
performance is outside an acceptable selection state. Baseline visual comparison,
remaining desktop systems, iOS, native adapters, and delivery cases remain open,
so Dioxus is not selected.

Dioxus browser candidate evidence now includes a frozen release bundle,
WebAssembly launch, a semantic tree, and a touch state transition. Release mode
removes the debug rebuild overlay and external font request and reduces WASM to
528,846 bytes. Desktop and mobile viewport, keyboard, and reduced-motion checks
pass. Offline reload fails. Pinned-baseline visual and packaging cases remain open.

## Executable cases

| ID | Boundary | Required scenario | Evidence required before selection |
|---|---|---|---|
| `P2-UI-01` | UI renderer | Build and launch iOS, Android, browser, macOS, Windows, Linux shells | Build logs, package artifacts, launch capture, exact tool versions |
| `P2-UI-02` | UI behavior | Fixed viewport, scale, font, locale, theme, state, keyboard, touch, reduced motion | Original and candidate screenshots, recordings, accessibility trees, interaction diffs |
| `P2-BROWSER-01` | Desktop browser | Tabs, trusted automation, webview isolation, downloads, deep links | Browser-host protocol trace, visual capture, failure and restart results |
| `P2-PLUGIN-01` | Plugin runtime | Git and npm acquisition, reviewed revision, contributions, settings, update failure, recovery | Raw plugin process traffic, filesystem state, restart results |
| `P2-HUB-01` | Hub auth and data | Setup, login, account and organization authority, PGlite restart, PostgreSQL parity | HTTP and database traces, old-state fixtures, authorization failures |
| `P2-HUB-02` | Direct Hub relationship | Daemon outbound registration, permission agreement, reconnect, superseded socket | Bidirectional protocol trace and recovery state |
| `P2-RELAY-01` | Distributed relay | Ownership convergence, node loss, reroute, opaque encryption, bounded backpressure | Multi-node trace, ciphertext capture, capacity and recovery metrics |
| `P2-AUDIO-01` | Audio and speech | Capture, playback, realtime voice, STT, TTS, local model readiness | Platform recordings, permissions, latency, interruption and device-change results |
| `P2-NATIVE-01` | Native adapters | Push, camera, file picker, haptics, notifications, background and deep-link behavior | iOS and Android device or simulator evidence |
| `P2-DELIVERY-01` | Packaging and updates | Install, upgrade, failed update, rollback, uninstall, state retention | Signed or explicitly unsigned test artifacts, logs, state digests, rollback result |

## Decision rule

A candidate passes only when all required platform cases have reproducible raw evidence and no unexplained baseline difference. Missing SDKs, devices, signing identities, service credentials, or operating systems keep the case blocked. They never become a silent exclusion.
