# Hub runtime pilot

## Decision

`P2-HUB-01` remains blocked. `P2-HUB-02` now has a real loopback WebSocket pilot
for both outbound daemon and receiving Hub endpoints, but remains incomplete
until selected production runtimes integrate them. The Rust candidate still uses a
single-process JSON snapshot. It has bounded packet-level authentication HTTP
and daemon WebSocket pilots, but no PGlite, baseline relational PostgreSQL
schema, or production HTTP server. A PostgreSQL transactional snapshot pilot
now exists. No Hub implementation tool is selected.

## Proven original-runtime behavior

The capture runs source and locked dependencies from Hub commit
`28f6c78833065fd282f9064f92a9aa61875dd359`. It archives that commit into a
disposable directory, installs from `package-lock.json`, and leaves the baseline
checkout unchanged. The lockfile SHA-256 is
`1547348f61e8f305af4c3790b830ee8db120e80628b4de2eaf39254c40d40274`.

Seven targeted files passed 30 of 30 tests with no failures or skips:

- `embedded-persistence.integration.test.ts`: PGlite state survives restart and
  a second process cannot open the same data directory.
- `index.embedded.integration.test.ts`: first-run state, interactive claim,
  login after restart, durable runtime secret, XDG selection, invalid config,
  auth initialization failure, and storage release.
- `environment-bootstrap.integration.test.ts`: PostgreSQL bootstrap restart,
  conflicting identity rollback, missing-password rollback, concurrent start
  serialization, password-change authorization, and browser product gating.
- `plan-prices.test.ts`: exact lookup keys, inactive prices, missing prices, and
  ambiguity rejection.
- `public-catalog.test.ts`: active Free and paid plans, public allowance figures,
  inactive-plan filtering, template concealment, and mismatched price keys.
- `provisioning-entitlement.test.ts`: active Free stamping and conservative
  fallback when Free is missing or inactive.
- `account-state-original.integration.test.ts`: raw signed-out, password-change,
  app-setup, and active browser account payloads from one bootstrap flow.

The PostgreSQL tests use a disposable `postgres:17-alpine` container. Ryuk is
disabled because Docker cannot mount the host Colima socket into its VM. The
suite stops its exact container in `afterAll`; no `postgres:17-alpine` container
remained after capture. Port `38941` is supplied to the in-process production
runtime. Port `6767` is never used.

The auth checks exercise Fetch `Request` and `Response` handlers in process.
They prove status and body assertions in the pinned tests, not packet-level HTTP
traffic from a bound socket.

## Rust candidate trace

The deterministic candidate trace covers forced password replacement, restart,
owner and member authority, daemon enrollment idempotency, permission agreement,
session continuation, and superseded generation refusal. Two executions had the
same SHA-256.

This remains a contract model. `EmbeddedFileStore::LIMITATIONS` names the missing
database semantics, and the trace repeats them instead of claiming parity.

## Rust packet-level authentication pilot

Commit `3bd3b4d` adds a bounded HTTP/1.1 adapter and a real loopback TCP test on a
random port. The test verifies signed-out state, temporary-password sign-in,
session cookie issuance, password-change gating, password replacement, app setup,
and persisted active state after restart. It also verifies that snapshots written
before browser-session fields existed still load with defaults.

The adapter also selects an active organization through the pinned
`/api/auth/paseo/select-organization` path. Selection validates current
membership, conceals foreign organizations with 404, persists across restart,
and defaults old single-membership snapshots to the bootstrap organization.
Its deterministic session token is not production authentication, and its five
routes do not represent the complete Hub API.

## Rust account, invitation, API-key, and billing boundary pilots

The candidate now preserves organization role gates for resource management and
adds an organization-scoped API-key pilot. It generates the baseline
`paseo_pk_` shape from OS randomness, returns the secret once, stores only a
SHA-256 verifier, compares verifiers in constant time, serializes the five exact
colon-delimited scopes, distinguishes unauthorized from forbidden, updates
last-use state only after successful scope authorization, records monotonic
fixed-clock use timestamps, revokes within an organization while preserving the
first revocation timestamp, and survives snapshot restart. Owner and member behavior, secret
concealment, malformed credentials, wrong secrets, missing scopes, last-use,
revocation, timestamps, and restart pass two targeted integration tests.

The invitation pilot passes six targeted tests covering owner/member authority,
email normalization, one live credential per organization and email, manager-only
listing, flag and seat-cap denial, current-member rejection, cancel and replacement,
email-bound acceptance, one-shot replay rejection, membership creation, and snapshot
restart. It preserves the first pending role when a reinvite reuses a credential.
A fixed-clock restart proves exact-expiry rejection, seat release, and replacement.
Its rendered email matches baseline text, HTML escaping, subject, destination, and
idempotency-key shapes. Relational invitation creation and acceptance races now
pass against disposable PostgreSQL: concurrent creation reuses one live
credential, concurrent replay accepts once, and one membership remains. HTTP
cancel and accept HTTP response shapes, external mail delivery behavior, and
entitlement races remain open. The create-invitation packet path and active
account-state selection now have pinned differential evidence.

The pinned and Rust runtimes now produce matching four-state browser account
payloads. The comparison preserves both raw documents and normalizes generated
account, organization, membership, invitation, organization-slug, invitation-link,
and invitation-expiry values. All status, registration, account, organization,
role, capability, operator, creation, member, and pending-invitation semantic
values match. Candidate packet evidence also matches the pinned 201 creation
status. This does not cover invited-user admission, multi-membership selection,
cookies, or database mutations across the complete auth API.

The Resend delivery pilot passes two targeted tests. It preserves optional
configuration, trimmed `re_` key validation, required sender validation, the
official endpoint, a ten-second request bound, bearer and idempotency headers,
and the exact JSON message shape. Loopback HTTP proves successful delivery and
422 rejection without exposing the provider response body. No live Resend
request is made; provider TLS and acceptance remain open.

The billing pilot passes four targeted tests covering active public plans, Free
and paid presentation values, inactive-plan filtering, exact price lookup keys,
ambiguity rejection, and conservative Free entitlement fallback. It makes no
Stripe request and does not claim webhook, checkout, subscription, seat-report,
or portal parity. API-key relational transaction serialization and revocation
races now pass against disposable PostgreSQL. The candidate uses the pinned
table columns, constraints, unique prefix, organization ordering index, and
timestamp behavior. Revoked-first issuance is rejected; issued-first tokens are
expired by revocation.

## Rust PostgreSQL pilot

The candidate opens real PostgreSQL 17 storage, creates a namespaced snapshot
table, serializes same-key writes with `pg_advisory_xact_lock`, and commits each
write transactionally. A separate relational API-key path uses the pinned table
contract, per-key transaction advisory locks, row locks, and token invalidation.
A relational invitation path uses the pinned member and invitation tables,
partial unique index, organization and invitation transaction locks, row locks,
normalized identity binding, and one-shot status transition.
A relational session path uses the pinned session columns and active-organization
index. Selection requires current membership, updates one live user session,
conceals foreign organizations, and fails closed after membership removal.
A disposable `postgres:17-alpine` runtime proves restart state, two concurrent
snapshot writers, both API-key revocation orderings, one concurrent invitation
credential, one concurrent invitation acceptance, and membership-bound session
selection.

The container runs inside an exact named tmux session, binds a random loopback
port, and is stopped by exact container and session names. No container or tmux
session remains after capture. This is not the baseline relational schema and
does not prove baseline transaction boundaries.

## Rust direct daemon WebSocket pilot

The candidate binds a random loopback port and accepts real WebSocket upgrades.
Seven tests cover SHA-256 verifier-only credential storage, standard protocol
negotiation, the legacy no-hello path, hello/server_info permission agreement,
reconnect, generation supersession, rejection of pending requests from the old
generation, continued use of the replacement socket, current-socket-only
offline persistence, restart state, invalid credentials, revocation with close
code 4403, and a fragmented HTTP upgrade that waits for complete headers before
application polling begins.

The pinned original passed all 16 tests in `src/daemons/registry.test.ts` and
four selected relationship tests in `src/daemons/daemons.test.ts`. The latter
covered verifier privacy, legacy scope mapping, invalid and revoked reconnects,
and current-generation presence. It ran from a disposable archive with Ryuk
disabled and stopped its exact PostgreSQL containers. No container remained.

Runtime persistence failures close the candidate socket and surface from
`HubDaemonRuntime::stop`; the pilot does not report durable success after a
failed write. `DaemonOutboundController` initiates the separate direct Hub path,
negotiates `hello` and `server_info`, responds to session requests, reconnects
after generation supersession, and stops after revocation. It does not use the
relay path. This remains pilot code, not integration in a selected production
daemon or Hub server.
Lead verification repeated the fragmented upgrade and reconnect supersession
tests 10 times each. The outbound controller test also passed 10 times, followed
by all eight socket tests and package clippy.

## Reproduce

```text
sh scripts/phase2/hub-runtime-capture.test.sh
scripts/phase2/hub-runtime-capture.sh
cargo test -p paseo-hub-pilot --test runtime_evidence -- --nocapture
cargo test -p paseo-hub-pilot --test http_runtime -- --nocapture
cargo test -p paseo-hub-pilot --test session_selection
cargo test -p paseo-hub-pilot --test api_key_boundary
cargo test -p paseo-hub-pilot --test billing_boundary
cargo test -p paseo-hub-pilot --test invitation_boundary
cargo test -p paseo-hub-pilot --test email_delivery
cargo test -p paseo-hub-pilot --test relational_api_keys
cargo test -p paseo-hub-pilot --test relational_invitations
cargo test -p paseo-hub-pilot --test relational_sessions
gtimeout 120 cargo test -p paseo-hub-pilot --test daemon_socket_runtime -- --nocapture
scripts/phase2/hub-postgres-runtime.sh
scripts/phase2/hub-account-state-compare.sh
cargo run --quiet -p paseo-hub-pilot --bin hub-runtime-evidence
```

From a disposable archive of the pinned Hub after a bounded `npm ci`, reproduce
the original daemon subset with:

```text
gtimeout 120 ./node_modules/.bin/vitest run src/daemons/registry.test.ts --bail=1
PORT=38942 TESTCONTAINERS_RYUK_DISABLED=true gtimeout 600 ./node_modules/.bin/vitest run src/daemons/daemons.test.ts --bail=1 -t 'keeps the daemon credential private|maps a legacy enrollment scope|rejects invalid and revoked credentials|replaces generations safely'
```

Capture safety bounds:

- dependency install timeout: 600 seconds;
- runtime test timeout: 900 seconds;
- immutable baseline and clean tracked-tree preflight;
- disposable archive, PGlite directories, and PostgreSQL container;
- no production service, deployment, publication, paid service, or port `6767`.

## Raw evidence

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `evidence/raw/phase2/hub-runtime-original.json` | 10,581 | `79ca69c64c749664518dadb5f8d1f30b9e50c101a6d26badf8a3dcdfc8f271e6` |
| `evidence/raw/phase2/hub-runtime-original.log` | 7,634 | `dde5c49d0eb0b9875635a31814f02c8c1d0f2a8c749ec80df3567707eda30996` |
| `evidence/raw/phase2/hub-runtime-npm-ci.log` | 700 | `68514755e3e70aa5be0bbef7372ba2066f5924d09410849f48da5b90350f84da` |
| `evidence/raw/phase2/hub-account-state-original.json` | 2,826 | `9889c9d8e0389f8c6967b2f33103a929e2e289346196caa13094c1ca5e70d4d5` |
| `evidence/raw/phase2/hub-account-state-rust.json` | 2,559 | `31e29a4651cd4b6812f207226af4580caea888829c98dd1a0d0f39d105ba6024` |
| `evidence/raw/phase2/hub-account-state-comparison.json` | 5,608 | `4f5eaf2e7ee627a45765d7c80531be4a17964357aa738c75dc0eb1ea52033931` |
| `evidence/raw/phase2/hub-runtime-rust.json` | 769 | `bc766a54fe52d1cab828122fb4a5fd8e4036eeea6cac5204b4d8f5941b337e6d` |
| `evidence/raw/phase2/hub-postgres-runtime.log` | 3,982 | `569f8bb19e84687470a52e7126d7c9af51bce7d40a7fff8514d357b20888e6ee` |
| `evidence/raw/phase2/hub-postgres-test.log` | 1,540 | `688ae8f25bdfc0edbcb122fb4838380bba1e84ec999aad6a727ba51c1491a751` |

Host: macOS Darwin 24.6.0 x86_64, Rust 1.94.0, Cargo 1.94.0,
Node 26.7.0, npm 11.19.0, Docker client 29.1.3, Docker server 28.4.0.

No normalization is applied to the runtime test report. Original JSON contains
wall-clock start times, durations, and disposable paths. The account-state
comparison normalizes generated identifiers, slug, invitation link, and expiry
only; both raw payloads and hashes remain available.

## Remaining evidence blockers

- Candidate PGlite storage, migrations, lock behavior, crash recovery, and old
  database fixtures do not exist.
- Candidate complete baseline-schema PostgreSQL behavior and embedded-versus-
  PostgreSQL differential results do not exist. API-key, invitation, and active-
  session subsets have relational evidence; remaining tables still use
  candidate-only snapshot evidence.
- Original-versus-Rust account-state bodies match for the four bootstrap states,
  including one pending team invitation. Broader cookie, admission,
  multi-organization, and database-state differential traces do not exist.
- Selected production daemon and Hub integration does not exist. The loopback
  pilot covers both sides of their direct relationship contract.
- Invitation entitlement races, live provider acceptance, cancel and accept
  packet traces, and complete organization HTTP traces remain open.
- Stripe catalog sync, webhook, checkout, subscription, seat-report, and portal
  boundaries remain open. The current billing pilot is offline and candidate-only.
