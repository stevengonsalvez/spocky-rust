# Phase 2 retained PGlite review J

Reviewer: `p2_hub_retained_review_j`

Model: `gpt-6-astra`, xhigh

Mode: read-only

Latest frozen commit: `b3ce81347fdacf2dcec1afb3a668b138f9e05638`

## Verdict

The narrow darwin/x64 ownership repair is accepted for cooperating new-protocol
hosts. No remaining P1 concurrent-owner schedule was found. Keep the retained
PGlite exception `required-not-accepted`. It does not complete Hub storage
parity or Phase 2.

The reviewer independently verified all eight retained-host artifact hashes and
byte sizes, the refreshed 17-case test log, all 98 migration-file hashes, the
Node executable digest, 50 compared table names, 537 deduplicated constraint
and index names, and 49 migration journal rows.

The reviewer also accepted the captured same-schema legacy handoff and its
hardened publication gate. The runner rejects baseline mutation, non-49/0/49
forward journal state, payload mismatch, and supported reverse-journal mismatch
before copying evidence. Long commands have forced-kill deadlines. No P0, P1,
or P2 finding remains in this narrow checkpoint.

## Closed findings

1. The shared OS guard is acquired before metadata changes and is never renamed
   or unlinked by cooperating hosts.
2. Node inherits a duplicate guard handle before database initialization. A
   stopped Node child keeps exclusion after abrupt Rust parent death.
3. Normal close releases the parent guard after confirmed child reap. The child
   handle preserves exclusion if cleanup cannot prove termination.
4. Bounded request delivery, response timeout, close ordering, JSON and JSONB
   tags, SQL null distinction, rollback observations, and integrated baseline
   path repairs remain verified.

## Remaining boundary

- Simultaneous pinned legacy pathname-only ownership and new OS-guard ownership
  is unqualified.
- Candidate-generated historical-schema resume, synthetic legacy metadata,
  baseline-produced same-schema directory handoff, and same-schema reverse
  reopen pass. Schema downgrade and mixed-owner handoff remain unqualified.
- Node packaging and platform availability, framed IPC performance, retained
  JavaScript delivery and support, callback transactions, keyed application
  locks, and full schema-definition provenance remain unqualified.
- The compatibility exception remains `required-not-accepted`.
