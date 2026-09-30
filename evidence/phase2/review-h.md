# Phase 2 browser-mode review H

Reviewer `p2_browser_mode_review` used `gpt-6-astra` at `xhigh` in read-only
mode against frozen commit `a14ac71`. No files were edited, tests or builds run,
services launched, or work delegated.

Verdict: reject browser parity acceptance.

The candidate desktop image exactly matches one genuine pinned-original mode,
and every mobile image is exact. A two-mode desktop visual contract is
defensible only as an explicit, narrowly scoped amendment with pinned runtime
conditions, complete-image SHA-256 membership, consecutive candidate-frame
stability, no masks or thresholds, and no automatic golden expansion.

Current evidence does not qualify that amendment or browser parity:

- upstream opens a dialog while the candidate changes status text; the harness
  compares only generic changed booleans
- accessibility comparison samples four focus entries rather than the complete
  relevant state
- one candidate frame per viewport does not prove internal stability
- the A/B upstream run predates instrumentation, so readiness equivalence is
  not proved

Interaction outcome, complete accessibility state, and candidate repeat
stability must be repaired before another review. A third visual mode,
candidate instability, divergent interaction or accessibility, or evidence
that the two modes represent distinct product states falsifies the proposed
visual amendment.
