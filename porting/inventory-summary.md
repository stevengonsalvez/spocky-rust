# Baseline Inventory Checkpoint

## Routing evidence

| Lane | Worker | Model | Effort | Baseline | State |
|---|---|---|---|---|---|
| daemon, protocol, state, providers, CLI | `inventory_daemon` | `gpt-5.6-sol` | medium | Paseo `5de45e208690b0efc51c59a585ae9729325a9204` | integrated |
| clients, plugins, UI, native | `inventory_clients` | `gpt-5.6-sol` | medium | Paseo `5de45e208690b0efc51c59a585ae9729325a9204` | integrated |
| Hub, relay, importer, delivery | `inventory_cloud` | `gpt-5.6-sol` | medium | all four pinned commits | integrated |

Runner spawn metadata records requested model and effort. The runner does not expose independent model-attestation output, so assignment remains a recorded evidence limitation.

## Measured reference surface

- Daemon inventory observed 475 message type literals, 79 server feature flags, and 342 compatibility tags.
- Daemon inventory counted 546 server, 63 protocol, 8 client, 88 CLI, and 7 relay test files.
- Client inventory counted 621 app unit or browser tests, 211 Playwright specifications, 53 mobile evidence artifacts, 71 desktop tests, and 22 plugin or example tests.
- Client inventory found 98 compatibility sites across app, desktop, plugin, and audio code.
- Native audio currently has no automated native tests.
- Hub source exposes 49 Drizzle migrations across PostgreSQL and embedded PGlite runtimes.
- Relay source exposes distributed ownership, reroute, bounded flow control, readiness, and load-shedding contracts.
- Importer source supports Conductor with macOS discovery and explicit cross-platform database selection.

These counts are inventory observations, not parity proof. Fixture generation records exact executed counts separately.

## Preserved baseline defects

| ID | Observed baseline behavior |
|---|---|
| GAP-001 | Protocol package remains explicitly unstable despite compatibility requirements. |
| GAP-002 | Compatibility backlog contains hundreds of tagged shims and legacy RPCs. |
| GAP-003 | Workspace-resource permission enforcement is incomplete. |
| GAP-004 | File preview accepts any daemon-readable regular file. |
| GAP-005 | HTTP and MCP remain open when daemon password is unset. |
| GAP-006 | Public service proxy bypasses daemon password. |
| GAP-007 | Forwarded authority remains client-influenced. |
| GAP-008 | Schedule overlap is rejected and restart marks active runs failed. |
| GAP-009 | Scheduled permission requests count as failed runs. |
| GAP-010 | Provider discovery is asynchronous and initially reports loading. |
| GAP-011 | Old clients hide non-legacy providers by app version. |
| GAP-012 | Some ACP providers reject non-empty MCP configuration. |
| GAP-013 | Browser automation requires a connected capable host. |
| GAP-014 | Terminal sessions are not persisted. |
| GAP-015 | The last interacting client owns PTY dimensions. |
| GAP-016 | Agent storage keys by working directory rather than workspace ID. |
| GAP-017 | Hidden subagent presentation resets after app restart. |
| GAP-018 | PID ownership is not cryptographic. |
| GAP-019 | Invalid schedule files are skipped and logged. |
| GAP-020 | An unreadable encryption keypair regenerates daemon identity. |

Do not silently repair these behaviors during parity work. A safety exception requires explicit approval and a recorded differential.

## Cloud baseline gaps

- Relay's 23,001-WebSocket Fly epoch has not run or received production certification.
- Relay has no in-VM watchdog for a wedged owner that remains in cluster membership.
- Fly rolling deployment does not activate relay drain state.
- Relay transports endpoint ciphertext but does not terminate encryption.
- Importer supports only Conductor, lacks commit-only recovery, and does not retry stale config writes.
- Billing omits invoices, tax, dunning UI, coupons, multi-currency, and alternative providers.
