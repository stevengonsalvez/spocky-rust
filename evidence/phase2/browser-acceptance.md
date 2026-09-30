# Browser acceptance enforcement

The browser runtime capture exits nonzero unless desktop and mobile pass direct
zero-different-pixel, keyboard activation, dialog-outcome, accessibility, and
candidate-stability gates. It does not normalize, mask, or allowlist pixels.

Original and candidate captures activate the Add a project control. The
original resolves `open-project-submit`; the candidate resolves its first
semantic project action. The candidate now opens the same pinned Add Project
method dialog contract instead of replacing the outcome with live-status text.
The comparison records the dialog label, visible text, and semantic controls.

The contract test injects and rejects these regressions:

- desktop pixel mismatch
- mobile pixel mismatch
- mobile keyboard activation failure
- desktop accessible focus-label mismatch
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

The focused renderer tests and script contracts pass. The full browser capture
was not run. Browser runtime, macOS desktop, Linux desktop, Windows desktop,
iOS, and Android environment evidence remains open.
