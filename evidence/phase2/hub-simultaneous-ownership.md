# Hub simultaneous ownership qualification

Pinned baseline: `28f6c78833065fd282f9064f92a9aa61875dd359`.

Run:

```sh
gtimeout --kill-after=30 360 scripts/phase2/hub-simultaneous-ownership.test.sh
```

The harness archives the pinned baseline into a disposable read-only source copy. It verifies the baseline lock source contains exclusive `open(path, "wx", 0o600)`, ten bounded owner reads, and stale-file unlink recovery before reproducing those lock operations at controlled pause points.

Observed candidate-first direction: candidate paused before opening the owner file; baseline created and completed its owner record; candidate resumed and returned `directory-in-use`.

Candidate now replaces stale or incomplete owner inodes and claims the replacement with exclusive creation. Embedded SQL recovery, live-inode preservation, retained child inheritance, and parent-death recovery remain green.

Narrow exception: baseline paused after exclusive create but before writing its owner record. After candidate's ten 10 ms reads are exhausted, candidate must treat the incomplete record as stale to preserve pinned crash recovery. Candidate replaces the inode and opens. Baseline can then finish writing its still-open unlinked inode, so both processes believe they own the directory. No lock protocol compatible with both bounded incomplete-record recovery and an indefinitely paused writer can distinguish those states.

Port 6767 was untouched. No production, deployment, or publish command ran.
