# Hub trigger and execution baseline enumeration

Task: `P2-HUB-TRIGGERS-01`

Pinned inputs:

- Hub `28f6c78833065fd282f9064f92a9aa61875dd359`
- Paseo `5de45e208690b0efc51c59a585ae9729325a9204`

## Offline behavior inventory

### Manual trigger intake

- Machine-authenticated manual routes reject missing and wrong credentials with `401`.
- Valid provider-namespaced payloads return `200` with the submitted delivery ID.
- Non-namespaced sources return `400`.
- Intake stores connection and resource evidence as null when omitted.
- Delivery identity is scoped by organization. Equal delivery keys in different organizations create distinct receipts.
- Replaying the same delivery in one organization resolves the original receipt and project.
- One accepted receipt creates one running trigger branch per project and configured trigger name.
- Replaying that branch returns the existing run instead of creating another run.
- A stale expected configuration version returns `409` with `configuration_changed`.
- Required output capability failure is recorded on the accepted run and creates no execution.

Sources:

- `src/triggers/manual/manual-run.test.ts`
- `src/triggers/manual/source.ts`
- `src/triggers/manual/source.test.ts`
- `src/db/trigger-acceptance.integration.test.ts`
- `src/db/memory.ts`

### Wakeup leases and unknown dispatch outcome

- Accepted runs create one pending workflow wakeup.
- A claim sets `leaseExpiresAt = now + leaseMs`.
- A second claim before expiry returns no work.
- A claim after expiry returns the same run and marks `leasedBeforeClaim` true.
- Lease release is fenced by the claimed expiry value. An expired worker cannot release a newer lease.
- Execution reservation occurs before daemon handoff.
- If dispatch crashes after reservation, the spawning execution remains durable.
- Recovery after lease expiry reuses that execution ID, re-dispatches it, and does not reserve usage again.
- Materialized trigger context is also reused rather than recomputed after this unknown outcome.

Sources:

- `src/workflows/engine.ts`
- `src/workflows/engine.test.ts`
- `src/triggers/schedule/schedule.integration.test.ts`
- `src/db/agent-executions.integration.test.ts`

### Execution records and finality

- Execution records begin in `spawning`, can become `running`, then terminal `succeeded` or `failed`.
- Terminal transitions clear the idle deadline and set completion time.
- Concurrent terminal attempts commit one complete winner, including result and Hub action.
- Later terminal attempts are no-ops and return the already committed record.
- Workflow step completion is idempotent.
- A terminal execution recovered after restart reconciles its workflow step and run.
- Stable execution identity is derived from trigger run, configuration revision, and trigger name.
- Fan-out branches have distinct stable execution identities.

Sources:

- `src/db/agent-executions.integration.test.ts`
- `src/db/machine-model.test.ts`
- `src/daemons/durable-execution.test.ts`
- `packages/server/src/server/hub/daemon-executions.ts`
- `packages/server/src/server/hub/daemon-executions.test.ts`

### GitHub webhook verification and replay

- Verification uses `X-Hub-Signature-256` with HMAC-SHA256 over the exact body bytes.
- Missing or invalid signatures return `401` before dispatch.
- An unconfigured secret returns `503` rather than accepting unsigned traffic.
- Signed malformed JSON, invalid payloads, missing required headers, missing installation IDs, and overlong headers return `400`.
- Bodies over 1,048,576 bytes return `413` before full buffering.
- Valid repository events normalize delivery, event, repository, installation, payload, and receipt time.
- Authentic unsupported events are durably dropped with `no_trigger_for_source` and return `200`.
- Valid deliveries with no handler are durably dropped with `configuration_unavailable` and return `200`.
- Duplicate delivery IDs do not dispatch twice.
- Signature hashes are stable and stored as receipt dedupe evidence.
- One accepted delivery fans out to independent configured consumers.
- Storage unavailability returns retryable `503`.

Sources:

- `src/triggers/github/webhook.ts`
- `src/triggers/github/webhook.test.ts`
- `src/db/trigger-acceptance.ts`
- `src/db/migrations/0004_webhook_signature_dedup.sql`

## Rust coverage and differential result

Every item below is exercised by `scripts/phase2/hub-triggers-original.integration.test.ts` (real baseline
code at the pinned commit, offline, in-memory database, `TZ=UTC`, generated IDs made deterministic) and
by `crates/spocky-hub-pilot/tests/hub_triggers_evidence.rs` (Rust).

- `scripts/phase2/hub-triggers-capture.sh` writes the baseline trace to
  `evidence/raw/phase2/hub-triggers-original.json`.
- `scripts/phase2/hub-triggers-compare.sh` runs the Rust trace and compares the two raw files byte for
  byte (`cmp`). Object key order, whitespace and every response body byte are part of the comparison;
  there is no key sorting and no normalization. The public API request ID is a fixed `request-1`
  header so problem bodies are deterministic, and the Rust model is given the baseline's generated run,
  revision and step run IDs (taken from the trace) so that the execution IDs it derives can be compared.
- `hub_triggers_evidence` asserts on every run that the Rust trace equals the committed baseline trace
  `evidence/phase2/hub-triggers-original.json`; it does not pass when the output variable is unset.

| Area | Cases compared |
| --- | --- |
| Manual intake (`handleManualTriggerRequest`) | 37 request cases, a 37-string `receivedAt` grid with the parsed epoch milliseconds, omitted `receivedAt`, one to three leading byte order marks, handler replay count, receipt evidence (provider, source, dropped reason, null connection and resource), cross-organization receipts |
| Manual run matching (`createManualRunProvider`) | 10 cases: matched, public delivery key, expected version current and stale, revision missing, trigger missing, actor forbidden and allowed, no user filter, wrong event trigger |
| Public manual-run response (`createPublicApi`) | 10 operation results compared as full response bodies (RFC 9457 problem JSON key order included), 401, 403, 503 authentication, invalid JSON, wrong and missing content type, one to three leading byte order marks, and an invalid UTF-8 byte inside a string |
| Runs, fan-out, leases, execution records | receipt dedupe, run per receipt, project and trigger, wakeup claim, lease claimable at exactly its expiry instant and not one millisecond before, fenced release, execution reuse after unknown outcome, step selection by step ID and ordinal, idle deadline capped by the execution and run deadlines, first terminal wins, idle deadline cleared, run success idempotent |
| Execution identity | generated run, revision and step run IDs, the derived execution IDs, and three fixed derivation vectors compared as literal UUID strings |
| GitHub webhook (`createWebhookSource`) | 43 cases: 503, 401 variants, 413 at and over the 1,048,576 byte limit, header bounds at 128 bytes, malformed JSON, invalid UTF-8, one and two leading byte order marks, non-object bodies, installation ID shapes, lifecycle events, unsupported and handlerless drops, multiple handlers and events, storage 503 and 500, replay, long and empty secrets, signature hash value |

The Rust webhook endpoint takes its acceptance boundary as a trait (`WebhookBackend`); the manual
handler is a closure. Neither production type holds counters or logs, those live in the tests.

Result of the last run of `hub-triggers-compare.sh`: `matched: true`, `comparison: byte-identical`,
`normalization: none`. Both traces have SHA-256
`b90c37caae59b1a6f55521e1693fbd1b7b58035682fd148ccba7f3b32137fcc4` (see `hub-triggers-sha256.txt`).
The local-offset path was also checked once at +05:30 by capturing with `TZ=Asia/Kolkata` and running the
Rust trace with `SPOCKY_HUB_TRIGGERS_LOCAL_OFFSET_MINUTES=330`; that run is not part of the committed
scripts.

## Remaining gaps (not covered, not claimed)

- Machine credential verification itself (`OperationAuthenticator` internals) and the Hub harness
  routes that install configuration. Only the outcome mapping is compared.
- Public manual-run request schema issues (`invalid_request` with issue paths). Only invalid JSON and
  content type are compared.
- Invocation input parsing and input filters (`trigger_filters_rejected`, rejected invocations) and the
  required-output capability failure recorded on an accepted run.
- Usage metering: the baseline does not reserve usage again on a replayed step; the Rust model has no
  meter, so only the replay path itself is compared.
- Run and step deadlines. `reserve_execution` reports `DeadlineElapsed`, but the baseline also times
  the run out; that side effect is not modelled.
- Concurrent terminal attempts, restart reconciliation of step and run, and workflow step completion
  idempotency (Postgres-backed behavior, outside the offline in-memory database).
- Durable provider receipt acceptance (`trigger-acceptance.ts`): organization and connection routing,
  signature hash storage, accept-time dedupe. The webhook differential uses the same recording stub on
  both sides, so dedupe there is a model of the boundary, not of the database.
- Postgres manual persistence differs from the in-memory database that is compared here. It stores
  null connection and resource ids on the receipt row (only the route snapshot carries them), and it
  requires the project to be `status = 'active'` as well as having an active configuration revision;
  the in-memory database checks only the revision. The Rust model follows the in-memory behavior.
- The manual request handler maps a database-unavailable error to `503` with
  `{"error":"database_unavailable"}`. The Rust model has no storage failure injection for the manual
  path, so that mapping is not compared.
- GitHub `push` repository synchronization, and every non-GitHub provider and schedule recurrence.
- `receivedAt` strings that only the baseline's legacy date parser accepts (for example `Aug 6 2026`,
  `2026/08/06`, `2026-08-06 12:00`, `2026-8-6`). Rust accepts the ISO 8601 grammar only. Date-times
  written without an offset use a fixed host offset given to the store; the baseline uses the host time
  zone including daylight saving rules.
- Non-ASCII header values and JSON numbers outside the f64 range.

## Baseline behavior preserved on purpose

- A replayed manual delivery in the in-memory database reaches the handler again as `accepted`; run
  creation, not intake, is the idempotent step. The `duplicate` branch is unreachable for manual
  receipts there.
- The manual payload requires the `payload` key. An omitted key is rejected with
  `Invalid input: expected nonoptional, received undefined`; an explicit `null` is accepted.
- `receivedAt` such as `2026-02-30` is accepted (the date rolls over), `2026-02-32` is rejected.
- The request body reader behind `Request.json()` drops up to two leading UTF-8 byte order marks (one in
  the body reader, one in the decoder): two parse, three do not. The webhook decoder drops one.
- A manual delivery for an unknown project or one without an active revision throws
  `manual project configuration unavailable` out of the request handler.
