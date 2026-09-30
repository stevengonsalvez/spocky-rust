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
