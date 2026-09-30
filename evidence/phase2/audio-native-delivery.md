# Audio, native, and delivery evidence

The model-only contract report remains reproducible with:

```text
cargo run --quiet -p paseo-audio-delivery-pilot
```

Raw output is retained at `evidence/raw/phase2/audio-native-delivery.json`.
It is 8,948 bytes with SHA-256
`4c063e077d0b4a0ca572898d615d4f5d401337c477ca90f4ff4ee1c20aba1f54`.
Two independent executions were byte-identical.

Local runtime evidence uses real processes, files, an external audio parser, and an
isolated Android Virtual Device:

```text
cargo run --quiet -p paseo-audio-delivery-pilot --bin paseo-local-runtime -- \
  --root evidence/raw/phase2/audio-native-delivery-runtime \
  --afinfo /usr/bin/afinfo \
  --adb /Users/stevengonsalvez/Library/Android/sdk/platform-tools/adb \
  --serial emulator-5580
```

`runtime-report.json` is 3,512 bytes with SHA-256
`4ce12b432dcea6e904546a606f1d0a9eae363556f6774f232f9ad3fa6bd8e976`.
It records these observed outcomes:

- macOS parsed the generated 3,244-byte WAV as mono Int16 at 16 kHz.
- A real child process preserved stdout, stderr, and exit code 7.
- Android 15 AVD `ainb-api35` executed camera, file-picker, haptic, notification,
  and background OS commands.
- Android push stayed unsupported because no test service credential was available.
- `paseo://app/` failed to resolve because the pilot APK has no matching route.
- Explicitly unsigned packages exercised install, checksum-rejected update, valid
  update, rollback, uninstall, and retained-state bytes on the real filesystem.

Eleven default integration tests and the isolated AVD integration test pass.
Formatting and clippy pass. The local evidence strengthens `P2-AUDIO-01`,
`P2-NATIVE-01`, and `P2-DELIVERY-01`, but does not complete them. Real microphone
capture, audible playback assertion, speech services, app-level native adapter
behavior, iOS runtime evidence, signed artifacts, and production updates remain
unproven. The host has no `simctl`, so iOS runtime work is environment-blocked.
