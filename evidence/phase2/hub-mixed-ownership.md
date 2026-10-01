# Hub mixed ownership qualification

Run:

```sh
gtimeout --kill-after=30 1200 scripts/phase2/hub-mixed-ownership.test.sh
```

Observed sequence: pinned baseline owns the disposable 49-row same-schema directory; candidate is excluded; baseline exits cleanly; candidate opens the unchanged directory; new pinned baseline is excluded.

Observed storage: baseline 49 journal rows; candidate 49 journal rows; candidate applied zero migrations.

Shutdown: both owners exited cleanly and their dedicated process groups were gone.

Platform: macOS x64.

Scope: ordered live starts only.

Limitations:

- Simultaneous pre-owner-record race is unqualified.
- Schema downgrade is unqualified.

Port 6767 was untouched.

| Artifact | SHA-256 |
| --- | --- |
| `hub-mixed-ownership-events.json` | `7e97f7222966074791fccffca71d753ea8e4e3e33957c3e7758f661b6524f7ea` |
| `hub-mixed-ownership-processes.json` | `9ac9b4a86cd61fcefa6cdfa6556dea986bce6e8278ea543230a5ed992d496458` |
| `hub-mixed-ownership-report.json` | `ec640ef7c641812ad108b9966ee9f45e6188fd148bcc5d40799f1f7398415ae3` |
