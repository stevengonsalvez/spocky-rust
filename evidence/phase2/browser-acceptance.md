# Browser acceptance enforcement

The browser runtime capture now exits nonzero unless desktop and mobile both
pass exact-pixel, keyboard activation, and accessibility comparison gates.
Original and candidate captures both activate the Add a project control.
The original resolves `open-project-submit`; the candidate resolves its first
semantic project action. Accessible-name changes do not prevent activation.

The contract test injects and rejects these regressions:

- desktop pixel mismatch
- mobile pixel mismatch
- mobile keyboard activation failure
- desktop accessible focus-label mismatch

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
frames, focus scanning, activation, and screenshots. DOM probes report pending
stylesheets, fonts, images, and relevant requests. Request completion and
failure events are retained. Capture failures write the completed captures and
the failing capture instrumentation into the attempt comparison JSON before
exiting. Instrumentation adds no retry or acceptance condition.

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
The exact desktop gate therefore continues to reject the branded comparison.
Resolution still requires either an approved nondeterminism exception with an
explicit contract or a source-level fix that renders the icon deterministically.

## Checks

```text
PASEO_REFERENCE_ROOT=/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite sh scripts/phase2/browser-runtime-capture.test.sh
node --check scripts/phase2/browser-runtime-capture.cjs
sh -n scripts/phase2/browser-runtime-capture.sh
sh -n scripts/phase2/browser-runtime-capture.test.sh
```

The full browser capture was not run. Hub work may build concurrently. Browser
runtime, macOS desktop, Linux desktop, Windows desktop, iOS, and Android
environment evidence remains open.
