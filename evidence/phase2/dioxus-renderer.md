# Dioxus renderer pilot

Dioxus `0.7.0` remains a candidate. It is not selected.

Integrated checks passed on 2026-09-29:

| Surface | Evidence | Result |
|---|---|---|
| Host semantic shell | two targeted tests | pass |
| Browser | `wasm32-unknown-unknown` compile | pass |
| macOS desktop | `x86_64-apple-darwin` compile | pass |
| Android mobile | `aarch64-linux-android` compile with NDK 27.1, API 35 | pass |
| iOS mobile | `aarch64-apple-ios` compile | blocked: iphoneos SDK absent |

The macOS desktop binary launched as PID 61790 from exact tmux session
`dev-paseo-ui-renderer-20260929-2328`. Orca listed the process as running.
No window was exposed through window enumeration. Accessibility-tree and screenshot
capture were unavailable because Orca Computer Use lacks macOS Accessibility and
Screen Recording permission. The exact tmux session was stopped and PID 61790 exited.

Raw launch output is retained at
`evidence/raw/phase2/dioxus-desktop-launch.log`. It is 7,735 bytes with SHA-256
`0e7f6a76e12fe18aedd3d18c3e9ededdd1847a34f40c1ae20fe4eaa691da8a48`.

No launch screenshot, accessibility tree, iOS compile, Windows or Linux launch,
APK, IPA, desktop bundle, signed artifact, install, update, or rollback evidence
exists. Compile and process-launch evidence cannot select the renderer.
