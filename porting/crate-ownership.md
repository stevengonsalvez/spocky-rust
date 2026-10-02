# Crate and Ownership Graph

```text
                         spocky-contracts
                       /        |        \
              spocky-domain  spocky-wire  spocky-crypto
                 /     |         |           |
        spocky-store  spocky-auth  |     spocky-relay-protocol
              |        |         |          /       \
              +----- spocky-daemon ---------+     spocky-relay
                       /   |   \                    |
              providers  hub  relay          relay deployment
                    \      |      /                 |
                     spocky-client             relay operations
                      /    |    \
              spocky-cli  plugins  importer
                         |
                    spocky-ui-core
                   /      |       \
             app shells  Hub UI  site/docs
```

## Lead-owned shared crates

| Boundary | Responsibilities |
|---|---|
| `spocky-contracts` | Wire schemas, capability descriptors, persisted representations, public cross-process types |
| `spocky-wire` | JSON envelopes, binary terminal and file frames, framing limits, codec errors |
| `spocky-crypto` | Endpoint key formats, vectors, pairing, encrypted frames, replay boundary behavior |
| `spocky-domain` | Lifecycle, identity, permissions, schedules, receipts, recovery, invariants |
| `spocky-differential` | Scenario definition, original and Rust drivers, raw artifacts, normalization, comparisons, executed-count protection |

The lead alone changes these interfaces, the workspace manifest, the task ledger, and integration wiring.

## Service and adapter ownership

| Boundary | Capability families |
|---|---|
| `spocky-store` | `DSTA`, `DWLABEL`, old state, migrations, atomic writes, journal recovery |
| `spocky-daemon` | `DLIF`, `DSEC`, `DTRM`, `DSCH`, `DGIT`, `DFIL`, `DBRW`, `DSVC`, `DOPS`, `DPUSH`, `DSPH`, `DWORK`, `DSCRIPT`, `DCFG`, `DWEB`, `DUSAGE`, `DRECEIPT`, `DMCP` |
| `spocky-provider-api` and provider crates | `DPRV` and external provider process adapters |
| `spocky-daemon-hub` | `CLOUD-HUB-REL`, `CLOUD-HUB-EXEC`, `CLOUD-HUB-DAEMON`, `CLOUD-HUB-SESSIONS`, `CLOUD-HUB-ATTACH` |
| `spocky-daemon-relay` | `CLOUD-RELAY-DAEMON` |
| `spocky-relay` | Distributed ownership, reroute, flow control, operations |
| `spocky-hub` | Auth, first-run setup, configuration compiler, API, store, workflows, integrations, billing, deployment |
| `spocky-client` | `DSDK`, `CLIENT-002` through `CLIENT-007` shared behavior |
| `spocky-cli` | `DCLI`, Hub CLI, exact process output and exit behavior |
| `spocky-plugin` | Plugin manifest, acquisition, review, staging, server/client host, update recovery, compatibility boundary |
| `spocky-skills` | Bundled and discovered skill indexing, compatibility, invocation inputs |
| `spocky-import` | Local session and Conductor import contracts |
| `spocky-ui-core` | Route, workspace, presentation, visual, accessibility, localization contracts |
| platform UI shells | iOS, Android, browser, desktop, Hub dashboard, site and docs renderers selected by pilots |
| `spocky-relay-cloudflare` | Cloudflare fallback, cutover, routing, protocol limits, deployment inputs |
| `spocky-delivery` | Packages, containers, installers, signing inputs, updates, rollout, rollback |

## Recorded dependency edges

| Edge | Reason | Recorded |
|---|---|---|
| `spocky-store` -> `spocky-contracts` | single JavaScript-compatible JSON implementation, `spocky-contracts/src/js_value.rs` | 2026-10-01, `porting/tasks.json` routing revision |
| `spocky-hub-pilot` -> `spocky-contracts` | the Hub public API uses `js_value` instead of its own `public_api/json.rs`, which is deleted | 2026-10-01, coordinator decision on the `p2_hub_api` review |
| `spocky-terminal` -> `spocky-wire` | terminal restore and snapshot frames use the binary terminal frame codec that `spocky-wire` owns | 2026-10-02, lead seam review of `p4_terminal` 8350699..fe4c3af |
| `spocky-xterm` -> `spocky-contracts` (dev only) | differential tests use the contracts text helpers; `spocky-xterm` keeps no Spocky runtime dependency except `spocky-contracts` | 2026-10-02, lead seam review of `p4_xterm_core` 0f5db95..8db4af1 |

No crate may carry a second JavaScript-compatible JSON parser or writer.

JavaScript value operators (ToBoolean truthiness, object spread, `String()`
conversion) over `JsValue` live in one place: `spocky-contracts/src/js.rs`,
next to `js_value`. Decided 2026-10-01 by the lead at the coordinator's
request. Sequence: `p3_session`'s `crate::js` integrates first; `p3_contracts`
moves it verbatim with its tests into `spocky-contracts`; `p3_session` switches
to it and deletes its copy; `p3_provider_codex` replaces `transport.rs`
`js_truthy` with it. No crate may carry a second implementation of these
operators.

## Architecture freeze rule

Names describe ownership boundaries, not a commitment to one crate per row. Contract and tool-selection pilots may split crates or revise dependency direction. They may not merge direct Hub transport into relay, terminate endpoint encryption in relay, or move compatibility logic into presentation code.

UI, plugin, native, browser, and delivery runtime choices remain unselected until measured pilots cover every required platform. No compatibility exception exists merely because the baseline uses JavaScript, Swift, Kotlin, Elixir, or browser APIs.
