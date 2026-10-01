# Audio platform qualification

Task `P2-AUDIO-01`, qualification checkpoint based on
`paseo@5de45e208690b0efc51c59a585ae9729325a9204`.

## Platform state after qualification runs

| Platform | Observed evidence | Remaining gate |
|---|---|---|
| macOS | Generated WAV parsing, system TTS, and muted playback engine execution | Real microphone, audible playback, STT, selected-app interruption and device changes |
| Linux | Integrated delivery checkpoint compiled every crate target under clippy | Selected-app audio capture and playback runtime |
| Windows | Locked all-target MSVC compile through `cargo-xwin` | Native runtime |
| Android | Historical Android 15 AVD OS-command evidence | Selected-app audio and native callback runtime |
| iOS | Command Line Tools only; iOS SDK and `simctl` unavailable | Simulator or device runtime on provisioned hardware |
| browser | Contract model only | Browser capture and playback runtime |

The bounded runner is `scripts/phase2/audio-platform-qualification.sh`. macOS
passed 10 targeted tests. Windows passed a locked all-target MSVC compile. The
committed logs have SHA-256 digests
`4320182c23e12bea4536f20f1d400fe948e33b66b5365e0860faf70309a73ea5`
and `ffd2a776b876d38cc454923dc712fb08ee1b72c692f7513443daba14dd378fc4`.
No device, emulator, signing, deployment, publication, paid service, or
production daemon operation is part of the runner.

## Residuals

Compilation is not runtime qualification. macOS muted playback proves engine
execution and artifact compatibility, not audible output. Historical Android
OS commands do not prove selected-app adapter behavior. Missing iOS hardware and
SDK remain environment facts, not compatibility exceptions.
