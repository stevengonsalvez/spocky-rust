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

Three targeted files passed 15 of 15 tests with no failures or skips:

- `embedded-persistence.integration.test.ts`: PGlite state survives restart and
  a second process cannot open the same data directory.
- `index.embedded.integration.test.ts`: first-run state, interactive claim,
  login after restart, durable runtime secret, XDG selection, invalid config,
  auth initialization failure, and storage release.
- `environment-bootstrap.integration.test.ts`: PostgreSQL bootstrap restart,
  conflicting identity rollback, missing-password rollback, concurrent start
  serialization, password-change authorization, and browser product gating.

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

This adapter is pilot code. Its deterministic session token is not production
authentication, and its four routes do not represent the complete Hub API.

## Rust PostgreSQL pilot

The candidate opens real PostgreSQL 17 storage, creates a namespaced snapshot
table, serializes same-key writes with `pg_advisory_xact_lock`, and commits each
write transactionally. A disposable `postgres:17-alpine` runtime proves restart
state plus two concurrent writers and observes revision 2 after both commits.

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
gtimeout 120 cargo test -p paseo-hub-pilot --test daemon_socket_runtime -- --nocapture
scripts/phase2/hub-postgres-runtime.sh
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
| `evidence/raw/phase2/hub-runtime-original.json` | 5,341 | `2f017881500b4b0e974ff765fec33764f7b05d2ed544310df380c346ec39c852` |
| `evidence/raw/phase2/hub-runtime-original.log` | 4,164 | `8dd2cffb029ad799124345199d235668cc38b2c0f86346b62899e2ea00d0e2fa` |
| `evidence/raw/phase2/hub-runtime-npm-ci.log` | 700 | `5eb36a06d15fab2b18a3bc0bc8b6c9070036b14f91f7915918dac6fa31c45ba2` |
| `evidence/raw/phase2/hub-runtime-rust.json` | 769 | `bc766a54fe52d1cab828122fb4a5fd8e4036eeea6cac5204b4d8f5941b337e6d` |
| `evidence/raw/phase2/hub-postgres-runtime.log` | 4,059 | `c6d5c6a538c290920719dd3ab522414a27d0f78c95ef912fa460530ac878a8bf` |
| `evidence/raw/phase2/hub-postgres-test.log` | 344 | `c9a0d55fbd4d596ea5368d552234049480db9315b1796893cbc401be0bc11a41` |

Host: macOS Darwin 24.6.0 x86_64, Rust 1.94.0, Cargo 1.94.0,
Node 26.7.0, npm 11.19.0, Docker client 29.1.3, Docker server 28.4.0.

No normalization is applied. Original JSON contains wall-clock start times,
durations, and disposable paths. The Rust JSON excludes generated paths and
times by construction.

## Remaining evidence blockers

- Candidate PGlite storage, migrations, lock behavior, crash recovery, and old
  database fixtures do not exist.
- Candidate baseline-schema PostgreSQL behavior and embedded-versus-PostgreSQL
  differential results do not exist. Snapshot transactions and advisory locking
  are candidate-only evidence.
- Original-versus-Rust HTTP status, body, cookie, and database state differential
  traces do not exist. Candidate-only packet behavior is covered.
- Selected production daemon and Hub integration does not exist. The loopback
  pilot covers both sides of their direct relationship contract.
- Account, organization, invitation, API key, and concealment parity is not
  demonstrated by the Rust candidate.
