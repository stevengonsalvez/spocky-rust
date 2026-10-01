# Hub mixed ownership qualification

Run:

```sh
gtimeout --kill-after=30 1200 scripts/phase2/hub-mixed-ownership.test.sh
```

Observed sequence: pinned baseline owns the disposable 49-row same-schema directory; candidate is excluded; baseline exits cleanly; candidate opens the unchanged directory; new pinned baseline is excluded.

Scope: ordered live starts only.

Limitations:

- Simultaneous pre-owner-record race is unqualified.
- Schema downgrade is unqualified.

Port 6767 was untouched.

| Artifact | SHA-256 |
| --- | --- |
| `hub-mixed-ownership-events.json` | `bd8e0c197ded19df531c9af462f4170b047c3aa367878e33b3efab0afd255a66` |
| `hub-mixed-ownership-processes.json` | `7352e8ce820de7ed233a0821c0c23454abce6305d88af01f3f0f8ee44ccde846` |
| `hub-mixed-ownership-report.json` | `1adeec0d360c058987a523d4ca85021d1fd12eac76ac1ff2ae1a6c6b284631ee` |
