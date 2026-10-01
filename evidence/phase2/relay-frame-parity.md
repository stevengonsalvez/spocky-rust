# Relay frame parity checkpoint

Status: escaped handshake JSON and fragmented WebSocket boundaries match the
pinned relay for the selected runtime surface.

## Pinned baseline

- Source: `/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust/.baselines/relay`
- Commit: `3fc41c96c8c63f3a7109e832899cc57d473c4531`
- Git state: clean
- Image: `elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79`
- Execution: disposable Docker copies, baseline mounted read-only, exact-name
  forced cleanup, 240 to 360 second command bounds

Direct `PaseoRelay.HandshakeValidation.check/2` capture returned:

```text
escaped_key={:accept, :e2ee_hello}
escaped_fields_and_type={:reject, :hello}
escaped_type={:reject, :e2ee_hello}
nested_escaped=:not_handshake
```

The pinned maximum fragmented-message test passed 1/1 in 30.4 seconds with a
4 GiB container cap. A disposable copy changed its two equal fragments from the
exact payload limit to two bytes over the total limit. Cowboy returned:

```text
fragmented_oversize_close={1009, ""}
```

The first combined baseline invocation used Docker's default memory and passed
the incomplete-fragment and oversized-frame cases, but its 32 MiB fragmented
delivery missed the 35 second receive deadline. The isolated 4 GiB rerun passed.
This is retained as an environment-sensitive baseline timing limitation.

## Candidate RED and GREEN

Command:

```sh
gtimeout 60s cargo test -p spocky-relay-pilot --test relay_frame_parity \
  -- --test-threads=1 --nocapture
```

RED result: 1 passed, 3 failed. The selected runtime forwarded an escaped
invalid handshake instead of closing it and used `Message too large` as the
reason for both fragmented `1009` closes.

GREEN result: 4 passed, 0 failed. Coverage proves:

1. Escaped top-level field names and handshake types are decoded before key
   validation. Accepted frames retain their exact escaped bytes.
2. Escaped nested lookalikes remain opaque and cross unchanged.
3. A fragmented message exactly at the total limit crosses unchanged while an
   interleaved ping receives its pong.
4. A fragmented data message over the total limit closes only its source with
   `1009` and an empty reason. An established healthy route still forwards
   opaque binary bytes.
5. A fragmented control message over its separate limit also closes with
   `1009` and an empty reason.

## Regression checks

```text
cargo test -p spocky-relay-pilot --test selected_runtime
10 passed, 0 failed

cargo clippy -p spocky-relay-pilot --all-targets -- -D warnings
passed

cargo fmt -p spocky-relay-pilot --check
passed
```

Only focused relay suites ran locally. Production load, TLS, deployment, and
non-loopback behavior remain outside this checkpoint.
