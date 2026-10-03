# Relay protocol slice 1

Capabilities: CLOUD-RELAY-PROTOCOL-027, CLOUD-RELAY-E2EE-022.
Crate: `spocky-relay-protocol` (pure functions over bytes, no dependencies).

## Baseline

- Source: `.baselines/relay` at `3fc41c96c8c63f3a7109e832899cc57d473c4531`, clean.
- Runtime: `elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79`
  (Elixir 1.20.2, OTP 29.0.5), Cowboy 2.17, Cowlib, Jason 1.4.5.
- The runner copies the pinned tree under `$HOME/.cache/spocky-p4-relay` (the Docker VM
  mounts `$HOME` only), never edits the source, and leaves `_build` outside Git.

## What is compared

| Surface | Pinned side | Rust side |
|---|---|---|
| Handshake classification and key validation | `PaseoRelay.HandshakeValidation.check/2` | `handshake::check` |
| Control ping detection | `Jason.decode` plus the `%{"type" => "ping"}` match | `control::is_ping` |
| Query parsing | `:cow_qs.parse_qs/1` | `query::parse_qs` |
| Route validation | `PaseoRelay.Connection.from_query/1` | `connection::from_query` |
| Control frame bytes | `Jason.encode!` of the maps the Owner sends | `control::{sync,connected,disconnected,pong}` |
| Wire limits | `PaseoRelay.Protocol` | `limits` |
| HTTP rejections, control frames, close frames | the running relay over real sockets | `rejection`, `control`, `close` |

Comparison is raw text with `diff`. Masked values: `generated_id` (the random v2
client id) and `wall_clock` (`ts` in pong). Nothing else.

## Result

```text
scripts/phase4/relay-protocol-differential.sh regenerate
relay protocol differential: 2911 corpus cases and 45 live wire cases, raw text identical
```

- Corpus: 1,400 mutated handshake documents plus hand cases (escapes, duplicates, number
  limits, surrogates, BOM, invalid UTF-8, nesting of 20,000), 250 ping cases, 900 query
  strings plus hand cases (100 key limit, percent escapes, Unicode white space, non-UTF-8
  ids, 254 to 300 byte ids), and encoder cases including `sync` with 33 to 257 ids.
- Live wire: eleven HTTP rejections, control sync, connected, disconnected, pong, replaced,
  data and client close frames, handshake close, oversize close, JSON escaping of ids, and
  an id Jason cannot encode.
- Committed fixtures replay without Docker: `cargo test -p spocky-relay-protocol`.

## Findings that shaped the port

- Jason map keys come out in atom-table order, so `type` precedes `connectionId`,
  `connectionIds` and `ts`. Alphabetical order is wrong.
- `Map.keys/1` of a map with more than 32 keys is hash order, not sorted. Ported
  `erts_internal_hash` (OTP 29 MurmurHash3 style fold) and the 4-bit trie descent. A map
  that shrinks to 32 keys or fewer is a sorted flat map again (verified).
- An identifier that is not valid UTF-8 crashes the Owner at `Jason.encode!`: the control
  socket closes `1012 Session owner moved` and the client `1012 Session expired`. The crate
  reports `InvalidUtf8` so the owner slice can reproduce the crash.
- `cow_qs` raises on the 101st key even when the remaining query is empty, and on an empty
  name before `=`. Cowboy answers both with an empty `400`.
- `String.trim/1` stops at invalid UTF-8; the port trims by decoded white space only.

## Known gaps

- Keys whose 64-bit hashes are identical form a BEAM collision node. The port lists them in
  key order. No colliding pair is known.
- Network-level behavior (Cowboy fragment assembly, idle timeouts, writer deadlines) is not in
  this crate; the existing pilot evidence covers selected cases and the network slices own it.
- `spocky-relay-pilot` still carries its own simplified query and handshake parsing. It can
  switch to this crate; that file is outside this lane.
