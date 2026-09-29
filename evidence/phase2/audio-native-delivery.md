# Audio, native, and delivery pilot

Integrated pilot command:

```text
cargo run --quiet -p paseo-audio-delivery-pilot
```

Raw output is retained at `evidence/raw/phase2/audio-native-delivery.json`.
It is 8,948 bytes with SHA-256
`4c063e077d0b4a0ca572898d615d4f5d401337c477ca90f4ff4ee1c20aba1f54`.
Two independent executions were byte-identical.

Seven targeted integration tests, formatting, and clippy pass. The report covers
`P2-AUDIO-01`, `P2-NATIVE-01`, and `P2-DELIVERY-01` contract transitions.
Real-device recordings, signed packages, and production updates remain unproven.
This evidence does not establish platform parity or select an implementation tool.
