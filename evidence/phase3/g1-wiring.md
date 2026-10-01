# Phase 3 G1 wiring: recorded deviations

Lane `p3_g1_wiring`, crate `crates/spocky-daemon-app`, 2026-10-01. This file
records behavior in the wiring crate that has no baseline counterpart, so a
reviewer can tell intended differences from parity defects.

## Rust-only safety nets

### A panicking request handler

`src/request.rs` `handle_request` wraps the dispatch call in `catch_unwind`.
A panic becomes the same frames a thrown handler error produces in
`session.ts` `handleRequest`:

1. `rpc_error` with `Request failed: <message>` and code `handler_error`.
2. `activity_log` with content `Error: <message>`.

`<message>` is the panic message when the payload is a string. Otherwise it is
the fixed text `handler panicked`, which the baseline never emits.

The baseline has no equivalent. A JavaScript handler throws an `Error` whose
own message fills these frames, and there is no panic. In Rust a panic is a
bug in a handler, not a modeled failure. This net keeps one buggy request from
taking down the connection thread that also serves `ping` and later requests.

It is unreachable in parity runs. Every slice handler returns its errors as
values, and a gate run that hit it would show an `rpc_error` the original
daemon never sends, so the gate would fail rather than hide it.

Test: `request::tests::a_panicking_handler_becomes_handler_error`.
