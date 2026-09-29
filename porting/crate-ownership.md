# Crate and Ownership Graph

```text
                         paseo-contracts
                       /        |        \
              paseo-domain  paseo-wire  paseo-crypto
                 /     |         |           |
        paseo-store  paseo-auth  |     paseo-relay-protocol
              |        |         |          /       \
              +----- paseo-daemon ---------+     paseo-relay
                       /   |   \                    |
              providers  hub  relay          relay deployment
                    \      |      /                 |
                     paseo-client             relay operations
                      /    |    \
              paseo-cli  plugins  importer
                         |
                    paseo-ui-core
                   /      |       \
             app shells  Hub UI  site/docs
```

## Lead-owned shared crates

| Boundary | Responsibilities |
|---|---|
| `paseo-contracts` | Wire schemas, capability descriptors, persisted representations, public cross-process types |
| `paseo-wire` | JSON envelopes, binary terminal and file frames, framing limits, codec errors |
| `paseo-crypto` | Endpoint key formats, vectors, pairing, encrypted frames, replay boundary behavior |
| `paseo-domain` | Lifecycle, identity, permissions, schedules, receipts, recovery, invariants |
| `paseo-differential` | Scenario definition, original and Rust drivers, raw artifacts, normalization, comparisons, executed-count protection |

The lead alone changes these interfaces, the workspace manifest, the task ledger, and integration wiring.

## Service and adapter ownership

| Boundary | Capability families |
|---|---|
| `paseo-store` | `DSTA`, `DWLABEL`, old state, migrations, atomic writes, journal recovery |
| `paseo-daemon` | `DLIF`, `DSEC`, `DTRM`, `DSCH`, `DGIT`, `DFIL`, `DBRW`, `DSVC`, `DOPS`, `DPUSH`, `DSPH` |
| `paseo-provider-api` and provider crates | `DPRV` and external provider process adapters |
| `paseo-daemon-hub` | `CLOUD-HUB-REL`, `CLOUD-HUB-EXEC` |
| `paseo-daemon-relay` | `CLOUD-RELAY-DAEMON` |
| `paseo-relay` | Distributed ownership, reroute, flow control, operations |
| `paseo-hub` | Auth, API, store, workflows, integrations, billing, deployment |
| `paseo-client` | `DSDK`, `CLIENT-002` through `CLIENT-007` shared behavior |
| `paseo-cli` | `DCLI`, Hub CLI, exact process output and exit behavior |
| `paseo-plugin` | Plugin manifest, server/client host, compatibility boundary |
| `paseo-skills` | Bundled and discovered skill indexing, compatibility, invocation inputs |
| `paseo-import` | Local session and Conductor import contracts |
| `paseo-ui-core` | Route, workspace, presentation, visual, accessibility, localization contracts |
| platform UI shells | iOS, Android, browser, desktop, Hub dashboard, site and docs renderers selected by pilots |
| `paseo-relay-cloudflare` | Cloudflare fallback, cutover, routing, protocol limits, deployment inputs |
| `paseo-delivery` | Packages, containers, installers, signing inputs, updates, rollout, rollback |

## Architecture freeze rule

Names describe ownership boundaries, not a commitment to one crate per row. Contract and tool-selection pilots may split crates or revise dependency direction. They may not merge direct Hub transport into relay, terminate endpoint encryption in relay, or move compatibility logic into presentation code.

UI, plugin, native, browser, and delivery runtime choices remain unselected until measured pilots cover every required platform. No compatibility exception exists merely because the baseline uses JavaScript, Swift, Kotlin, Elixir, or browser APIs.
