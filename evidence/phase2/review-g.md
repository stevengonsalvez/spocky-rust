# Phase 2 Spocky gap review G

Reviewer `p2_gap_review_spocky` used `gpt-6-astra` at `xhigh` in read-only
mode. Frozen candidate `25bf2b7a1e80ad438589181d228913e801a9e855` was clean before
review. No files were edited, tests or builds run, services launched, or work
delegated.

Verdict: rejected.

Closure gates, in recommended order:

1. Hub storage must use a compatible engine and dialect, inspect the installed
   schema, replay historical migrations, reopen old state, survive crashes, and
   match embedded and PostgreSQL mutation traces. Current SQLite storage still
   persists whole-state bytes, installs a final schema instead of replaying the
   migration history, and accepts 17 constraint mismatches.
2. Browser acceptance must fail on injected pixel, interaction, and
   accessibility differences. Fresh-process runs must establish repeatability,
   followed by launch, visual, input, accessibility, performance, and package
   evidence on the six required platforms. The separate branded capture has
   zero desktop and mobile RMSE, but the harness records rather than enforces
   that result.
3. The selected plugin wrapper must support settings migration and binary IPC,
   then qualify update recovery, restart, descendant cleanup, and real client
   contributions across required platforms. Node retention has no accepted
   compatibility exception.
4. Relay qualification still needs the control protocol, identifier limits,
   capacity ledger, discovery, readiness, metrics, topology handling, and
   selected Linux behavior under bounded load.
5. Audio, native, and delivery need selected-app capture and playback,
   permissions, interruption and device changes, speech readiness, native
   callbacks and deep links, plus platform install, update, failure, rollback,
   and state-retention comparisons. Phase 2 permits explicitly unsigned
   artifacts and does not require deployment or paid services.

Hub, plugin, and relay repairs are independent. Platform environment
provisioning can proceed in parallel. App-level native and delivery evidence
depends on selected renderer shells. Phase 3 remains blocked until every Phase
2 gate is accepted by frozen-candidate review.

The shared offline reload failure remains classified as pinned behavior, not a
port regression. Existing upstream evidence and branded evidence stay separate.
