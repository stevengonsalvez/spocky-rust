# Relay differentials on Linux

The capacity and writer differentials were run in GitHub Actions, outside the lane's machine, so
the pinned Elixir side is reproduced independently.

- Run: https://github.com/stevengonsalvez/spocky-rust/actions/runs/37229794787 (branch
  `ci/p4_relay/relay-differential`, commit `5cac7615`, workflow `relay-differential`), conclusion
  success.
- Runner: `ubuntu-24.04`, kernel `6.17.0-1022-azure` x86_64. Actions pinned to commit SHAs
  (`actions/checkout` 11bd7190, `actions/upload-artifact` ea165f8d), read-only token, no secrets.
- Pinned relay: `getpaseo/paseo-relay` at `3fc41c96c8c63f3a7109e832899cc57d473c4531`, checked out
  and verified in the job. Image: `elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79`
  (pulled by digest).
- Results, from the job log:
  - `relay writer differential: 4744 operations: Rust identical to the pinned relay` (the script
    ran the pinned relay twice and diffed both against each other and against the committed
    transcript without the clock lines, then replayed a fresh capture raw);
  - `relay capacity differential: 11514 operations: Rust identical to the pinned relay`.
- SHA-256 of the committed fixtures at the run's commit:

| file | sha256 |
|---|---|
| relay-capacity-ops.txt | 0f9e9a1e02dbc4ad3970620e9d98c09dcedf7750ac0d1f9df5f3b4e0531f884d |
| relay-capacity-baseline.txt | dae6b99db2e4dac5f860b1f9e7a3c132dfb40a904879d0ea61fa3a9823de24d7 |
| relay-writer-ops.txt | a43440e521307258736a65c6803bceb1c95fd3842d4ae45eb7c5ca3462a42d19 |
| relay-writer-baseline.txt | a19955a65db3a4ee3149dc88bb4f90fc1cf1054477b6fc7f9e88588706517751 |

- Fresh capacity capture on the runner: `826659cc06e1df67e1b115dbb4a8e420a14d90d8945a3436a499ebe0ad18f7a0`.
  It differs from the committed transcript, as expected: the memory readings are inputs that vary
  per run, and the replay of the fresh capture passed. The job uploaded the fresh captures as the
  `relay-differential` artifact.
