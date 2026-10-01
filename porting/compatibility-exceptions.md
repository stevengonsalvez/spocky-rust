# Compatibility Runtime Exceptions

No compatibility runtime exception is accepted.

An exception requires capability, owner, exact runtime and version, boundary, exchanged data, pilot evidence, security and packaging risk, performance and support risk, original-versus-candidate tests, platforms, removal condition, and review date.

## Harness-only tools

`scripts/phase2/relay-ops-tls-proxy.py` is a local differential-test TLS edge
proxy. It is excluded from shipped runtime ownership and is not a compatibility
runtime exception. Replace it with Rust only if test-harness ownership becomes
part of the release artifact.

## Candidates

Neither candidate below is accepted for release or parity.

### Hub paused incomplete legacy owner

- Status: candidate only
- Capability and owner: mixed Hub data-directory ownership, Hub storage
- Runtime: pinned baseline `28f6c78833065fd282f9064f92a9aa61875dd359` and Rust `spocky-hub-pilot` at `5d29c4a5b1fe68498f43f7e6b3d61d74e76b533f`
- Boundary and data: `.paseo-hub.lock` containing legacy `pid` and `token` JSON or candidate `os-file-lock-v1` JSON
- Evidence: `evidence/phase2/hub-simultaneous-ownership-report.json`, `baselinePausedAfterExclusiveCreate`
- Risk: candidate may replace an incomplete live legacy owner and permit two live owners; corruption and recovery impact remain unqualified
- Performance and support: ten 10 ms legacy reads are bounded, but no safe mixed-start guarantee exists
- Differential coverage: handwritten pinned lock-operation model against the real candidate; pinned database runtime is not executed for this race
- Platforms: macOS x64 evidence only; Windows unqualified
- Removal condition: atomic cross-runtime claim protocol or removal of supported concurrent mixed-version starts
- Review date: 2026-10-01

### Hub stale-unlink live-owner replacement

- Status: candidate only
- Capability and owner: mixed Hub data-directory ownership, Hub storage
- Runtime: pinned baseline `28f6c78833065fd282f9064f92a9aa61875dd359` and Rust `spocky-hub-pilot` at `5d29c4a5b1fe68498f43f7e6b3d61d74e76b533f`
- Boundary and data: `.paseo-hub.lock` inode identity and owner JSON
- Evidence: `evidence/phase2/hub-simultaneous-ownership-report.json`, `completedLiveRecordStaleUnlinkToctou`
- Risk: path unlink may delete a newly completed live legacy record and permit two live owners; corruption and recovery impact remain unqualified
- Performance and support: identity recheck narrows the race but cannot make unlink conditional and atomic
- Differential coverage: deterministic handwritten operation model; pinned database runtime is not executed for this race
- Platforms: macOS x64 evidence only; Windows unqualified
- Removal condition: conditional atomic deletion primitive shared by both runtimes or removal of supported concurrent mixed-version starts
- Review date: 2026-10-01
