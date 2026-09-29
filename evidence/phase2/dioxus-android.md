# Dioxus Android runtime pilot

Dioxus `0.7.0` launched on the `ainb-api35` Android AVD on 2026-09-30.
This is candidate evidence, not renderer selection or parity evidence.

## Build and launch

The repository-local Dioxus CLI reported `dioxus 0.7.0 (37e71be)`. The unsigned
debug build used Java 17, Android API 35, NDK 27.1.12297006, and the
repository-local Rust and Cargo homes:

```text
.tools/bin/dx build --android -p paseo-ui-renderer-pilot \
  --bin paseo-ui-mobile --no-default-features --features mobile \
  --bundle android --codesign false --verbose
```

The generated APK installed on `emulator-5554`. Package
`com.example.PaseoUiMobile` launched `dev.dioxus.main.MainActivity`; PID 5151
became the resumed activity. The exact emulator was shut down after capture.

## Interaction and accessibility

The first cold launch produced a temporary Android System UI responsiveness
dialog after the emulator boot. Choosing **Wait** exposed the candidate UI.
Touching Reviewer changed the visible and accessibility state to:
`Selected agent: Reviewer. Status: Waiting.`

UI Automator exposed the app WebView, text nodes, toggle buttons, workspace,
agents, and status. This proves one Android AVD launch, touch transition, and
semantic-tree path. It does not compare candidate rendering with the pinned
Paseo baseline.

## Performance observation

The captured app log contains no app crash. Cold start skipped 459 frames and
reported a 9303 ms HWUI duration. Later events skipped 176 and 62 frames, with
reported durations of 4081 ms, 1289 ms, and 851 ms. Renderer selection remains
blocked on this unexplained performance gap and the remaining platform cases.

## Retained raw evidence

Raw files remain ignored under `evidence/raw/phase2/dioxus-android/`.

| File | Bytes | SHA-256 |
|---|---:|---|
| `app-debug.apk` | 64,109,630 | `5750f006782cde352bc44477abc0acb23f9a36e934c2a67d222ab78d003938ae` |
| `app-logcat.txt` | 14,264 | `814d25b38f038bc38507724e157a789daacee812adeb1e85b02bf8c3f61d57fc` |
| `implementer-accessibility.xml` | 8,352 | `9ff9db492a0450193cfb7c755ed30a57471b7181901af79026fddc24e4ff9c44` |
| `implementer.png` | 100,425 | `6be3c8b17a9ca4af615eb0034cedfc42f6c55e01897d5db6782b528dce68793a` |
| `reviewer-accessibility.xml` | 8,349 | `823ed448dd5da3ddff6267cd57c0690e73d72e29e981a7ab4cfc15783cfd31d3` |
| `reviewer.png` | 101,829 | `3b66e6fc12250bdcffac83130c1b538505b3b2292f0ca72ae1ef68afd8c759d9` |

The CLI installation used repository-local `.tools`, `.cargo-home`, and
`.rustup`. Dioxus also populated its normal user cache under `~/.dx`, and Gradle
used `~/.gradle`; no global configuration changed. The CLI locked graph warned
about yanked `auth-git2 0.5.8`, `keccak 0.1.5`, and `spin 0.9.8`, plus a future
incompatibility in `num-bigint-dig 0.8.4`.
