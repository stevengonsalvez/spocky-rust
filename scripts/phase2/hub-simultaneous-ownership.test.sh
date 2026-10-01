#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-simultaneous-test.XXXXXX")
trap 'gtimeout 30 rm -f "$output"' EXIT HUP INT TERM

gtimeout --kill-after=30 360 "$repository_root/scripts/phase2/hub-simultaneous-ownership.sh" >"$output"
jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.disposableSourceReadOnly == true
  and .candidatePausedBeforeOwner.pause.event == "paused-before-owner-open"
  and .candidatePausedBeforeOwner.baselineOpened == true
  and .candidatePausedBeforeOwner.candidateRejected.error == "directory-in-use"
  and .candidatePausedBeforeOwner.candidateExit == {"code": 0, "signal": null}
  and .baselinePausedAfterExclusiveCreate.emptyOwnerFileCreated == true
  and .baselinePausedAfterExclusiveCreate.candidateReady.opened == true
  and .baselinePausedAfterExclusiveCreate.baselineWriteCompleted == true
  and .baselinePausedAfterExclusiveCreate.distinctInodes == true
  and .baselinePausedAfterExclusiveCreate.visibleOwnerProtocol == "os-file-lock-v1"
  and .baselinePausedAfterExclusiveCreate.bothProcessesLive == true
  and .baselinePausedAfterExclusiveCreate.candidateExit == {"code": 0, "signal": null}
  and .conclusion.exclusiveCandidateCreate == "implemented"
  and .conclusion.pausedBaselineAfterCreate == "inherent-unresolved-race"
' "$output" >/dev/null
gtimeout 30 cat "$output"
