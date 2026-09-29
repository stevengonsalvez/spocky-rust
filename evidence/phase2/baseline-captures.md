# Phase 2 Original Runtime Captures

Captured on 2026-09-29 from Paseo commit `5de45e208690b0efc51c59a585ae9729325a9204` in a disposable directory. The source checkout remained unchanged.

## Command

```text
scripts/phase2/capture-baselines.sh
```

The script copies pinned protocol and crypto sources, installs the exact locked runtime dependencies into a temporary directory, executes those sources with `tsx`, writes raw JSON under ignored `evidence/raw/phase2/`, and removes only its validated temporary directory.

## Raw artifacts

| Artifact | SHA-256 |
|---|---|
| `evidence/raw/phase2/pinned-wire.json` | `f7a609aced8350ae634e81176b77dd31297ca3a032bd9c52b090427b26bdfa10` |
| `evidence/raw/phase2/pinned-crypto.json` | `3ddc666cfb41731222ced4d60fd4e13a2aa16bc8127abef15814a6134763faf9` |

The wire capture contains terminal, resize, file begin, file chunk, file end, demultiplexing, and malformed-frame results. The crypto capture contains deterministic Curve25519 keys, canonical base64, the TweetNaCl shared key, an actual baseline `encrypt` bundle, and decrypted bytes. Rust tests retain byte-exact fixed vectors separately; no normalization applies to either capture.

These captures prove the original runtime inputs used by the contract pilots. They do not prove full protocol, encrypted-channel, or system parity.
