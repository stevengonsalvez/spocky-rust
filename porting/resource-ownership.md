# Resource Ownership

| Resource | Owner | Isolation | Teardown |
|---|---|---|---|
| Reference Paseo checkout | baseline reader | Read-only at exact commit | None |
| Hub baseline | baseline reader | `.baselines/hub`, detached exact commit | None |
| Relay baseline | baseline reader | `.baselines/relay`, detached exact commit | None |
| Importer baseline | baseline reader | `.baselines/import`, detached exact commit | None |
| Production daemon port `6767` | out of scope | Never touched | None |
| Test daemon ports | scenario harness | Dynamically allocated, never `6767` | Exact PID or named tmux session |
| Test state homes | scenario harness | Unique temporary directory per scenario | Remove exact directory after digest capture |
| Test databases | owning Hub scenario | Unique database file or isolated instance | Exact database or container identity |
| Provider credentials | owning real-provider scenario | Existing test credentials only | Revoke only disposable credentials created by scenario |
| Crypto keys | owning scenario | Generated inside scenario state home | Remove exact state home after evidence capture |
| Worktrees and branches | assigned writer | One exclusive task scope | Release after settled integration |
| Devices and simulators | qualification lead | Serialized access | Stop exact simulator or device session |
| CI jobs | qualification lead | Workflow run and matrix leg identity | Cancel exact run only |

Every long wait records success, failure, cancellation, timeout, and skip terminal states, plus deadline and maximum polls.

