# Linux audio runtime

Task `P2-AUDIO-01`, bounded checkpoint against
`paseo@5de45e208690b0efc51c59a585ae9729325a9204`.

`scripts/phase2/audio-linux-runtime.sh` ran in the pinned Linux image
`rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`.
The repository mount was read-only. The output mount contained generated audio
and the structured report. The exact named container used `--rm` and the runner
retained no container after exit. Output is restricted to this owned evidence
root and symlinked root ancestors are rejected. Docker probes and cleanup have
hard deadlines, inspection errors fail closed, cleanup is verified before
metadata publication, and container ownership changes name only generated files.

## Observed runtime

- ALSA 1.2.8 played a generated 3,244-byte mono PCM16 WAV through the `null`
  playback PCM. `aplay` exited zero.
- ALSA 1.2.8 captured a generated 16,000-sample mono PCM16 stream through an
  isolated `file` capture PCM backed by `null`. `arecord` exited zero and the
  resulting 32,044-byte WAV matched every generated input sample.
- `pactl` 16.1 and `pw-cli` 0.3.65 executed version probes. These are tool
  availability observations, not PulseAudio or PipeWire server-graph runtime.
- The Linux process deadline test killed and reaped a spawned descendant process
  group. A second test proves the same behavior after the direct parent exits.
  A regrouped descendant can escape that group, but cannot keep the caller
  waiting on output pipes beyond the command deadline; the reader threads close
  only after the test removes its exact PID.
- The targeted Linux runtime suite passed 5 tests. Locked all-target package
  clippy passed with warnings denied.

The retained report is
`evidence/phase2/audio-linux-runtime/run/linux-audio-report.json`. Its SHA-256 is
`bc1ab525d4ed05fb4ddd41f487465e7e1aa0c0d20cf3ff421f25abc6ec2f209e`.
The container log SHA-256 is
`5bf26ea90b01add0e4ac30c5b0fcda011139ed1e336e5324dbc91809f1f4dfad`.
The run metadata SHA-256 is
`15a8dac0f4f134b337d7563ce6b1937894d5df5636b2e894b34d5e3ea6df6de4`.

## Limits

This checkpoint does not prove physical microphone capture, audible output,
PulseAudio or PipeWire server graphs, speech-to-text, text-to-speech, selected-app
integration, deployment, or production behavior. It did not use port 6767.
