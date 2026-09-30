# Browser acceptance enforcement

The browser runtime capture now exits nonzero unless desktop and mobile both
pass exact-pixel, keyboard activation, and accessibility comparison gates.
Original and candidate captures both activate the Add a project control.

The contract test injects and rejects these regressions:

- desktop pixel mismatch
- mobile pixel mismatch
- mobile keyboard activation failure
- desktop accessible focus-label mismatch

The validation result retains `shared-pinned-failure` when both runtimes fail
offline reload. That pinned behavior does not become a candidate regression.
Default and branded evidence stems remain separate.

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
