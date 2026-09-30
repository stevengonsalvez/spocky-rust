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

A second retained report adds the macOS system speech and playback path without
overwriting the Android capture:

```text
cargo run --quiet -p paseo-audio-delivery-pilot --bin paseo-local-runtime -- \
  --root evidence/raw/phase2/audio-native-delivery-runtime-12327d4 \
  --afinfo /usr/bin/afinfo
```

Its 2,415-byte `runtime-report.json` has SHA-256
`2e7ffcfea0b137c890df3bf32140565a749403b1aece642bca2b8d14cdda862e`.
macOS `say` produced a 67,356-byte mono 22,050 Hz LPCM AIFF. `afinfo` parsed the
artifact and `afplay -v 0` consumed it through the system playback engine. All
three processes exited zero. Playback was muted, so this proves engine execution
and artifact compatibility, not audible output.

`ProcessCommand` now defaults to a 30-second deadline, drains stdout and stderr
concurrently, and returns a deterministic timed-out error. Its Unix path creates
and kills an isolated process group. A real shell regression proves a timed-out
child is reaped, and a 256 KiB dual-stream regression proves pipe buffers cannot
deadlock the adapter. Windows descendant cleanup remains unproven.

The macOS delivery runtime uses real disposable `.app` bundles and executable
launches:

```text
scripts/phase2/delivery-macos-runtime.sh --output \
  /tmp/paseo-delivery-runtime-b08899c
```

The retained report at
`evidence/raw/phase2/delivery-macos-runtime-b08899c/delivery-runtime-report.json`
has SHA-256
`1d027efe978b14b1e0bf4192f790d95a94b16fba4fc98c35f38d3beeaba7b1a2`.
The log has SHA-256
`81bc88ec68ce89f817db3632e2bf98dd9616a100e2b6767d659926d3bba7cd51`.
It proves install and executable launch, corrupt-update rejection without active
bundle or state mutation, valid upgrade, rollback and launch, uninstall, and
retained external state. Every path remains under the disposable root.

Fifteen default integration tests and the isolated AVD integration test pass.
Formatting and clippy pass. The local evidence strengthens `P2-AUDIO-01`,
`P2-NATIVE-01`, and `P2-DELIVERY-01`, but does not complete them. Real microphone
capture, audible playback assertion, speech-to-text, app-level native adapter
behavior, iOS runtime evidence, signing, notarization, Gatekeeper, updater
network behavior, `quitAndInstall`, and production installation remain unproven.
The host has no `simctl`, so iOS runtime work is environment-blocked.
