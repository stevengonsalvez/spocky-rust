# Hub public API, CLI device authorization and OpenAPI baseline enumeration

Task: `P2-HUB-API-01` (capability `CLOUD-HUB-API-019`)

Pinned inputs:

- Hub `28f6c78833065fd282f9064f92a9aa61875dd359`

This document inventories what the pinned Hub source and tests define for the scoped organization
public API, CLI device authorization and the OpenAPI document, and says which parts can be observed
offline. Every statement below was read from the cited baseline source or observed by running the
pinned source with its in-memory database and stubbed operations, in a disposable copy of the
pinned commit (`git archive`, dependencies installed from the local npm cache, no network, no
database server). The sections before "Rust coverage and differential result" make no claim about Rust
code.

## Operations

`src/public-api/operation-manifest.ts` defines ten operations. Every operation requires exactly one
scope. The router in `src/public-api/index.ts` selects an operation by exact `url.pathname` and
upper-cased method.

| Operation id | Method and path | Scope | Success | Request body schema |
| --- | --- | --- | --- | --- |
| `listTriggers` | `GET /api/v1/triggers` | `configuration:validate` | 200 | none |
| `validateTrigger` | `POST /api/v1/triggers/validate` | `configuration:validate` | 200 | `TriggerYamlRequest` |
| `installTrigger` | `POST /api/v1/triggers/install` | `configuration:install` | 201 | `TriggerYamlRequest` |
| `listProjects` | `GET /api/v1/projects` | `projects:read` | 200 | none |
| `listConfigurationResources` | `GET /api/v1/configuration-resources` | `configuration:validate` | 200 | none |
| `listSetupResources` | `GET /api/v1/setup-resources` | `configuration:validate` | 200 | none |
| `validateConfiguration` | `POST /api/v1/configurations/validate` | `configuration:validate` | 200 | `InstallConfigurationRequest` |
| `installConfiguration` | `POST /api/v1/configurations/install` | `configuration:install` | 201 | `InstallConfigurationRequest` |
| `dispatchManualRun` | `POST /api/v1/manual-runs` | `runs:dispatch` | 200 | `DispatchManualRunRequest` |
| `issueEnrollmentToken` | `POST /api/v1/daemons/enrollment-tokens` | `daemons:enroll` | 201 | none |

The five scopes are `projects:read`, `configuration:validate`, `configuration:install`,
`runs:dispatch` and `daemons:enroll` (`src/auth/api-key-contract.ts`). The CLI start and poll routes
(`POST /api/v1/cli-authorizations`, `POST /api/v1/cli-authorizations/poll`) are not in the manifest.
They are separate TanStack routes that call `CliAuthorizations.start` and `CliAuthorizations.poll`;
the splat route `src/routes/api/v1/$.ts` sends every other `/api/v1/*` request, for any method, to
`publicApi.handle`. `GET /api/openapi.json` calls `publicApi.openapi()`. When the application has no
database, the four CLI authorization handlers in `src/app.ts` return 503
`{"error":"database_unavailable"}`.

## Request handling order

For a routed operation `execute` runs these steps in order:

1. A composition of `unavailable`, or no operations object, returns 503 `infrastructure_unavailable`
   before authentication. `createPublicApi` throws `enabled public API requires application
   operations` when the composition is `enabled` and operations are `null`.
2. `authenticator.authorize(request, scope)` runs. A thrown `DatabaseUnavailableError` returns 503
   `authentication_unavailable`; any other thrown error reaches the outer boundary.
3. `unauthorized` returns 401 `unauthorized` with `WWW-Authenticate: Bearer`; `forbidden` returns
   403 `insufficient_scope` with the required scope in the detail.
4. For operations with a request schema the body is read. A content type that does not contain
   `application/json` (case-insensitive substring), or a body that `Request.json()` cannot parse,
   returns 400 `invalid_json`. Then the schema runs; failure returns 400 `invalid_request` with one
   issue per schema failure.
5. The operation runs and its result is mapped to a response; the mapped success body is passed
   through the success schema, so a result that violates it (for example a non-UUID id, a
   non-positive version or `active: false` on a configuration installation) throws.
6. A thrown `DatabaseUnavailableError` anywhere returns 503 `infrastructure_unavailable`; every other
   thrown error returns 500 `internal_error` with a fixed message that carries no cause text.

Before step 1 the request identifier is `x-request-id` trimmed when non-empty, otherwise a generated
UUID. It is echoed in the `x-request-id` response header and the `requestId` problem field.

## Response bodies and status codes

Success bodies are `Response.json` of the schema-parsed value (compact JSON, schema key order,
`content-type: application/json`, `x-request-id`). Problems are RFC 9457 documents with key order
`type`, `title`, `status`, `detail`, `code`, `requestId` and an optional `issues` array. The content
type is `application/problem+json`; `type` is `https://paseo.sh/problems/` followed by the code with
underscores replaced by hyphens.

| Status | Code | Produced by |
| --- | --- | --- |
| 400 | `invalid_json` | non-JSON content type or unparseable body |
| 400 | `invalid_request` | request schema failure, with `issues` |
| 400 | `invalid_input` | manual run rejected the submitted input, with `issues` |
| 401 | `unauthorized` | credential missing, malformed, wrong, revoked; revoked before an enrollment token was issued |
| 403 | `insufficient_scope` | credential lacks the operation scope |
| 403 | `actor_forbidden` | manual run actor not allowed |
| 404 | `not_found` | no manifest path matches |
| 404 | `project_not_found`, `configuration_not_found`, `trigger_not_found` | operation results |
| 405 | `method_not_allowed` | path matches, method does not; `Allow` lists the methods |
| 409 | `configuration_changed`, `daemon_offline`, `dispatch_conflict` | manual run results |
| 422 | `invalid_trigger`, `invalid_configuration_bundle`, `invalid_configuration` | operation results, with `issues` |
| 500 | `internal_error` | any other thrown error or invalid operation result |
| 503 | `infrastructure_unavailable` | unavailable composition, database error, operation result |
| 503 | `authentication_unavailable` | authenticator reported the database unavailable |

Request schemas are strict zod 4 objects. Issue messages, issue order and the accepted values are
observable and are taken from the running baseline: `Invalid input: expected string, received
undefined`, `Too small: expected string to have >=1 characters`, `Unrecognized key: "x"`, `Invalid
UUID` and similar. Notable behavior observed on the pinned commit:

- Issues follow schema key order, array elements in order, and the unrecognized key issue comes last
  with path `[]` (or the element path).
- `input` of `DispatchManualRunRequest` is required at runtime (`Invalid input: expected nonoptional,
  received undefined`) although the OpenAPI document does not list it under `required`.
- Length checks also run on values of the wrong type that carry a length (arrays, and objects with a
  numeric `length`), so one wrong-typed value can yield two issues.
- `projectSlug`, `trigger`, `actor` and `deliveryKey` are trimmed with `String.prototype.trim` before
  their length checks, and the trimmed value is what the operation receives (observed: no-break
  space, byte order mark and line and paragraph separators are trimmed, U+0085 is not).
- String limits count UTF-16 code units. UUIDs accept versions 1 to 8 with any case, the nil UUID
  and the lowercase max UUID only.
- The key `__proto__` was not reported as unrecognized at the top level or inside a file element, and
  is not read as a schema key. Integer-like keys are listed first, in ascending order, then other
  keys in insertion order. For duplicate JSON keys the last value wins.
- `Request.json()` drops up to two leading UTF-8 byte order marks (two parse, three do not) and
  replaces invalid UTF-8 inside strings.

## Credential rules

`PublicCredentialAuthenticator` (`src/auth/public-credentials.ts`) sends the request to the CLI
credential authenticator when the `Authorization` header starts with `Bearer paseo_cli_`, otherwise
to the API key authenticator.

- Both read `Authorization` and accept only the literal prefix `Bearer ` followed by 1 to 200
  characters. Header names and the `Bearer` word are case-sensitive in the value.
- API keys have the form `paseo_pk_` plus 12 characters from `[A-Za-z0-9_-]`, `_`, then a non-empty
  secret (`src/auth/api-keys.ts`). The prefix selects the stored row, the SHA-256 base64url hash of
  the whole token is compared in constant time, a revoked row is unauthorized, and a row without the
  required scope is forbidden. The stored scope list is returned as the credential scopes.
- CLI credentials have the form `paseo_cli_` plus 12 characters from `[A-Za-z0-9_-]`, `_`, then a
  non-empty secret (`src/auth/cli-credentials.ts`). Rules are the same except that there is no scope
  check: a valid credential carries all five scopes and is authorized for any operation.
- A successful check records last use; if the update does not affect exactly one row the result is
  unauthorized.

## CLI device authorization

`src/cli-authorizations/index.ts`, with state from `Database.startCliAuthorization`,
`pollCliAuthorization`, `inspectCliAuthorization` and `decideCliAuthorization`. Constants: lifetime
600 seconds, initial poll interval 5 seconds, 5 active requests per client fingerprint, 1,000 active
requests overall. The fingerprint is the SHA-256 base64url hash of the `x-paseo-client-address`
request header, or `unknown`.

- `start` takes an empty strict JSON object (`Request.json()` failure or any other body is 400
  `{"error":"invalid_request"}`; the content type is not checked). It generates a 32-byte base64url
  device code and an 8-byte base32 user code formatted as 4-4-5 characters with dashes. Capacity
  exhaustion returns 429 `{"status":"retry_later","interval":5}` with `retry-after: 5`. Success is 201
  with `deviceCode`, `userCode`, `verificationUri`, `verificationUriComplete`, `expiresAt`, `interval`.
  The verification URI is `/cli-login` resolved against the configured public base URL, or the
  request URL when none is configured; the complete form adds `?code=<userCode>`.
- `poll` takes `{"deviceCode": string(32 to 200 characters)}` strictly, otherwise 400
  `invalid_request`. The response is 200 with `status` and `interval`, plus `credential` and
  `organizationId` when authorized. States: `pending`, `slow_down`, `authorized`, `denied`,
  `expired`, `disclosed`. Polling before `nextPollAt` returns `slow_down` and adds 5 seconds to the
  interval; an unknown device code returns `expired` with interval 5; an approved request is
  disclosed once (`authorized`) and every later poll returns `disclosed`. The credential is derived
  from the device code (`paseo_cli_` plus the first 12 base64url characters of
  SHA-256(`paseo-cli-prefix\0` + device code), `_`, SHA-256(`paseo-cli-credential\0` + device code)
  as base64url).
- `inspect` and `decide` require browser organization access. No access object returns 503
  `{"error":"auth_unavailable"}`; the cookie-mutation check response is returned as is; a
  `ProductRequestError` becomes `{"error":<code>}` with its status; other errors propagate.
  `inspect` returns `expiresAt`, `organization` and `canManage`, or 404
  `{"error":"authorization_unavailable"}` when the code is unknown, expired or already decided.
  `decide` returns 403 `{"error":"forbidden"}` without the manage capability, then validates the
  body (400 `invalid_request`), then 403 `{"error":"organization_required"}` when the body
  organization is not the access organization, then 404 `authorization_unavailable` or 200
  `{"status":"approved"}` or `{"status":"denied"}`.
- The user code is normalized with NFKC, upper-casing and removal of everything outside `A-Z2-7`
  before lookup, so lower case, missing dashes and compatibility characters all match.
- Request status values are `pending`, `approved`, `denied`, `expired`, `disclosed`; a decision is
  only possible while `pending`, and expiry is evaluated when the record is read.

## OpenAPI document

`publicOpenApiDocument` is generated at import time with `@asteasolutions/zod-to-openapi` from the
manifest, the CLI start and poll registrations and the schemas in `src/public-api/contracts.ts`.
`publicApi.openapi()` returns it with `Response.json` and `cache-control: public, max-age=300`. The
document has OpenAPI `3.1.0`, twelve paths (the ten operations plus the two CLI routes), 22
component schemas, a `bearerAuth` security scheme, a `servers` entry for `/`, and empty
`components.parameters` and `webhooks`. Manifest operations carry `security` and
`x-required-scopes`; every documented manifest response lists `X-Request-ID`, the 401 response also
lists `WWW-Authenticate`, and error responses use `application/problem+json` with the `Problem`
schema. The two CLI routes document their responses without headers.

## Rust coverage and differential result

Every item below is exercised by `scripts/phase2/hub-api-original.integration.test.ts` (the real
pinned Hub code at `28f6c78`, offline, in-memory database, `TZ=UTC`, node v22.20.0 (version and sha256 asserted by the capture script), zod 4.4.3,
`@asteasolutions/zod-to-openapi` 9.1.0) and by `crates/spocky-hub-pilot/tests/hub_api_evidence.rs`
(Rust). Both run one case list, `scripts/phase2/hub-api-cases.json`, which
`scripts/phase2/hub-api-cases.mjs` generates; the capture script refuses to run when the two differ.

- `scripts/phase2/hub-api-capture.sh` archives the pinned commit, installs from the local npm cache and
  writes the raw trace and the OpenAPI document to `evidence/raw/phase2/`.
- `scripts/phase2/hub-api-compare.sh` runs the Rust test against those raw files and compares the trace
  and the OpenAPI document with `cmp`. Object key order, whitespace and every response body byte are part
  of the comparison; there is no key sorting and no normalization.
- `hub_api_evidence` asserts on every run that the Rust trace equals the committed trace
  `evidence/phase2/hub-api-original.json` and that the Rust OpenAPI document equals the committed
  `evidence/phase2/hub-api-openapi-original.json`. It also pins the case counts (471 HTTP cases, 68
  scenarios, 10 manifest operations), so an empty or shrunken baseline fails.
- Trace headers are the response `Headers` iteration: names lower case, sorted by name, equal names
  combined with `, `. Operation inputs longer than 2,048 UTF-16 units are recorded as
  `sha256:<UTF-16 length>:<digest of the UTF-8 JSON text>` instead of the text, so for the
  megabyte-sized string cases the comparison covers the digest, not the characters.
- Generated values are injected, never rewritten afterwards: identifiers `00000000-0000-4000-8000-<n>`
  (a counter reset for each case or scenario), random bytes taken from SHA-256 of
  `spocky-hub-api-random:<n>`, and a fixed clock starting at `2026-08-06T12:00:00.000Z`. A test checks
  the committed baseline values against those sequences. The baseline draws two random byte strings
  and one identifier per API key it creates; the Rust scenario skips the same draws.

| Area | Cases compared |
| --- | --- |
| Manifest | the ten operations with method, path, scope, success status, result mapping, tag, summary, description, whether a request schema exists, and every documented response |
| Responses by result | every result variant of every operation (including results that break the response schema and must become 500), full response bodies and headers |
| Authentication outcomes | unauthorized, forbidden, unavailable and thrown errors for each operation, the scope asked of the authenticator, and database and generic failures thrown by operations |
| Routing | unknown paths, trailing slash, case, percent-encoding, dot segments, backslashes, tabs, query and fragment, hosts and ports, CLI and OpenAPI paths on the splat router, 405 with `Allow` for every other method, `handleOperation` without routing, unavailable composition |
| Request identity | absent, blank, padded, duplicate, non-ASCII and no-break-space `x-request-id` values on success and problem responses |
| Body decoding | content type variants, byte order marks, invalid UTF-8, duplicate keys, integer-like and `__proto__` keys, escapes, trailing data, whitespace sets, nesting |
| Request schemas | strict object checks with every wrong type, boundary lengths in UTF-16 units, trimming sets, UUID forms, array limits, nested element issues, unknown key wording, and the length checks that run on wrong-typed values including `ToNumber` conversion of an object `length` |
| Operation inputs | the exact JSON each operation receives (key order, trimmed values, number text) |
| OpenAPI | the whole document, byte for byte, and its headers, size and SHA-256 |
| CLI device authorization | start, poll, inspect and decide bodies and states, polling throttle, expiry at the exact second, per-client and global limits, verification URIs for configured and request URLs, user code normalization including compatibility characters, every access failure kind |
| API key scope order | keys created with `[runs:dispatch, projects:read]` and with a repeated scope report their scopes in creation order with duplicates removed, as the baseline's `[...new Set(scopes)]` does |
| Deep `length` arrays | a wrong-typed value whose `length` is an array nested 1,000 to 3,000 deep is answered 400, and 3,200 or more nested levels 500 `internal_error` (V8 on node 22.20.0 throws a `RangeError` while joining; node 26 had joined 4,400, so the boundary moves with the Node version and the stack). The exact boundary between 3,000 and 3,200 is not compared and is listed as a divergence of the stack-depth family. Rust joins iteratively and stops at 3,100 levels. The CLI authorization bodies (`poll`, `inspect`, `decide`) throw the same `RangeError` out of the handler, which Rust reports as a handler error with the same message |
| Credential rules | Bearer parsing, prefix routing, API key scopes and revocation, CLI credential issue, disclosure and revocation, forbidden and unauthorized outcomes over 43 distinct authorization header shapes and five scopes (59 checks) |

Result of the last run of `hub-api-compare.sh`: `matched: true`, `comparison: byte-identical`,
`normalization: none`. The traces have SHA-256
`78a18e074a8cee6d81387a8e2d6cbd4b95de500aad0956c9535c893679815ba9` and the OpenAPI documents have
SHA-256 `7e5bd6cc236947da1c0428a9ef2d3594f1fc063602da0bd58d47c6391d672b69` (see
`hub-api-sha256.txt`). Behavioral tests of the same flows are in `crates/spocky-hub-pilot/tests/hub_api.rs`.

API keys are authorized through the existing `HubPilot::authorize_api_key`; CLI credentials are
checked against the in-memory credential store that device authorization fills. Both are compared on
outcome, credential kind, organization and scopes.

## Remaining gaps (not covered, not claimed)

- Everything listed above under "Not observable offline": PostgreSQL behavior, the operation semantics
  in `src/public-operations`, browser surfaces and framework routing.
- The API key store is the Rust file-backed key boundary, not the PostgreSQL `OrganizationApiKeys`;
  credential ids and last-use times are not compared. CLI credentials live in memory, not in
  `organization_cli_credentials`.
- Operation results that Rust types cannot represent: non-integer versions, workflow statuses outside
  the four known values and structurally invalid results rejected by the baseline's `is*Result`
  guards. Integer versions at or below zero and non-UUID identifiers are covered.
- JSON strings with lone surrogate escapes are held by `spocky_contracts::js_value` as an escape pair;
  no case compares them against the baseline yet. Nesting is covered up to about 1 MB of brackets
  (500,000 arrays, 200,000 objects and an unterminated run): `js_value` parses, clones and drops
  iteratively, and the pinned Hub answers 400 `invalid_request` or `invalid_json`.
- Header values outside Latin-1 and request URLs with credentials cannot occur in fetch and are not
  modelled.
- Database failures inside the CLI authorization handlers propagate out of the baseline handlers; Rust
  reports only access failures and an unusable verification URL that way.
- The baseline never prunes `cli_authorizations` records (no delete in `memory.ts`, `pg.ts` or the
  migrations); neither does Rust. The in-memory list is unbounded and scanned linearly.
- Failure logging (`reportFailure`) and the framework's response to an exception are not compared.
- Concurrent decisions, polls and starts. The in-memory state machine is single threaded.

## What is testable offline and what is not

Observable offline with the pinned source, its in-memory database and stubbed operations:

- The public API boundary in `src/public-api/index.ts` and its tests (`public-api.test.ts`), driven
  with a stub authenticator and stub operations: routing, request identity, authentication
  outcomes, body decoding, schema validation, result mapping, error boundary, composition.
- The operation manifest and the OpenAPI document byte for byte.
- `CliAuthorizations` against the in-memory database (`cli-authorizations.test.ts`): start, poll,
  inspect, decide, throttling, expiry, capacity limits, single disclosure.
- API key and CLI credential authentication with a query runtime that answers the few statements
  they issue.

Not observable offline, so not part of any claim here:

- `src/public-api/built-server.integration.test.ts` needs the production build and a PostgreSQL
  container (`RUN_BUILT_PUBLIC_API_TESTS=1`): tenant isolation of projects and resources, persisted
  configuration revisions, receipts and enrollment tokens, and the PostgreSQL error boundary.
- `src/cli-authorizations/cli-authorizations.integration.test.ts` needs a PostgreSQL container:
  serialized concurrent decisions and polls, the session and membership authority check inside
  `decideCliAuthorization` (the in-memory database never returns `forbidden`), stored credential
  rows and database-clock expiry.
- `src/public-operations` (the operation semantics behind the manifest: configuration compilation,
  trigger YAML validation, project resolution, enrollment token persistence) and the
  `createAppPublicOperations` adapters.
- Browser surfaces: `src/cli-authorizations/approval.tsx`, `functions.ts`, the `cli-login` route and
  the API reference page.
- TanStack router behavior for methods other than `POST` on the two CLI routes and for
  `GET /api/openapi.json` headers added by the framework.
