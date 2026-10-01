# Spocky Rust Exact Parity

This repository rebuilds Paseo-owned implementation surfaces as Spocky in Rust
against four immutable source baselines. Behavior, compatibility, state,
visuals, interactions, accessibility, performance, packaging, failure recovery,
and supported platforms must match before feature work begins.

## Product identity amendment

Stevie selected **Spocky** as the rewrite product identity on 2026-09-30 after
being informed of existing `spocky.ai` overlap. This is an authorized branding
exception to text and visual parity, not legal clearance or permission to
publish, buy a domain, rename remotes, or create mascot assets.

Task `BRAND-SPOCKY-001` owns the coordinated rename. Its safe boundary is after
the active exact-browser and embedded-schema checkpoints integrate. Cross-cutting
writers pause there. The lead then serially renames owned crates, modules,
packages, binaries, scripts, current commands, UI strings, and help text from
`paseo-*` or `paseo_*` to `spocky-*` or `spocky_*`.

That boundary closed at commit `4cce774259c974fb44ae78520488938a793ed5f8`.
All 14 current Cargo packages, Rust imports, owned binaries, commands, and new
product-facing text now use Spocky. Compatibility identifiers remain unchanged
and are enumerated in `porting/spocky-branding-inventory.md`.

Every remaining Paseo name is classified before change:

| Class | Treatment |
|---|---|
| Owned implementation | Rename to Spocky at the serialized boundary |
| Compatibility contract | Inventory, migrate, and test before changing |
| Upstream attribution or provenance | Preserve exact Paseo name and source |
| Historical evidence | Preserve command, capture, digest, and observation |
| Physical orchestration path | Keep stable while workers and goal run |

New product-facing identity defaults to Spocky. Existing `PASEO_*` inputs,
legacy state, protocol and wire names, crypto contexts, deep links, cookies,
auth and storage keys remain compatible until an explicit migration proves old
and new behavior. Never move or delete user state for branding. Branded evidence
is captured separately from frozen original baselines.

## Immutable baselines

| System | Commit | Local source |
|---|---|---|
| Paseo | `5de45e208690b0efc51c59a585ae9729325a9204` | `PASEO_REFERENCE_ROOT` or sibling `../paseo-rewrite` |
| Hub | `28f6c78833065fd282f9064f92a9aa61875dd359` | `.baselines/hub` |
| Distributed relay | `3fc41c96c8c63f3a7109e832899cc57d473c4531` | `.baselines/relay` |
| Importer | `8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5` | `.baselines/import` |

Never compare against a moving branch. Baseline directories are read-only inputs and are excluded from Git.

## Provision the baselines

Set `PASEO_REFERENCE_ROOT` only when the Paseo reference is not the sibling `paseo-rewrite` checkout. Verify its commit before any comparison:

```sh
git -C "${PASEO_REFERENCE_ROOT:-../paseo-rewrite}" rev-parse HEAD
```

Provision the other pinned sources inside this repository, then detach each checkout at its immutable commit:

```sh
git clone https://github.com/getpaseo/hub.git .baselines/hub
git -C .baselines/hub checkout --detach 28f6c78833065fd282f9064f92a9aa61875dd359
git clone https://github.com/getpaseo/paseo-relay.git .baselines/relay
git -C .baselines/relay checkout --detach 3fc41c96c8c63f3a7109e832899cc57d473c4531
git clone https://github.com/getpaseo/import.git .baselines/import
git -C .baselines/import checkout --detach 8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5
cargo test -p spocky-baseline --test pinned_sources
```

If a repository URL changes, copy an existing checkout into the matching `.baselines/` path and detach it at the recorded commit. Never substitute a newer commit.

## Execution order

1. Inventory every observable baseline capability and known defect.
2. Freeze protocol, binary, crypto, state, CLI, UI, platform, and failure fixtures.
3. Build the original-versus-Rust differential harness.
4. Complete contract and tool-selection pilots.
5. Prove the unchanged-client Codex vertical slice.
6. Expand by the dependency-ready task DAG.
7. Qualify every platform and mixed-version path.
8. Pass milestone and final adversarial review gates.

## Evidence contract

Every scenario retains raw and normalized outputs. Normalization is limited to documented generated IDs, wall-clock values, and temporary paths. Behavior, ordering, missing versus null semantics, errors, frames, state transitions, visuals, assertions, fixture counts, and executed-test counts are never normalized.

Tracked evidence manifests live under `evidence/`. Large raw captures live under `evidence/raw/` and remain untracked. Each manifest records its raw artifact digest and reproducible command.

## Safety

- Never start, stop, or restart the production daemon on port `6767`.
- Use disposable homes, ports, repositories, databases, accounts, credentials, and keys.
- Do not mutate production Hub, relay, provider, billing, deployment, release, or store state.
- Run long-lived services in named tmux sessions and stop exact identities only.
- Run targeted local tests. Full matrices belong in CI.
- Preserve all four baseline checkouts without edits.

## Build, test, run, and rollback

- Build current pilot crates with `cargo build --workspace`. No parity-qualified Spocky service binary exists yet.
- Run only the targeted commands recorded in `porting/tasks.json`. Full platform matrices belong in CI after their jobs exist.
- Run future services with a disposable home, random non-6767 port, and named tmux session. Record the exact home, port, session, commit, and log path in evidence before launch.
- Stop only the recorded tmux session. Delete only its exact disposable home after evidence capture.
- Roll back by checking out the last signed verified checkpoint and restoring the scenario's disposable state snapshot. Never roll back a production instance during parity work.
- Cloudflare relay, Hub, delivery, installers, and update paths remain owned implementation scope. Production deployment is outside this execution session.

## Routing and ownership

The lead owns shared contracts, the root Cargo workspace, cross-cutting schemas, the task ledger, integration, and shared evidence. Writers use isolated worktrees, branches, and exclusive paths. Every writer is explicitly assigned `gpt-5.6-sol` at medium effort unless a recorded blocker requires Sol high or xhigh. Astra is read-only and limited to adversarial review of frozen completed commits.

The Orca orchestration runtime was unavailable at execution start after one approved launch attempt. Runner-native goal and worker metadata are retained as routing evidence. This limitation does not prove an unobserved model assignment.

Routing revision, 2026-10-01: Stevie directed that all sessions run in Claude,
not Codex, from 2026-10-01 through 2026-10-03. This supersedes the Sol writer
rule for that window only. A Claude lead (`claude-opus-5-5`) replaces the
retired Sol lead and keeps the same ownership. A separate Claude coordinator
launches writer lanes. Astra stays read-only and is not used in the window.
Success criterion 3 requires Sol to author all fixes, so every Claude-authored
commit in the window carries a recorded evidence limitation. Details live in
`routing.revisions` in `porting/tasks.json`.

Integration workflow, 2026-10-01: writers make very small commits that each
change one file. The lead runs the code-review skill on each writer commit
range, routes fixes back to the writer, then cherry-picks signed commits to
main at every verified checkpoint, not only at lane end. Lead ledger and docs
commits follow the same one-file rule. The objective is exact like-for-like
parity with Paseo, nothing more.

## Task states

Tasks move through `ready`, `implementing`, `verifying`, `reviewing`, `integrating`, and `done`. A blocked task names its exact unmet dependency and evidence. The durable ledger is [`porting/tasks.json`](porting/tasks.json).

## Current boundary

Phase 1 inventory and architecture freeze passed paired adversarial review at
signed commit `2fa22be761a0fbec42fd759df69bff37e248824c`. Phase 2
differential harness and pilots are implementing. Repeated adversarial reviews
remain rejected while repairs and runtime evidence continue.

The branded empty-project Chromium gate accepts exact full-image membership,
interaction, accessibility, candidate stability, and failure isolation at
commit `500d4bcb450ba8ca8aaa05f33da46b6752b3b2e5`. Mobile is exact. Desktop
candidate captures are byte-identical across same-page and fresh contexts and
match one of two pinned complete baseline images. The two baseline images differ
by 19 Plus-icon pixels with no distinguishing readiness signal. No mask,
normalization, threshold, or runtime exception is used. The shared
offline-reload failure remains pinned behavior.
Hub retained-PGlite evidence matches 50 table names, 537 constraint and index
names, 49 journal rows, and five narrow scenarios. The retained JavaScript host
uses one shared OS-backed data-directory lock across the Rust storage adapters.
Its 17 retained-host and 13 embedded-SQL tests pass, but the exception remains
unaccepted. Read-only review accepts narrow darwin/x64 cooperating-host
ownership and retains packaging, platform, IPC performance, delivery,
callback-transaction, keyed-lock, full-schema, and mixed-legacy gaps.
Real pinned-baseline forward handoff and same-schema baseline reopen pass with
exact marker payloads and no journal change. The hardened harness enforces those
values before publication and bounds long commands with forced-kill fallbacks.
Read-only review accepts this same-schema handoff checkpoint. A bounded
cross-version harness also proves additive future-journal handoff in both
directions and atomic rollback of a failed candidate migration batch. Neither
runtime validates journal prefixes or hashes, so destructive and semantic
future migrations remain unqualified. Simultaneous mixed-owner races and the
retained compatibility exception remain open.
Ordered mixed legacy/candidate starts now exclude the second owner in both
directions, and clean handoff preserves the same directory, 49-row journal, and
baseline marker. Publication derives both observed journal counts, requires zero
candidate migrations, and rejects unclean owner shutdown. Signal cancellation
shares bounded detached-group cleanup across readiness rejection. Read-only
review accepts this narrow macOS x64 ordered-start checkpoint. Simultaneous
pre-record races now have a narrow source-and-evidence checkpoint. Exclusive
candidate creation prevents replacement of a completed live legacy owner, and
the canonical harness passes strict signal cleanup, baseline integrity, and
bounded execution gates. A paused incomplete legacy writer and a stale-unlink
TOCTOU still permit dual live owners in deterministic operation models. Both
remain unaccepted compatibility-exception candidates. Windows and pinned
database runtime for these two races remain unqualified. General schema
downgrade and full parity remain unqualified.
The selected
plugin wrapper passes settings migration success and failure plus binary IPC.
Its selected 25-case matrix passes on macOS and pinned Linux. Windows MSVC
all-target compilation passes, but native Windows runtime qualification remains
open. The selected relay runtime passes control,
pairing, capacity, readiness, metrics, discovery, and node-loss cases; bounded
loopback frame work now matches whole-document JSON classification, Jason
duplicate and numeric boundaries, deep opaque nesting, fragmented data and
control limits, exact close reasons, and unrelated-route continuity. Read-only
review accepts this narrow checkpoint. Production TLS, non-loopback, load, and
deployment qualification remain open. Full renderer
platforms, cross-platform plugin clients, and native audio cases remain
incomplete. Audio passes 10 targeted macOS cases and Windows MSVC all-target
compilation. Unsigned macOS delivery lifecycle and retained Linux AppImage and
deb lifecycle pass. Linux ALSA null playback and deterministic null capture pass
in a pinned container. Its runner rejects symlinked output anchors, bounds and
verifies exact container cleanup, reaps its process group, and bounds the caller
wait when a descendant escapes that group while retaining pipes. Physical
audio, PulseAudio and PipeWire server graphs,
STT, and TTS remain open. Signed delivery and native Windows, iOS, Android, and
browser delivery remain open. Phase 3 remains blocked. No runtime parity
milestone is complete. On 2026-10-01 Stevie accepted the Hub retained
JavaScript PGlite host as a time-boxed interim compatibility exception
(`accepted-interim`) with an unchanged removal condition. Lane
`p2_pglite_rust_host` builds the Rust Wasm host that replaces it. No other
runtime compatibility exception is accepted.
