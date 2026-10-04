# Baseline Inventory Checkpoint

## Routing evidence

| Lane | Worker | Model | Effort | Baseline | State |
|---|---|---|---|---|---|
| daemon, protocol, state, providers, CLI | `inventory_daemon` | `gpt-5.6-sol` | medium | Paseo `5de45e208690b0efc51c59a585ae9729325a9204` | integrated |
| clients, plugins, UI, native | `inventory_clients` | `gpt-5.6-sol` | medium | Paseo `5de45e208690b0efc51c59a585ae9729325a9204` | integrated |
| Hub, relay, importer, delivery | `inventory_cloud` | `gpt-5.6-sol` | medium | all four pinned commits | integrated |

Runner spawn metadata records requested model and effort. The runner does not expose independent model-attestation output, so assignment remains a recorded evidence limitation.

## Measured reference surface

Exact generated indexes under `porting/details/` retain these records:

| Index | Records |
|---|---:|
| Paseo tracked tree | 5,069 |
| Hub tracked tree | 823 |
| Relay tracked tree | 60 |
| Importer tracked tree | 32 |
| Paseo tests | 1,659 |
| Hub tests | 213 |
| Relay tests | 14 |
| Importer tests | 5 |
| Protocol literals | 628 |
| Server features | 78 |
| CLI command declarations | 90 |
| App routes | 25 |
| Hub routes | 48 |
| Compatibility sites | 440 |
| Delivery inputs | 64 |
| Total retained records | 9,248 |

The capability matrix has 94 source-backed rows. Rejected reviews drove explicit mappings for skills, workspace-label recovery, push, speech and model downloads, Hub MCP behavior, importer WAL rejection, relay protocol limits, workspaces, scripts, daemon configuration, hosted web UI, usage, message receipts, daemon MCP, Hub setup and configuration, Hub daemon and session authority, attachments, plugin lifecycle, client preferences and profiles, command search, settings restart recovery, creation, schedule editing, History, and client usage presentation.

Native audio currently has no automated native tests. Hub exposes PostgreSQL and embedded PGlite migrations. Relay exposes distributed ownership, reroute, bounded flow control, readiness, and load-shedding contracts. Importer supports Conductor with macOS discovery and explicit cross-platform database selection.

These counts are inventory observations, not parity proof. Fixture generation records exact executed counts separately.

## Review status

Two independent read-only reviews rejected candidate `5e0ad27f8381e83ecbec83ea5a8f5c2dd67429ac`. Fresh review pairs rejected repaired candidates `018c58144fe1457e15521fd9feabab3db7e44569` and `f7fc96ca51ee1d54c13327874de1bb0c9eb9636c`. Candidate `89893dd5349472f120ab7d3ed65a67423babab8f` received one acceptance and one rejection. Two fresh reviewers accepted signed candidate `2fa22be761a0fbec42fd759df69bff37e248824c`; Phase 1 is complete.

## Preserved baseline defects

| ID | Observed baseline behavior |
|---|---|
| GAP-001 | Protocol package remains explicitly unstable despite compatibility requirements. |
| GAP-002 | Compatibility backlog contains hundreds of tagged shims and legacy RPCs. |
| GAP-003 | Workspace-resource permission enforcement is incomplete. |
| GAP-004 | File preview accepts any daemon-readable regular file. |
| GAP-005 | HTTP and MCP remain open when daemon password is unset. |
| GAP-006 | Public service proxy bypasses daemon password. |
| GAP-007 | Forwarded authority remains client-influenced. |
| GAP-008 | Schedule overlap is rejected and restart marks active runs failed. |
| GAP-009 | Scheduled permission requests count as failed runs. |
| GAP-010 | Provider discovery is asynchronous and initially reports loading. |
| GAP-011 | Old clients hide non-legacy providers by app version. |
| GAP-012 | Some ACP providers reject non-empty MCP configuration. |
| GAP-013 | Browser automation requires a connected capable host. |
| GAP-014 | Terminal sessions are not persisted. |
| GAP-015 | The last interacting client owns PTY dimensions. |
| GAP-016 | Agent storage keys by working directory rather than workspace ID. |
| GAP-017 | Hidden subagent presentation resets after app restart. |
| GAP-018 | PID ownership is not cryptographic. |
| GAP-019 | Invalid schedule files are skipped and logged. |
| GAP-020 | An unreadable encryption keypair regenerates daemon identity. |
| GAP-021 | A project or workspace registry file that fails to load is treated as empty, and the next mutation overwrites it (`workspace-registry.ts` `load()` 285-304). Pinned by `crates/spocky-store/tests/registry_files.rs`. |

Do not silently repair these behaviors during parity work. A safety exception requires explicit approval and a recorded differential.

## Approved divergences

| ID | Divergence | Decision |
|---|---|---|
| DIV-001 | Pinned Node `22.20.0` `JSON.stringify` throws `RangeError` on values nested between 3,000 and 5,000 levels deep, at a stack-dependent depth; the Spocky JSON writer in `spocky-store` writes them. Pinned `JSON.parse` reads at least 100,000 levels, so Node can read what Spocky writes. Node `26.7.0` stringifies 1,000,000 levels, so the limit is not stable across Node versions either. Same family: pinned zod `4.4.3` recursive `z.json` validation throws `RangeError` by 10,000 levels (3,000 passes), so the baseline skips such an agent record on load while Spocky loads it. | Accepted by the coordinator on 2026-10-01 while Stevie is away. The baseline limit is nondeterministic and only affects values the baseline can never persist. A mixed-version test must show pinned Node parsing a Spocky-written 10,000-deep record. |
| DIV-002 | At timeline seq 2^53 (`Number.MAX_SAFE_INTEGER + 1`), the pinned baseline `mergeSeqRanges` never terminates because `seq + 1` no longer advances a JS double. Spocky matches the JS-double seq arithmetic up to that point but terminates instead of hanging. | Accepted by the coordinator on 2026-10-01 as a safety divergence: reproducing it would hang the daemon. Only reachable with a seeded or persisted seq at 2^53. |
| DIV-003 | Hub public API `ToNumber` on a `length` property that is a nested array calls V8's recursive `Array.prototype.join`, which overflows its stack at a stack-dependent depth: pinned Hub `28f6c78` joins 4,400 levels and throws `RangeError` at 4,600. `spocky-hub-pilot` `public_api/validation.rs` `MAX_JOIN_DEPTH` fixes the boundary at 4,500, so depths 4,401 to 4,599 may differ from the baseline. Same family as DIV-001. | Accepted by the coordinator on 2026-10-01. The baseline boundary depends on the V8 stack and is nondeterministic. Differential cases stay at 4,400 (joins) and 4,600 (throws); depths in between are not compared. |
| DIV-004 | `spocky-session` `agent_manager/create.rs` maps a create-hook config nested past the zod port's recursion limit to `Outcome::TooDeep`, reported as the V8 `RangeError` "Maximum call stack size exceeded" the baseline throws. The text matches; the depth at which it triggers is the zod port's boundary, not V8's stack-dependent one, so configs near the boundary may be accepted by one side and rejected by the other. Same family as DIV-001. | Accepted by the coordinator on 2026-10-02. The baseline boundary depends on the V8 stack and is nondeterministic; differential cases stay well clear of it. |
| DIV-005 | Withdrawn. The locale modifier shapes that differed (a modifier with a non-variant piece sorting before a valid variant piece, such as `LANG=de@ab-cde-fghij`) are ported in `0f1f3537`: only the sorted run of variants before the first other piece stays a variant, and `-` and `_` separate pieces alike. No divergence remains; the shapes are pinned in `crates/spocky-contracts/tests/js_locale_differential.rs`. | Withdrawn by `p3_contracts` after the coordinator's trigger correction, which showed the rule. |
| DIV-006 | A locale modifier that repeats a piece which is not a BCP 47 variant (`LANG=de_DE@euro_euro`): pinned Node `22.20.0` throws `RangeError: Internal error. Icu error.` from `Intl.DateTimeFormat().resolvedOptions()`, `String.prototype.localeCompare`, and `Number.prototype.toLocaleString` under the default locale; `toLocaleLowerCase()` works and `spocky_contracts::locale::default_locale` returns the tag with the duplicate removed (`de-DE-x-lvariant-euro`). `spocky_contracts::js::locale_compare` does not read the default locale, so it does not throw either. A repeated variant (`@abcde_abcde`) does not throw in Node. | Accepted by the coordinator on correction. Not a realistic environment. Pinned by `repeated_modifier_piece_divergence_is_pinned` in `crates/spocky-contracts/tests/js_locale_differential.rs`. |

## Cloud baseline gaps

- Relay's 23,001-WebSocket Fly epoch has not run or received production certification.
- Relay has no in-VM watchdog for a wedged owner that remains in cluster membership.
- Fly rolling deployment does not activate relay drain state.
- Relay transports endpoint ciphertext but does not terminate encryption.
- Importer supports only Conductor, lacks commit-only recovery, and does not retry stale config writes.
- Billing omits invoices, tax, dunning UI, coupons, multi-currency, and alternative providers.
