# Phase 2 retained PGlite review J

Reviewer: `p2_hub_retained_review_j`

Model: `gpt-6-astra`, xhigh

Mode: read-only

Frozen commit: `ef6639b`

## Verdict

The retained PGlite host is a defensible compatibility-runtime candidate. Keep
its exception `required-not-accepted`. It does not complete Hub storage parity
or Phase 2.

The reviewer independently verified all eight committed artifact hashes and
byte sizes, all 98 migration-file hashes against the pinned clean Hub commit,
the Node executable digest, 50 compared table names, 537 deduplicated
constraint and index names, and 49 migration journal rows. The evidence proves
its named catalog, journal, and narrow scenario results. It does not prove full
column and constraint-definition parity, integrated Hub mutations, callback
transactions, keyed locks, platform delivery, or IPC performance.

## Blockers

1. P1: lock creation writes the owner record after creating an empty file. A
   competing host can unlink that incomplete live lock and become a second
   owner. Add bounded grace and compare-safe reclamation with a concurrent race
   test.
2. P1: request timeout starts after blocking frame delivery. Bound writes as
   well as response receipt, fail closed, and cover a live child that stops
   reading an over-pipe-capacity allowed frame.
3. P1: the host acknowledges close before database close and lock release.
   Acknowledge completed shutdown and prove data-bearing close and reopen.
4. P1: JSON scalar tags and JSON null collapse into primitive or SQL-null
   values. Preserve JSON and JSONB distinctions and add round-trip coverage.
5. P2: crash, lost-reply, partial-migration, and old-state evidence is narrower
   than several prose claims. Add data-bearing cases and reduce remaining
   claims to observed behavior.
6. P2: retained scripts' default baseline path works in the worker worktree but
   not the integrated main checkout. Make documented commands reproducible
   without an unrecorded override.
7. P2: Node and PGlite identities are observed but not enforced at adapter
   open. Platform packaging, signing, updates, support ownership, and IPC
   performance remain unqualified exception-acceptance work.
