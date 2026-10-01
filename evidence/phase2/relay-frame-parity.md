# Relay frame parity checkpoint

Status: bounded handshake JSON classification and fragmented WebSocket
boundaries match the pinned relay for the selected runtime surface.

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
lone_surrogate_key=:not_handshake
malformed_ignored=:not_handshake
trailing_comma=:not_handshake
duplicate_type_first_ping=:not_handshake
duplicate_type_first_hello={:reject, :hello}
duplicate_key_first_valid={:accept, :hello}
duplicate_key_first_invalid={:reject, :hello}
positive_exp_overflow=:not_handshake
negative_exp_overflow=:not_handshake
integer_1023={:reject, :hello}
integer_1024={:reject, :hello}
integer_1025=:not_handshake
negative_integer_1024=:not_handshake
negative_integer_1025=:not_handshake
opaque_depth_20000=:not_handshake
```

Jason 1.4.5 rejects the whole document before handshake classification when a
string contains a lone surrogate, an ignored field is malformed, or trailing
syntax is present. Duplicate object fields retain their first value.
Non-finite float tokens invalidate the document. Integer tokens stop at 1,024
bytes, including a leading minus sign. Jason parsed a valid array nested 20,000
levels deep as opaque in 1.5 milliseconds.

The pinned maximum fragmented-message test passed 1/1 in 30.4 seconds with a
4 GiB container cap. A disposable copy changed its two equal fragments from the
exact payload limit to two bytes over the total limit. Cowboy returned:

```text
fragmented_oversize_close={1009, ""}
nonfinal_control_aggregate={:close, 1009, ""}
fragmented_route={{:close, 1009, ""}, {:close, 1001, "Client disconnected"}}
```

The control capture sent two nonfinal text fragments whose aggregate exceeded
64 KiB. Cowboy closed immediately after the second fragment without waiting for
FIN. The route capture proves the offending source receives `1009`, its paired
data socket receives `1001 Client disconnected`, and unrelated routes remain
outside that closure.

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

Review RED result: 4 passed, 2 failed. The selected runtime classified a
handshake from malformed JSON and retained an oversized nonfinal control
message while waiting for FIN.

Final-review RED result: both new focused tests failed. A 50,000-level opaque
array caused a stack-overflow abort in a disposable relay subprocess.
Jason-invalid numeric documents closed the client with `1008`.

GREEN result: 8 passed, 0 failed. Coverage proves:

1. Escaped top-level field names and handshake types are decoded before key
   validation. Accepted frames retain their exact escaped bytes.
2. Escaped nested lookalikes remain opaque and cross unchanged.
3. Whole-document JSON validation leaves lone surrogates, malformed ignored
   fields, trailing commas, and trailing syntax opaque.
4. Duplicate `type` and `key` fields use the first value, matching Jason 1.4.5.
5. A fragmented message exactly at the data total limit crosses unchanged
   while an interleaved ping receives its pong.
6. A fragmented data message over the total limit closes the offending route:
   source `1009`, paired data socket `1001`, both with pinned reasons.
7. An unrelated established route still forwards opaque binary bytes.
8. Nonfinal control fragments close with `1009` as soon as their aggregate
   exceeds the separate control limit.
9. An explicit parser stack handles a 50,000-level opaque array without native
   stack growth. The relay process and an unrelated established route survive.
10. Non-finite floats and integer tokens over Jason's 1,024-byte limit leave
    the complete handshake lookalike opaque. The exact boundary stays valid.

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
