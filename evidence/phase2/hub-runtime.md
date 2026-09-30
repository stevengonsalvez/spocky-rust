# Hub runtime pilot

## Decision

`P2-HUB-01` and `P2-HUB-02` remain blocked. The pinned original Hub runtime is
locally executable, but the Rust candidate still uses a single-process JSON
snapshot. It has no PGlite, PostgreSQL, HTTP server, or direct daemon WebSocket.
No Hub implementation tool is selected.

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

## Reproduce

```text
sh scripts/phase2/hub-runtime-capture.test.sh
scripts/phase2/hub-runtime-capture.sh
cargo test -p paseo-hub-pilot --test runtime_evidence -- --nocapture
cargo run --quiet -p paseo-hub-pilot --bin hub-runtime-evidence
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

Host: macOS Darwin 24.6.0 x86_64, Rust 1.94.0, Cargo 1.94.0,
Node 26.7.0, npm 11.19.0, Docker client 29.1.3, Docker server 28.4.0.

No normalization is applied. Original JSON contains wall-clock start times,
durations, and disposable paths. The Rust JSON excludes generated paths and
times by construction.

## Remaining evidence blockers

- Candidate PGlite storage, migrations, lock behavior, crash recovery, and old
  database fixtures do not exist.
- Candidate PostgreSQL storage, transactions, advisory locks, concurrency, and
  embedded-versus-PostgreSQL differential results do not exist.
- Original-versus-Rust HTTP status, body, cookie, and database state traces do
  not exist.
- Real daemon outbound registration, permission agreement, reconnect, socket
  supersession, revocation, and bidirectional protocol traces do not exist.
- Account, organization, invitation, API key, and concealment parity is not
  demonstrated by the Rust candidate.
