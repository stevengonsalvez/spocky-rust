# Relay protocol slice 1

Capabilities: CLOUD-RELAY-PROTOCOL-027, CLOUD-RELAY-E2EE-022.
Crate: `spocky-relay-protocol` (pure functions over bytes, no dependencies).

## Authority

The BEAM relay (`relay@3fc41c96c8c63f3a7109e832899cc57d473c4531`, the source the capability
matrix names for CLOUD-RELAY-PROTOCOL-027) is the authority wherever it differs from the
Cloudflare adapter in `packages/relay/src/cloudflare-adapter.ts` (paseo `5de45e2`). The crate
follows the BEAM relay on every point below. A Cloudflare fallback lane (CLOUD-RELAY-LEGACY-009)
can reuse `connection::from_query`, `query::parse_qs` and the `control` encoders, and then
inherits BEAM behavior; it must add its own differences on top, not replace these.

| Behavior | BEAM relay (followed) | Cloudflare adapter |
|---|---|---|
| Check order | upgrade first (`socket.ex:15`), then role, `serverId`, `v`, `connectionId` (`connection.ex:16-20`) | `serverId` (about `:593`), `v` (`:598`), role (`:426`), then the `426` upgrade answer (`:314`) |
| Identifier limits | `serverId` and `connectionId` at most 256 bytes (`connection.ex:39,61`) | none |
| Trim set | `String.trim`: strips U+0085, keeps U+FEFF | JS `trim`: strips U+FEFF, keeps U+0085 |
| Duplicate query keys | last wins (`socket.ex:300`) | first wins |
| `%zz`, `=x`, more than 100 keys | empty `400` | accepted |
| Invalid UTF-8 identifier | `Jason.encode!` crash, control closes `1012 Session owner moved`, client `1012 Session expired` | decoded to U+FFFD |
| Generated v2 client id | `conn_` plus 8 random bytes in lowercase hex (`connection.ex:68`) | `conn_` plus the first 16 hex digits of a UUID (`:350`) |
| `sync` id order | `Map.keys/1` of the client map (`ownership.ex:283`): sorted to 32 keys, hash order above | `Set` insertion order (`:277`) |
| Control character escape | `\u001F` (uppercase hex, Jason) | `\u001f` |
| Close codes and statuses | `1008`, `1009`, `1012`, `1013`, `503`, `409` | `1011 Control send failed` (`:305`), `404`, `/health` |
| Key order of control frames, pong shape | `type` first | same |

Details of the source-only and header parts are in "Known gaps".

## Baseline

- Source: `.baselines/relay` at `3fc41c96c8c63f3a7109e832899cc57d473c4531`, clean.
- Runtime: `elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79`
  (Elixir 1.20.2, OTP 29.0.5), Cowboy 2.17, Cowlib, Jason 1.4.5.
- The runner copies the pinned tree under `$HOME/.cache/spocky-p4-relay` (the Docker VM
  mounts `$HOME` only), never edits the source, and leaves `_build` outside Git. Every
  `docker run` has a 900 second limit and a cleanup trap that removes the exact container.

## What is compared

| Surface | Pinned side | Rust side |
|---|---|---|
| Handshake classification and key validation | `PaseoRelay.HandshakeValidation.check/2` | `handshake::check` |
| Control ping detection | `Jason.decode` plus the `%{"type" => "ping"}` match | `control::is_ping` |
| Query parsing | `:cow_qs.parse_qs/1` | `query::parse_qs` |
| Route validation, generated id | `PaseoRelay.Connection.from_query/1` | `connection::from_query` |
| Control frame bytes and `sync` order | `Jason.encode!` of the maps the Owner sends, `Map.keys/1` | `control::{sync,connected,disconnected,pong}`, `erlang_map` |
| Wire limits | `PaseoRelay.Protocol` | `limits` |
| HTTP rejections, control frames, close frames | the running relay over real sockets | `rejection::classify`, `control`, `close` |

Comparison is raw text with `diff`. Masked values, and nothing else: `generated_id` (a v2
client id the relay generated, rendered as `<generated_id>` only when it is exactly `conn_`
plus 16 lowercase hex digits) and `wall_clock` (`ts` in pong). The Rust corpus render emits
`BAD_GENERATED` unless the id it builds has that shape and the bytes the test supplied.

## Result

```text
scripts/phase4/relay-protocol-differential.sh regenerate
relay protocol differential: 3091 corpus cases and 54 live wire cases: pinned relay identical
to the fixtures, Rust identical to the pinned relay
```

The script runs four checks and fails on any difference: the committed baseline fixtures
against a fresh pinned run (a second run after `regenerate`, so it also shows determinism),
the Rust render of both corpora against the fresh pinned output, and the live-wire test
against the fresh capture. The last step is the cargo test output below.

```text
running 9 tests
test an_identifier_jason_cannot_encode_has_no_frame ... ok
test late_control_frames_are_disconnect_notices ... ok
test every_close_the_relay_sent_is_in_the_table ... ok
test control_frames_and_closes_match_the_pinned_relay_raw ... ok
test http_rejections_match_the_pinned_relay ... ok
test escaped_identifiers_encode_like_jason ... ok
test generated_connection_ids_have_the_relay_shape_and_sync_in_its_order ... ok
test a_map_that_shrinks_back_to_32_keys_lists_ids_sorted_again ... ok
test sync_frames_list_ids_in_the_pinned_relay_order_for_every_id_class ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Fixtures replay without Docker: `cargo test -p spocky-relay-protocol`.

### Corpus

- Main corpus (2,911 cases): 1,400 mutated handshake documents plus hand cases, 250 ping
  cases, 900 query strings plus hand cases, encoder cases.
- Extra corpus (180 cases):
  - 50 handshake and 13 ping cases: accepted surrogate pairs (escaped and raw), escaped
    field names and type values, a non-string first `type` with a later duplicate, `\u0000`,
    subnormals, the `1.7976931348623157e308` to `2e308` overflow boundary, a 1,100 digit
    mantissa with a negative exponent.
  - 82 `sync` rows past 32 keys for every id length class the BEAM hash loop treats
    differently: 21 bytes (`conn_` plus 16 hex, the production shape), 16, 12 to 15, 17, 24,
    31, 32, 33, 48 and 255 bytes, non-ASCII, mixed lengths; 33 to 100 keys. Four rows add a
    pair of keys that share 32, 32, 36 and 40 low hash bits, which puts them 8 to 10 levels
    deep in the trie.
  - 35 `syncdel` rows: a map built past 32 keys, then keys deleted down to 39, 33, 32, 31,
    20, 1 and 0 keys, as the Owner removes disconnected clients.
- Live wire (54 cases): eleven HTTP rejections, control sync, connected, disconnected, pong,
  replaced, data and client closes, handshake close, oversize close, JSON escaping of ids, an
  id Jason cannot encode, `sync` for eight id classes (including 33 clients), the same
  server after one and after two clients disconnect (32 keys sorted again, then 31), and a
  fixed valid handshake key. A separate capture (`relay-protocol-live-generated.tsv`) holds
  forty random generated ids unmasked, with the relay's own `connected` and `sync` frames;
  the test checks every id's shape and replays the `sync` order through the crate.

## Findings that shaped the port

- Jason map keys come out in atom-table order, so `type` precedes `connectionId`,
  `connectionIds` and `ts`. Alphabetical order is wrong.
- `Map.keys/1` of a map with more than 32 keys is hash order, not sorted. The port implements
  `erts_internal_hash` (OTP 29 MurmurHash3 style fold) and the 4-bit trie descent. A map
  that shrinks to 32 keys or fewer is a sorted flat map again: backed by the `syncdel`
  rows and by the live case that disconnects clients from a 33 client server.
- An identifier that is not valid UTF-8 crashes the Owner at `Jason.encode!`. The crate
  reports `InvalidUtf8` so the owner slice can reproduce the crash.
- `cow_qs` raises on the 101st key even when the remaining query is empty, and on an empty
  name before `=`. Cowboy answers both with an empty `400`.
- `String.trim/1` stops at invalid UTF-8; the port trims by decoded white space only.

## Known gaps

- Attacker reachable: a v2 `connectionId` is chosen by the client (up to 256 bytes). Keys
  whose 64-bit hashes are equal form a BEAM collision node, whose internal order the port
  does not reproduce (it sorts them by key). A pair costs about 2^32 hash evaluations. Trie
  depth is verified to level 10; collision nodes are not.
- Response headers are not modeled: the `426` adds `connection: close`; Cowboy adds
  `server: Cowboy` and `content-length` (the cow_qs `400` shows only `content-length`); the
  `409` carries the reroute header. The `spocky-relay` network slice owns them. Status and
  body of every rejection, including the 503 bodies `owner`, `draining` and `cluster` and
  the empty `409`, are in `rejection`; only the HTTP 400 and 426 answers were captured live.
- Seven close constants come from the Elixir source and were not produced on a socket: ingress
  capacity, data route unavailable, delivery unavailable, memory pressure, capacity
  unavailable, slow consumer and control unresponsive. They need memory pressure, ledger loss,
  a stalled owner or a slow consumer, which the flow slice (FLOW-021) reproduces. Cowboy's own
  protocol closes (`1002`, `1007`) and a crashed connection (`1011`) are not in the table.
- Network-level behavior (Cowboy fragment assembly, idle timeouts, writer deadlines) is not in
  this crate; the existing pilot evidence covers selected cases and the network slices own it.
- `spocky-relay-pilot` still carries its own simplified query and handshake parsing. It can
  switch to this crate; that file is outside this lane.
