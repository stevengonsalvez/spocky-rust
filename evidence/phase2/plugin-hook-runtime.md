# Plugin hook and usage runtime parity

Status: baseline contribution inventory frozen before implementation. Differential
implementation and results follow in this task.

## Provenance

- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`
- Rust base: `23a3232745bb63c79afed149439e35ffbe2b47e4`
- Baseline worker: `packages/server/src/server/plugins/plugin-process.ts`
- Hook contract and implementation:
  `packages/plugin/src/server/lifecycle.ts` and
  `packages/server/src/server/plugins/lifecycle/index.ts`
- Usage contract and implementation: `packages/plugin/src/server/usage.ts` and
  `packages/server/src/server/plugins/usage-sources/index.ts`
- Provider contract: `packages/plugin/src/server/provider.ts`

## Baseline-supported contribution shapes

This inventory was written before changing the selected Rust-owned wrapper.

### Lifecycle hooks

Event observers registered with `server.on(name, handler)`:

1. `agent.created`
2. `agent.turn_started`
3. `agent.turn_ended`
4. `agent.permission_requested`
5. `agent.permission_resolved`
6. `agent.archived`
7. `workspace.created`
8. `workspace.archived`

Request transforms registered with `server.before(name, handler)`:

1. `agent.create`
2. `agent.session_open`
3. `workspace.create`

Event handlers receive a structured clone of the event and
`{ paseo, signal }`. They execute in registration order. Individual event
handler failures are logged and swallowed, later handlers still run, and the
request result is `null`.

Before handlers receive `{ request: structuredClone(currentRequest) }` and
`{ paseo, signal }`. They execute in registration order. An `undefined` return
keeps the current request; another return replaces it after schema and mutation
boundary validation. Failures reject the invocation. `agent.session_open` may
change only `env`; `agent.create` cannot change `config.cwd`.

Each invocation has an abort controller keyed by request ID. `hook.cancel`
aborts it. Shutdown aborts active invocations and clears registrations.
Registration and removal emit `hooks.changed`; the ready frame also carries the
current event and before catalogs.

### Usage execution

`server.registerUsageSource(source)` contributes:

- `usage.identify`: parse `input` asynchronously, then call `identify(input)`;
  result is `{ key, label? }` or `null`.
- `usage.fetch`: parse `input` asynchronously, then call `fetch(input)`; result
  is a usage report.
- `usage.discover`: call optional `discover()` without an input; omission
  returns `[]`.

All results cross a JSON serialization boundary. Unknown source IDs, input
parse failures, callback failures, and non-JSON results return an `error` frame
for the same request ID. Ready metadata contains `id`, `label`, optional
sanitized icon content, and whether discovery exists.

### Provider connection plumbing

`server.registerProvider(provider)` contributes:

- `provider.catalog_key`: optional `getCatalogCacheKey(options)` returns a
  string or `undefined` through a result frame.
- `provider.connect`: calls `connect({ versions, capabilities })`, then emits
  `provider.connected` with negotiated version and capabilities, or
  `provider.connect_failed`.
- `provider.send`: calls the live connection's `send(input)`, then emits
  `provider.accepted`, or `provider.rejected`.
- Connection callbacks emit validated `provider.event` frames.
- `provider.close` unsubscribes, awaits `close()`, and emits
  `provider.closed`, including an error when close fails.

Connection IDs are unique across live and pending connections. Shutdown
tombstones pending connects, closes a late result, unsubscribes and closes live
connections, and waits for in-flight close work.

## Preserved baseline defects

- Event hook exceptions are console-only side effects. Caller receives a
  successful `null` result and cannot observe the failure.
- Unknown event hook names invoke zero handlers and return `null`; unknown
  before hook names fail.
- Hook cancellation is cooperative. Aborting a signal does not settle a
  handler that ignores it.

## Remaining scope

- Daemon RPC integration remains outside this task.
- Native Windows runtime qualification remains outside this task.
