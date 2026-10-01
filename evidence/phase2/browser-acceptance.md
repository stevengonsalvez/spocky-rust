# Browser acceptance enforcement

The browser runtime capture exits nonzero unless desktop and mobile pass exact
full-PNG hash, keyboard activation, dialog-outcome, accessibility, and
candidate-stability gates. It does not normalize, mask, compare by RMSE, retry
until a preferred image appears, or accept a pixel-count threshold.

Original and candidate captures activate the Add a project control. The
original resolves `open-project-submit`; the candidate resolves its first
semantic project action. The candidate now opens the same pinned Add Project
method dialog contract instead of replacing the outcome with live-status text.
The comparison records the dialog label, visible text, and semantic controls.
Focused renderer contracts pin the candidate to the latest observed original
DOM semantics. Attempt `20261001T001233Z-50366` verifies the exact dialog
outcome in both viewports and the exact mobile accessibility cycle.

The contract test injects and rejects these regressions:

- third desktop candidate hash
- mobile candidate hash mismatch
- candidate hash instability
- browser, OS, dependency, source, or viewport drift
- automatic golden-set expansion
- mobile keyboard activation failure
- desktop accessible focus-label mismatch
- malformed keyboard focus entry
- incomplete keyboard focus cycle
- dialog shape or content mismatch
- consecutive same-page candidate instability
- fresh-browser-context candidate instability
- a nonzero pixel count mislabeled as passing

The validation result retains `shared-pinned-failure` when both runtimes fail
offline reload. That pinned behavior does not become a candidate regression.
Default and branded evidence stems remain separate.

Each full run writes into a timestamped `<stem>-attempts` directory. Failed
attempts retain their logs, partial captures, and `attempt.json` failure status.
Canonical comparison JSON and PNGs update only after every acceptance gate
passes. An accepted attempt retains the previously published artifacts beside
its attempt evidence.

Each capture also records non-compared instrumentation with monotonic offsets.
Checkpoints cover navigation, meaningful text, font readiness, two animation
frames, complete keyboard focus-cycle scanning, activation, and screenshots.
DOM probes report pending
stylesheets, fonts, images, and relevant requests. Request completion and
failure events are retained. Capture failures write the completed captures and
the failing capture instrumentation into the attempt comparison JSON before
exiting. Instrumentation adds no retry or acceptance condition.

Candidate screenshot stability has two independent zero-pixel gates per
viewport. One compares consecutive screenshots of the same page. The other
compares the accepted screenshot with a new browser context. Original repeat
captures remain diagnostic evidence for the two observed upstream modes and do
not create a visual exception.

The visual contract is limited to empty-project Chromium at desktop `1280x800`
and mobile `390x844`. It pins Google Chrome `154.0.8037.59` at its absolute
executable path, macOS `15.7.3` build `24G419`, Darwin `24.6.0` x86_64, scale
factor `1`, loaded system fonts, light theme, `en-US`, dependency lock hashes,
and source and harness commits. A clean tracked tree is required so those
commits identify the captured code.

Desktop candidate PNGs must equal exactly one of these complete-file SHA-256
values:

- `fad844b57077bcdbed0c93db7de03e5811243049ef7b6b284dbb2a8286a6480f`
- `597095777e1d610387667c732b7c08624e4f135a6064e1b1b739ec1342f4dc7d`

Mobile candidate PNGs must equal
`37ff2c272ad311efe1fc2e22df94ecb75af3a5f74a47b2ee6c7b356e58d99075`.
Every same-page and fresh-context candidate frame must have the same hash as its
accepted viewport frame. The contract rejects any third hash. Expanding the set
requires an explicit code and evidence change.

## Desktop baseline evidence

Attempt `20261001T002951Z-91995` verifies exact interaction and complete
accessibility in both viewports, zero mobile pixel difference, and zero
candidate same-page and fresh-context instability. Its original and candidate
desktop focus payloads are byte-for-byte equal. Both include the pinned
source's unnamed focusable `div`. The structural completeness gate accepts that
entry, rejects missing captured fields, and retains exact whole-payload
equality.

The same attempt has 19 desktop pixel differences at `x=20..28`, `y=46..57`.
Its candidate desktop image is exact upstream mode B, `59709577...f4dc7d`.
Original desktop is exact upstream mode A, `fad844b5...480f`. Candidate desktop
same-page and fresh-context frames are exact mode B. Every mobile image is
the exact pinned mobile hash.

Attempt `20260930T235823Z-10020` cleanly reached every acceptance gate. Candidate
same-page and fresh-context screenshots had zero differing pixels in both
viewports. Mobile matched the original at zero pixels. Desktop differed by 19
pixels. Direct difference coordinates are `x=20..28`, `y=46..57`, inside the
History icon row. No mask or exception is applied.

The same attempt rejected candidate interaction and accessibility. Its candidate
dialog used a label, a different GitHub subtitle and key hint, and `div`
controls. Its focus cycle used different footer labels, menu controls, card
roles, and community link elements. Focused contracts now pin the candidate to
the recorded original values. This remains unverified by a later full capture.

Two rejected branded attempts show two original desktop pixel modes. Attempt
`20260930T225932Z-86610` captured `fad844b5` first and `59709577` on repeat.
Attempt `20260930T231716Z-32848` captured `fad844b5` for both original passes.
The candidate captured `59709577` in both attempts. The modes differ by a
normalized RMSE of `0.0000847864`, localized to the New workspace plus icon.

In the instrumented attempt, both original desktop screenshot probes reported
328 text characters, 11 loaded stylesheets, loaded fonts, two observed animation
frames, no images, and no pending requests. Interaction and accessibility passed
for both desktop runtimes. Original, repeated original, and candidate mobile
screenshots were identical.

No recorded readiness probe distinguishes the two original desktop pixel modes.
The narrow full-image contract accepts either complete upstream mode while
rejecting any third rendering. No visual mask, normalization, threshold, or
automatic golden expansion is implemented.

## Checks

```text
PASEO_REFERENCE_ROOT=/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite sh scripts/phase2/browser-runtime-capture.test.sh
node --check scripts/phase2/browser-runtime-capture.cjs
sh -n scripts/phase2/browser-runtime-capture.sh
sh -n scripts/phase2/browser-runtime-capture.test.sh
cargo test -p spocky-ui-renderer-pilot --test shell -- --test-threads=1
```

The focused renderer tests and script contracts pass. No full browser capture
was run after the latest semantic repair. Browser runtime, macOS desktop, Linux
desktop, Windows desktop, iOS, and Android environment evidence remains open.
