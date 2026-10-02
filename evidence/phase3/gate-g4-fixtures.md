# G4 fixtures

Run each with `scripts/phase3/gate.sh <id> [--self-check-only]`. Every fixture runs the pinned Paseo CLI against a real pinned codex 0.159.0 pointed at the loopback Responses stub, on a disposable home. The self-check (original daemon on both sides) must pass before any parity run.

| Fixture | What it does | Pinned behaviour it compares |
|---|---|---|
| `g4-http500` | The stub answers the first turn with HTTP 500 and a JSON error body. Steps: `workspace create`, `run`, `ls -a`. | The pinned CLI reports the failure in the result, not the exit code: `run` exits 0 with status `error`, and `ls -a` shows status `error`. The stub sees exactly one request. |
| `g4-nocodex` | No `codex` on the daemon's `PATH` (the shim is not written). Same steps. | Spawn and probe failure texts, provider availability, the create refusal and the CLI exit code. |
| `g4-disconnect` | The CLI is killed (signal 9) once the stub holds the second, delayed turn in flight; then `wait`, `logs`, a second `send`, `logs`. | The daemon finishes the abandoned turn and a new client sees and extends it. |
| `g4-socketdrop` | A node probe (`scripts/phase3/g4-subscriber.mjs`) over the pinned `DaemonClient`: client A runs in a child process that the probe kills with SIGKILL while the stub holds the turn open, so its TCP connection drops with no close handshake; client B fetches the agent and cancels it. | A ends with `SIGKILL`; B fetches the agent `running`, sees no error frame, and gets `cancel_agent_response`; B's raw wire text is compared. |
| `g4-oldstate` | The original daemon makes the home (one turn), the daemon under test opens it after a restart: `ls -a`, `inspect`, `send`, `logs`. | Stored agents, registries and receipts load across daemons. |
| `g4-newstate` | The reverse: the daemon under test makes the home, the original opens it. | Same, in the other direction. |

Retries (`g4-http500`): codex retries are off in the codex config every fixture home gets, `request_max_retries = 0` and `stream_max_retries = 0` in `CODEX_HOME/config.toml`, identical on both sides. The stub therefore sees exactly one request and the 500 does not take minutes; no 500-then-404 script is used.
