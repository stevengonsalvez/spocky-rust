# Hub simultaneous ownership qualification

Pinned baseline: `28f6c78833065fd282f9064f92a9aa61875dd359`.

Run:

```sh
gtimeout --kill-after=30 1500 scripts/phase2/hub-simultaneous-ownership.test.sh
```

The 1500-second top-level bound covers the 1220-second conservative failure envelope, including signal fixtures, the 1020-second ownership wrapper, escalation, and final cleanup.

Default baseline discovery supports the canonical checkout's `.baselines/hub` and the task worktree's sibling canonical checkout. Missing paths and distinct dual matches fail closed before the pinned commit and clean-tree gates run.

The harness archives the pinned baseline into a disposable read-only source copy. Exact source hash and spelling gates cover exclusive `open(path, "wx", 0o600)`, ten 10 ms owner reads, live-PID rejection, and path unlink. The harness executes a handwritten model of those lock operations. It does not execute the pinned database runtime. Accepted ordered runtime evidence remains in `hub-mixed-ownership-report.json`.

Observed candidate-first direction: candidate process paused before lock acquisition; modeled baseline created a completed live owner record; candidate resumed, preserved the record's inode and content, returned `directory-in-use`, and exited with code 0. A crate-private unit-test hook separately pauses actual candidate acquisition after the guard and immediately before owner creation. That test also preserves the completed live owner inode and content.

Candidate now replaces stale or incomplete owner inodes and claims the replacement with exclusive creation. Embedded SQL recovery, live-inode preservation, retained child inheritance, and parent-death recovery remain green.

Residual exception one: modeled baseline pauses after exclusive create but before writing its owner record. Candidate exhausts ten 10 ms reads and must treat the incomplete record as stale to preserve pinned crash recovery. Candidate replaces the inode and opens. Baseline can then finish writing its still-open unlinked inode, so both processes believe they own the directory.

Residual exception two: candidate reads stale inode A; modeled baseline unlinks A, creates completed live inode B, and writes a live owner; candidate's pending path unlink deletes B and creates inode C. Identity snapshots prove A, B, and C are distinct while both modeled owners are live. Rechecking identity narrows this window but cannot make path unlink conditional and atomic with the pinned protocol.

Parity is not claimed for either residual exception. Signal cleanup regression requires distinct validated fixture PIDs, exact HUP 129 and TERM 143 exits, and no shell fallback while removing the detached owner and stopped descendant.

Port 6767 was untouched. No production, deployment, or publish command ran.
