# Phase 2 exit review A

Reviewer `p2_exit_review_a` used `gpt-6-astra` at `xhigh` in read-only mode.
Observed approval policy was `never`, sandbox was `danger-full-access`, and network
was enabled. No files were edited and no work was delegated.

Frozen candidate `20e92abc6589ecbc46d8cac7f97faba1b30abd70` was rejected.

Release blockers found:

- equal requested-capture errors could pass the differential harness;
- stored agent directory names differed for Windows and trailing-slash paths;
- state and lifecycle pilots did not execute original-versus-Rust scenarios;
- Hub, plugin, relay, browser, audio, native, and delivery pilots lacked required runtime evidence;
- renderer and compatibility tools remained unselected;
- differential child processes had no deadline;
- iOS, Windows, Linux, baseline-visual, native, and delivery evidence remained incomplete.

The harness capture-error defect, stored-path defect, and process deadline were
repaired by Sol after this frozen review. Review remains rejected until every
other blocker is repaired and the completed candidate is re-reviewed.
