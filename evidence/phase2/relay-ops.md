# Relay local operations checkpoint

Status: locally provable relay operations residuals pass without deployment.
Production Fly epoch, watchdog, DNS adapters, deployment, and rolling drain
remain outside this checkpoint.

## Baseline inventory first

Pinned input:

- Source: `/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust/.baselines/relay`
- Commit: `3fc41c96c8c63f3a7109e832899cc57d473c4531`
- Git state before each harness run: clean

The pinned relay itself does not terminate TLS. `PaseoRelay.Listener` selects
`:ranch_tcp`, `:cowboy_clear`, and HTTP only. It accepts an IP through
`PASEO_RELAY_HOST` and defaults to `127.0.0.1`. The Fly adapter binds clear HTTP
on `0.0.0.0:4000`; Fly's `[http_service]` maps `internal_port = 4000` and
`force_https = true`. Therefore Fly Proxy terminates public TLS and forwards
clear HTTP and WebSocket traffic to the relay. The relay has no certificate,
private-key, TLS-version, or cipher configuration surface.

The pinned `/metrics` response exposes these fixed families:

- Gauges: `ready`, `draining`, `active_websockets`, `active_sessions`,
  `ingress_reserved_bytes`, `inflight_delivery_bytes`,
  `backpressured_sources`, `max_frame_bytes`, and four BEAM memory gauges.
- Counters: reroutes, connection rejections, forwarded frames and bytes,
  slow-consumer disconnects, delivery timeouts, memory-pressure disconnects,
  and handshake accepted/rejected labels for both routing versions and both
  handshake types.
- Histograms: `delivery_wait_seconds` with `0.001`, `0.01`, `0.1`, `1`, `10`,
  and `+Inf` buckets; `frame_size_bytes` with `1024`, `65536`, `1048576`,
  `8388608`, `33554418`, and `+Inf` buckets.

The baseline delivery wait begins when an admitted message starts delivery and
finishes when its delivery token finishes. Its capacity ledger owns current
inflight payload bytes and blocked-source count. This checkpoint adds only the
three named residual families to the selected Rust runtime:
`spocky_relay_inflight_delivery_bytes`,
`spocky_relay_backpressured_sources`, and
`spocky_relay_delivery_wait_seconds`. Existing candidate metric families stay
unchanged.

## Implemented local boundary

`NetworkNode::bind_on` binds peer and WebSocket listeners to one explicit local
IP. `SPOCKY_RELAY_HOST` selects that address for the process binary; omitted
configuration retains loopback.

The delivery channel now carries byte count and queue timestamp metadata.
Enqueue raises inflight bytes and blocked-source count. Successful writes,
failed writes, full or disconnected channels, and dropped queued work all
reconcile through one drop-safe completion path. Completion records the pinned
delivery-wait bucket shape. Payload bytes remain opaque.

The runtime now flushes the close reply queued by Tungstenite after receiving a
peer close. This is required for the baseline's clean close handshake and does
not alter any frame classification or close reason.

`relay-ops-local.sh` creates a disposable home, CA, CA-signed one-day server
certificate, and two exact-name local TLS edge proxies. Both the pinned relay
and Rust relay stay cleartext behind those proxies. CA-verified HTTPS health and
WSS upgrade checks pass for both endpoints. All services use random non-6767
ports. Rust and TLS services run in exact-name tmux sessions. The baseline runs
in an exact-name container limited to 2 CPUs and 2 GB. Cleanup targets only
those identities.

## Non-loopback and bounded load result

Observed local interface: `192.168.1.187`. Both original and Rust health and
WebSocket clients connected through that address. The Rust focused test also
bound both listeners directly to it and forwarded an opaque frame.

The local profile is intentionally 201 sustained WebSockets and 280 admission
attempts, not the unexecuted 23,001-WebSocket Fly epoch. It fits the shared
4-CPU, 4-GB Docker budget while still exercising hundreds of sockets. Both
runtimes used a 256-WebSocket application ceiling.

| Observation | Pinned original | Rust |
| --- | ---: | ---: |
| Admission successes from 280 | 256 | 256 |
| Capacity rejections | 24 | 24 |
| Admission-run clean closes | 256 | 256 |
| Sustained active sockets | 201 | 201 |
| Frames sent / received | 5,829 / 5,829 | 5,829 / 5,829 |
| Abnormal closes | 0 | 0 |
| Ordering failures / frame loss | 0 / 0 | 0 / 0 |
| Latency p50 / p95 / p99 ms | 17 / 22 / 24 | 19 / 35 / 38 |
| Resource samples | 12 | 12 |
| Memory sample range | 149.1 to 163.9 MiB container | 2,152 to 36,596 KiB RSS |
| CPU sample range | 0.15% to 49.53% container | 0.0% to 54.0% process |

Both final metrics snapshots reported zero inflight delivery bytes and zero
backpressured sources. Focused Rust coverage forwards an attached 8 KiB payload,
requires a positive delivery histogram count, and requires both transient
gauges to reconcile to zero. Existing slow-consumer coverage retains `1013 Slow
consumer`. The bounded load did not force delivery pressure; it proves clean
steady-state backpressure accounting, not a kernel-pressure certification.

Resource numbers are distributions from the same local run but use different
accounting boundaries: Docker container for the BEAM baseline and host process
RSS for Rust. They are diagnostic, not a capacity or production performance
claim.

## Reproduction

```sh
CARGO_TARGET_DIR="$PWD/.target-relay-ops" \
  gtimeout 150s sh scripts/phase2/relay-ops-local.sh
CARGO_TARGET_DIR="$PWD/.target-relay-ops" \
  cargo test -p spocky-relay-pilot --test relay_ops -- --test-threads=1
```

Latest result: local TLS, WSS, non-loopback, 201-socket sustained load,
280-attempt admission cap, 12-sample resource distributions, and three focused
Rust tests pass.

## Preserved defects and remaining gaps

- The pinned relay's missing in-VM watchdog remains unchanged.
- The 23,001-WebSocket Fly epoch remains unexecuted and uncertified.
- Production TLS and provider-edge behavior remain undeployed and unqualified.
- Static Rust peer discovery still has no DNS or deployment-provider adapter.
- Rolling deployment still does not activate relay drain state.
- Production service packaging, deployment, publication, domains, paid
  services, and external-service mutation remain untouched.
- The local steady load does not certify kernel-pressure thresholds, physical
  network paths, or production capacity.
