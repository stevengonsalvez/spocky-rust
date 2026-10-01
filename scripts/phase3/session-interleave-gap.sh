#!/bin/sh
# Reruns the cross-agent same-tick interleave check recorded in
# evidence/phase3/session-interleave-gap.md: two agents' sessions emit
# events alternately in one synchronous loop, and the agent manager's
# subscriber feed order must match the pinned Paseo build's.
#
# The check is the "interleave" scenario of
# crates/spocky-session/tests/agent_manager_differential.rs, kept as
# evidence/phase3/session-interleave-gap.patch because it fails while the
# gap is open. This script applies the patch in a disposable git worktree
# at HEAD, runs that test, and removes the worktree (by its exact path).
#
# Needs SPOCKY_PINNED_NODE (node v22.20.0) and SPOCKY_PASEO_DIST (the pinned
# packages/server/dist/server). Exit 0: the feed orders match and the gap
# can close. Exit 1: they still differ (the test prints both orders).
set -eu

: "${SPOCKY_PINNED_NODE:?set SPOCKY_PINNED_NODE to the pinned node binary}"
: "${SPOCKY_PASEO_DIST:?set SPOCKY_PASEO_DIST to the pinned dist/server}"

root=$(git rev-parse --show-toplevel)
patch="$root/evidence/phase3/session-interleave-gap.patch"
tree=$(mktemp -d "${TMPDIR:-/tmp}/spocky-interleave-gap.XXXXXX")
rmdir "$tree"
git -C "$root" worktree add --quiet --detach "$tree" HEAD
cleanup() { git -C "$root" worktree remove --force "$tree"; }
trap cleanup EXIT

git -C "$tree" apply "$patch"
status=0
(cd "$tree" && cargo test --locked -p spocky-session --test agent_manager_differential \
  scenarios_match_pinned_manager) || status=1
if [ "$status" -eq 0 ]; then
  echo "session-interleave-gap: feed orders match; the gap can close"
else
  echo "session-interleave-gap: feed orders still differ; the gap is open"
fi
exit "$status"
