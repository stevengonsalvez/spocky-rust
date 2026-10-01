# Browser acceptance enforcement

The browser runtime capture exits nonzero unless desktop and mobile pass direct
zero-different-pixel, keyboard activation, dialog-outcome, accessibility, and
candidate-stability gates. It does not normalize, mask, or allowlist pixels.

Original and candidate captures activate the Add a project control. The
original resolves `open-project-submit`; the candidate resolves its first
semantic project action. The candidate now opens the same pinned Add Project
method dialog contract instead of replacing the outcome with live-status text.
The comparison records the dialog label, visible text, and semantic controls.
Focused renderer contracts pin the candidate to the latest observed original
DOM semantics. Attempt `20261001T001233Z-50366` verifies the exact dialog
outcome in both viewports and the exact mobile accessibility cycle.

The contract test injects and rejects these regressions:

- desktop pixel mismatch
- mobile pixel mismatch
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

## Desktop baseline evidence

Attempt `20261001T001233Z-50366` verifies exact interaction in both viewports,
zero mobile pixel difference, zero mobile accessibility difference, and zero
candidate same-page and fresh-context instability. Its original and candidate
desktop focus payloads are byte-for-byte equal. Both include the pinned
source's unnamed focusable `div`. The former completeness predicate rejected
that valid source entry because it required a label or text. The focused
contract now accepts complete unnamed entries, rejects missing captured fields,
and retains exact whole-payload equality. A later full capture has not verified
the corrected gate.

The same attempt has 19 desktop pixel differences at `x=20..28`, `y=46..57`,
inside the History icon. The candidate path data already matched pinned
`lucide-react-native` 0.546.0. The renderer now emits the pinned root and child
SVG presentation attributes directly. Focused renderer contracts pin those
attributes. A later full capture has not verified the icon repair. No mask,
threshold, normalization, or visual exception is applied.

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
The exact desktop gate therefore continues to reject both recorded branded
comparisons. No visual exception or allowlist is implemented.

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
