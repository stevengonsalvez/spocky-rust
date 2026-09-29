# Phase 1 Check Evidence

## Contract cases

- Four immutable baseline commits verify.
- Modified tracked baseline content is rejected.
- Untracked reference `logs/` and `.agents/goals/` remain permitted and untouched.
- Every capability source and test path resolves inside its owning pinned checkout.
- Every capability dependency resolves and the graph is acyclic.
- Capability IDs are unique.
- Rust formatting, lint, targeted tests, and diff whitespace checks pass.

## Commands

```text
$ cargo fmt --check
exit 0

$ cargo clippy -p paseo-baseline --tests -- -D warnings
Finished `dev` profile [unoptimized + debuginfo]
exit 0

$ cargo test -p paseo-baseline --test pinned_sources
running 2 tests
test verifies_all_four_immutable_source_commits ... ok
test rejects_a_baseline_with_modified_tracked_content ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
exit 0

$ ruby scripts/validate_inventory.rb
capabilities=74 unique_ids=74 dependency_dag=valid
source_and_test_paths=valid detail_records=9248
exit 0

$ jq empty porting/tasks.json
exit 0

$ git diff --check
exit 0
```

Checked on 2026-09-29 against signed baseline-verifier repair `419848354b6299b74282aee9db99688284e7486f`. The three mounted secondary baselines had no changes. The Paseo reference had only the pre-existing untracked `.agents/goals/` and `logs/` paths.
