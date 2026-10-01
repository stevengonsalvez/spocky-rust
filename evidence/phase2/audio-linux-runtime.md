# Linux audio runtime

Task `P2-AUDIO-01`, bounded checkpoint against
`paseo@5de45e208690b0efc51c59a585ae9729325a9204`.

`scripts/phase2/audio-linux-runtime.sh` ran in the pinned Linux image
`rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`.
The repository mount was read-only. The output mount contained generated audio
and the structured report. The exact named container used `--rm` and the runner
retained no container after exit.

## Observed runtime

- ALSA 1.2.8 played a generated 3,244-byte mono PCM16 WAV through the `null`
  playback PCM. `aplay` exited zero.
- ALSA 1.2.8 captured a generated 16,000-sample mono PCM16 stream through an
  isolated `file` capture PCM backed by `null`. `arecord` exited zero and the
  resulting 32,044-byte WAV matched every generated input sample.
- `pactl` 16.1 and `pw-cli` 0.3.65 executed version probes. These are tool
  availability observations, not PulseAudio or PipeWire server-graph runtime.
- The Linux process deadline test killed and reaped a spawned descendant process
  group.
- The targeted Linux runtime suite passed 3 tests. Locked all-target package
  clippy passed with warnings denied.

The retained report is
`evidence/phase2/audio-linux-runtime/run/linux-audio-report.json`. Its SHA-256 is
`bc1ab525d4ed05fb4ddd41f487465e7e1aa0c0d20cf3ff421f25abc6ec2f209e`.
The container log SHA-256 is
`8c26f6d81183e5af9d25855fdb2ad2b4199862a7567d5b8ef9b997fc123b55ac`.
The run metadata SHA-256 is
`0b33707ee6aba7f961974e63aafd99ff85de62ba3518546352d06efc5ff2472d`.

## Limits

This checkpoint does not prove physical microphone capture, audible output,
PulseAudio or PipeWire server graphs, speech-to-text, text-to-speech, selected-app
integration, deployment, or production behavior. It did not use port 6767.
