# Hub account, organization and API-key routes: enumeration

Baseline: Hub `28f6c78833065fd282f9064f92a9aa61875dd359`, `src/auth/organization-access.ts`
(`OrganizationAccess.handle`) behind `src/auth/server.ts:245` (`/api/auth/paseo/*`), plus the
`better-auth` library handler for the rest of `/api/auth/*`. Scope of this slice: the HTTP route
table, request validation, status codes and error bodies. Capture target: the real pinned
`createAuthServer` on PostgreSQL, driven by raw `Request` objects, trace compared byte for byte.

## Route table (`/api/auth/paseo/*`)

Every route is a raw `Request` handler. Errors are `{"error":"<code>"}` with the listed status.
Order of checks matters and is part of the contract. Unknown path or a non-POST method on any path
other than the two GET routes answers 404 `not_found`. An unexpected failure answers 500
`request_failed`; an entitlement denial answers through `entitlementDenialResponse`.

| Method and path | Body schema (strict) | Success | Errors in check order |
|---|---|---|---|
| GET `state` | none | 200, one of six `status` payloads | none; never throws a product error |
| GET `api-keys` | none | 200 `{keys[],cliCredentials[]}` | 401 `unauthenticated`, 403 `password_change_required`, 403 `organization_required`, 403 `forbidden` (needs `manageResources`) |
| POST `create-organization` | `{name: trim, min 1, max 100}` | 201 `{organizationId, organizationSlug}` | 401, 403 `password_change_required`, 403 `organization_creation_disabled`, 400 `invalid_request` |
| POST `select-organization` | `{organizationId: min 1}` | 200 `{organizationId}` | 401, 403 `password_change_required`, 400 `invalid_request`, 404 `organization_unavailable`, 401 `unauthenticated` (expired session row) |
| POST `create-invitation` | `{email: trim email, role: invitationRole}` | 201 manager summary with `link` | 401, 403 pcr, 400, 403 `organization_required`, 403 `forbidden`, 409 `already_member`, entitlement denial |
| POST `cancel-invitation` | `{invitationId: min 1}` | 200 `{canceled:true}` | 401, 403 pcr, 400, 403 `organization_required`, 403 `forbidden`, 404 `invitation_unavailable` |
| POST `accept-invitation` | `{invitationId: min 1}` | 200 `{organizationId}` | 401, 403 pcr, 400, 404 `invitation_unavailable` (unknown, expired, other email, bad role) |
| POST `change-member-role` | `{memberId: min 1, role: organizationRole}` | 200 `{memberId, role}` | 401, 403 pcr, 400, 403 `organization_required`, 404 `member_unavailable`, 403 `forbidden`, 409 `last_owner_required` |
| POST `remove-member` | `{memberId: min 1}` | 200 `{removed:true}` | 401, 403 pcr, 400, 403 `organization_required`, 404 `member_unavailable`, 403 `forbidden`, 409 `last_owner_required` |
| POST `api-keys` | `{name: trim min 1 max 100, scopes: string[] min 1}` | 201 `{key{...}, secret}` | 401, 403 pcr, 400, 403 `organization_required`, 400 `invalid_api_key_scopes` (unknown or duplicate scope), 403 `forbidden` |
| POST `revoke-api-key` | `{id: uuid}` | 200 `{revoked:true}` | 401, 403 pcr, 400, 403 `organization_required`, 403 `forbidden`, 404 `api_key_unavailable` |
| POST `revoke-cli-credential` | `{id: uuid}` | 200 `{revoked:true}` | same as `revoke-api-key`, 404 `cli_credential_unavailable` |

Body parse: invalid JSON and a schema failure are both 400 `invalid_request`; no field detail is
returned. The session is read before the body, so an anonymous request with a bad body is 401.

## Dispatch in `createAuthServer().handle` (`auth/server.ts:243`)

Checked in this order; the first match wins.

| Order | Match | Behavior |
|---|---|---|
| 1 | path starts with `/api/auth/paseo/` | cross-origin cookie guard, then `OrganizationAccess.handle` |
| 2 | `/api/auth/sign-up/email` | registration admission, then better-auth; `RegistrationAdmissionError` is 403 `registration_closed` |
| 3 | `/api/auth/change-password` | cross-origin cookie guard, then the Hub `changePassword` handler |
| 4 | not in `RAW_PRODUCT_PATHS` and not under `/api/auth/reset-password/` | 404 `{"error":"not_found"}` for every method |
| 5 | everything left | better-auth `handler(request)` |

`RAW_PRODUCT_PATHS` is an allowlist of six: `get-session`, `sign-up/email`, `sign-in/email`,
`sign-out`, `change-password`, `verify-email`. The better-auth library's other endpoints are not
reachable over HTTP on the pinned Hub; the account recovery calls (`request-password-reset`, POST
`reset-password`) go through server functions, not these paths.

Cross-origin cookie guard (`rejectCrossOriginCookieMutation`): only for POST with a `cookie` header.
Evaluated in order: `sec-fetch-site: cross-site` is 403 `{message:"Cross-site navigation login
blocked. This request appears to be a CSRF attack.", code:"CROSS_SITE_NAVIGATION_LOGIN_BLOCKED"}`;
missing or `null` `origin` and `referer` is 403 `MISSING_OR_NULL_ORIGIN` ("Missing or null Origin");
an origin that differs from the browser origin is 403 `INVALID_ORIGIN` ("Invalid origin"). The
browser origin is `x-paseo-trusted-request-origin` when present (a malformed value throws) and the
configured base URL otherwise. The guard runs before session and body.

## Rust status today (`crates/spocky-hub-pilot/src/http.rs`)

`HubHttpService::handle` has 13 routes. The six paseo routes it shares with the table do not match
the baseline wire contract; they are an earlier pilot, not a port:

| Item | Baseline | Rust today |
|---|---|---|
| anonymous | 401 `unauthenticated` | 401 `unauthorized` |
| bad body | 400 `invalid_request` | 400 `invalid_body` |
| `select-organization` for a non-member | 404 `organization_unavailable` | maps hub errors by pilot rules |
| `create-invitation` without an active organization | 403 `organization_required` | 404 `organization_unavailable` |
| strict objects, trim, max lengths | zod schemas above | serde defaults |

Wire differences found in the 13 routes (baseline dispatch above):

- `POST request-password-reset`, `POST reset-password`, `paseo/change-password` and
  `paseo/complete-app-setup` are routes the baseline does not serve: the first two are 404 on the
  baseline, change-password lives at `/api/auth/change-password`, and app setup has no HTTP path
  (`completeAppOnboarding` is a server function).
- `sign-out` is served by the baseline and missing in Rust.
- The cross-origin cookie guard does not exist in Rust.
- `GET reset-password/<token>` is allowed by the baseline and handled by better-auth.

Missing entirely: `GET api-keys`, `POST api-keys`, `revoke-api-key`, `revoke-cli-credential`,
`create-organization`, `change-member-role`, `remove-member`, the entitlement checks (`canInviteMembers`,
seats headroom), seat-change notification, last-owner protection, the 48 hour invitation lifetime,
and manager invitation `link`.

Domain support in `HubPilot` and the relational modules: `api_keys.rs` (create, list, authorize,
revoke), `relational_sessions.rs` (select organization, remove membership), `lib.rs`
(`create_organization_for_session`, `select_organization`, `add_member`). No `change_member_role`,
no CLI credential list or revoke on the HTTP side, no entitlement service.

## better-auth part

The library behind `auth.handler` (`auth/server.ts:158`: email and password with a minimum length,
`organization` plugin with `teams` and dynamic access control off, `mustChangePassword` and
`isInstanceOperator` user fields, verification and reset mailers) answers `get-session`,
`sign-in/email`, `sign-out`, `verify-email`, the sign-up body after admission, and
`reset-password/<token>`. Its payloads are library behavior; the differential records them byte for
byte from the real library instead of re-deriving them.

## Boundary with p4_hub_control

- `spocky-hub-pilot` (this lane): HTTP routes, request validation, status codes, error bodies,
  response shaping, the differential.
- Auth and daemon domain crates (p4_hub_control): membership, invitation, API-key and CLI-credential
  rules. Until those crates exist the handlers call `HubPilot` and the relational modules, and the
  three missing domain operations (`change_member_role` with last-owner protection, `remove_member`,
  CLI credential revoke) are added behind the same `HubPilot` seam so they can move unchanged.

## Capture and differential

Engine: embedded PGlite on both sides (coordinator decision: no container). The baseline runs the
real production runtime (`startProductionRuntime`, no `DATABASE_URL`, fresh data directory per
scenario) and sends raw `Request` objects through `runtime.auth`, the entry the `/api/auth/$` route
calls. `runtime.browserAccount` (the entry server functions use) skips the origin guard and the
dispatch allowlist and is not part of the HTTP surface. The Rust side runs the same SQL on
`spocky-pglite-host` with the Hub's own migrations.

Baseline trace (`hub-account-routes-original.json`, sha256 in `hub-account-routes-sha256.txt`):

- Case list `scripts/phase2/hub-account-routes-cases.mjs` prints `hub-account-routes-cases.json`;
  the capture script refuses a JSON that differs from the generator output.
- Two scenarios, 281 traced requests: `open` (open registration and organization creation; five
  actors plus a maker and a rival organization) and `bootstrap` (invite-only, bootstrap owner,
  organization creation disabled, forced password change).
- Per request the trace holds status, every response header and the raw body. Masking is limited
  to generated identity (account, membership, organization, invitation, key ids, slugs and secrets,
  each replaced by its capture name) and wall clock (an ISO timestamp becomes its whole-hour offset
  from the request, so the 48 hour invitation lifetime stays checked).
- Two captures are byte-identical.

Not traced: sign-in, sign-up, sign-out and verify-email payloads (better-auth session cookies belong
to the auth domain crate), `change-password`, the entitlement denial path (the capture stamps the
unlimited self-hosted template, so no request is denied), and a successful `revoke-cli-credential`
(a CLI credential needs the device authorization flow; the unknown, forbidden and validation paths
are traced).

Rust replay plan: handlers over `PgliteHost` issue the baseline SQL verbatim inside the same
transaction boundaries; a session reader seam (cookie to account session, as
`AccountSessionReader`) stands in for better-auth, with the test reader reading the `session` and
`user` rows the seed wrote. Order of work: the origin guard and dispatch, request schemas, the read
routes, then each mutation with its SQL, then the byte comparison with counts pinned in the test.

## Open decisions

- Entitlements (`requireFlag`, `requireHeadroom`) are a separate Hub module. Recommendation: model
  them as an injected trait with the unlimited self-hosted implementation, as the baseline tests do.
- The six Rust routes the baseline does not serve (see above) change a public surface. Recommendation:
  move them to the baseline paths and delete the extras in the same slice, with the coordinator
  confirming no caller depends on them (`tests/account_recovery_http.rs`, `tests/http_runtime.rs`).
