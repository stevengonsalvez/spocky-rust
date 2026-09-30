# Selected relay runtime checkpoint

Status: selected Linux process checkpoint passes locally. This closes a bounded
subset of `P2-RELAY-01`; production deployment remains unselected.

## Runtime boundary

`spocky-relay-network-node` now runs the selected checkpoint over real loopback
TCP and WebSocket listeners. Environment inputs set the cluster floor, active
WebSocket ceiling, and static peer discovery:

- `SPOCKY_RELAY_MIN_CLUSTER_SIZE`
- `SPOCKY_RELAY_MAX_WEBSOCKETS`
- `SPOCKY_RELAY_PEERS`, comma-separated `NODE=ADDRESS` entries

The runtime exposes `/health`, `/ready`, and fixed-cardinality `/metrics`
responses on the WebSocket listener. Readiness closes at capacity, during a
drain, below the live peer floor, and after bounded peer-loss detection.

## Measured behavior

The five selected-runtime tests prove:

1. V2 control receives `sync`, `connected`, and `disconnected`; client frames
   buffer until data attachment and cross unchanged in both directions.
2. Client loss closes data with `1001 Client disconnected`; duplicate data
   closes the replaced socket with `1008 Replaced by new connection` without
   leaking its admission.
3. Canonical control ping receives a timestamped pong. A full 32-frame
   pre-attach queue closes its client with `1013 Data route unavailable`.
4. Route identifiers stop at 256 bytes. Capacity rejects with HTTP `503 Relay
   connection capacity` before creating ownership, then releases on close.
5. A selected Linux child discovers its configured peer. Under 16 owned
   sessions, loss produces the bounded `Discovered`, `Available`, `Lost` trace,
   clears remote ownership, increments one peer-loss metric, and closes
   readiness.

The existing network and in-memory suites retain HTTP `409` reroute, `1012
Session owner moved`, `1013 Slow consumer`, ordered ciphertext forwarding, and
owner takeover coverage. Payload bytes remain opaque; metrics and topology
events contain only counts, node IDs, states, and sequence numbers.

## Reproduction

```sh
scripts/phase2/relay-selected-runtime.sh
```

Result: selected runtime 5/5, prior relay tests 15/15, package clippy with
warnings denied, and package formatting check pass.

## Preserved defects and residuals

- The pinned relay's missing in-VM watchdog remains unchanged.
- Rolling deployment still does not activate relay drain state.
- The 23,001-WebSocket Fly epoch and production load certification remain
  absent.
- Static peer discovery has no DNS or deployment-provider adapter.
- The selected checkpoint does not cover handshake-key validation, 32 MiB frame
  limits, control watchdog resets, ingress memory accounting, or rolling drain.
- Production service packaging, non-loopback qualification, TLS, deployment,
  and external-service evidence remain absent.
