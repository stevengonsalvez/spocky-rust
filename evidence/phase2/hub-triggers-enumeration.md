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
code at the pinned commit, offline, in-memory database, generated IDs made deterministic) and
by `crates/spocky-hub-pilot/tests/hub_triggers_evidence.rs` (Rust). The baseline is captured twice, once
per host time zone: `TZ=UTC` and `TZ=Europe/London`.

- `scripts/phase2/hub-triggers-capture.sh` writes the two baseline traces to
  `evidence/raw/phase2/hub-triggers-original.json` (UTC) and
  `evidence/raw/phase2/hub-triggers-original-europe-london.json`. The committed copies are
  `evidence/phase2/hub-triggers-original.json` and
  `evidence/phase2/hub-triggers-original-europe-london.json`.
- The capture installs the pinned `package-lock.json` with `npm ci --offline`: every package comes from
  the local npm cache and is checked against the lock's integrity hashes, with no network access. The
  only online path is an explicit opt-in, `SPOCKY_HUB_TRIGGERS_ALLOW_ONLINE_INSTALL=1`, which writes an
  `ONLINE INSTALL` line to `evidence/raw/phase2/hub-triggers-npm-ci.log`. The committed captures were
  taken with the offline install.
- `scripts/phase2/hub-triggers-compare.sh` first compares each raw original capture with the committed
  evidence file (`cmp`; `SPOCKY_HUB_TRIGGERS_RECAPTURE=1` reruns the capture first). It then runs the
  Rust trace with `TZ` set to the capture's zone and compares the two raw files byte for byte (`cmp`).
  Object key order, whitespace and every response body byte are part of the comparison; there is no
  key sorting and no normalization. The public API request ID is a fixed `request-1` header so problem
  bodies are deterministic. The generated run, revision and step run IDs of the capture come from a
  counter-based UUID mock; the Rust side hardcodes the same literal values and passes them through the
  `run_id` and `step_run_ids` fields of `AcceptedRunInput`, so the execution IDs it derives are
  compared without reading the baseline trace for them.
- `hub_triggers_evidence` has three tests. Two build the store in an explicit zone (UTC and
  Europe/London) and require byte equality with the committed capture for that zone. The third builds
  the store as production does, `TriggerStore::default()`, which reads the host zone (`TZ`, else
  `/etc/localtime`) and requires byte equality with the capture for that zone (a fresh capture when
  `SPOCKY_HUB_TRIGGERS_BASELINE` is set). None of them passes when an output variable is unset. On a
  host whose zone matches neither capture the third test compares against a store built from that
  same zone. A mismatch reports both line counts and the first differing line.

| Area | Cases compared |
| --- | --- |
| Manual intake (`handleManualTriggerRequest`) | 37 request cases, a 47-string `receivedAt` grid with the parsed epoch milliseconds (including local date-times around the 2026 Europe/London daylight saving transitions and one in 1800), omitted `receivedAt`, one to three leading byte order marks, handler replay count, receipt evidence (provider, source, dropped reason, null connection and resource), cross-organization receipts |
| Manual run matching (`createManualRunProvider`) | 10 cases: matched, public delivery key, expected version current and stale, revision missing, trigger missing, actor forbidden and allowed, no user filter, wrong event trigger |
| Public manual-run response (`createPublicApi`) | 10 operation results compared as full response bodies (RFC 9457 problem JSON key order included), 401, 403, 503 authentication, invalid JSON, wrong and missing content type, one to three leading byte order marks, and an invalid UTF-8 byte inside a string |
| Runs, fan-out, leases, execution records | receipt dedupe, run per receipt, project and trigger, wakeup claim, lease claimable at exactly its expiry instant and not one millisecond before, fenced release, execution reuse after unknown outcome, step selection by step ID and ordinal, idle deadline capped by the execution and run deadlines (run deadline before the execution deadline, and execution deadline before the run deadline before the idle deadline), first terminal wins, idle deadline cleared, run success idempotent |
| Execution identity | generated run, revision and step run IDs, the derived execution IDs, the host offset in January and in August, and three fixed derivation vectors compared as literal UUID strings |
| GitHub webhook (`createWebhookSource`) | 43 cases: 503, 401 variants, 413 at and over the 1,048,576 byte limit, header bounds at 128 bytes, malformed JSON, invalid UTF-8, one and two leading byte order marks, non-object bodies, installation ID shapes, lifecycle events, unsupported and handlerless drops, multiple handlers and events, storage 503 and 500, replay, long and empty secrets, signature hash value |

The Rust webhook endpoint takes its acceptance boundary as a trait (`WebhookBackend`); the manual
handler is a closure. Neither production type holds counters or logs, those live in the tests.

Result of the last run of `hub-triggers-compare.sh`: `matched: true`, `comparison: byte-identical`,
`normalization: none`, for both zones, and each raw original equals its committed evidence file. Trace
SHA-256 values are in `hub-triggers-sha256.txt`. In the Europe/London capture the host offset is 0 in
January and 60 in August.

Local time zone behavior is also checked by `hub_triggers.rs` against values read from Node v26.7.0
with the zone set through `TZ`: Europe/London, Australia/Lord_Howe (30 minute change),
Australia/Sydney (daylight time across the new year), America/New_York, America/Sao_Paulo and
Pacific/Apia, including skipped and repeated local times and local mean time with seconds in the offset.

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
  `2026/08/06`, `2026-08-06 12:00`, `2026-8-6`). Rust accepts the ISO 8601 grammar only.
- Host time zone: Rust reads `TZ` (a zone name, optionally after a colon) or `/etc/localtime` from the
  TZif files of the host. A `TZ` value that is a POSIX rule string with no zoneinfo file, a version 1
  TZif file and an unreadable zone all give UTC; none of these is compared with the baseline, which
  uses ICU. Rust and ICU can disagree when the host zoneinfo and ICU data are different releases.
  `local_offset_minutes_at` drops the seconds of a local mean time offset, as `getTimezoneOffset` does.
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
