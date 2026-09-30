# Hub embedded storage closure checkpoint

Pinned Hub baseline `28f6c78833065fd282f9064f92a9aa61875dd359` uses PGlite
`0.5.4`. The differential queries both installed databases. It does not derive
expected tables or constraints from the Drizzle snapshot.

Run:

```sh
sh scripts/phase2/hub-embedded-differential.test.sh
scripts/phase2/hub-embedded-differential.sh
cargo test --locked -p spocky-hub-pilot --test embedded_sql_runtime
```

The installed table inventories match at 50 tables. Installed constraint and
index inventories do not match: PGlite reports 537 names and SQLite reports
209. SQLite has 17 names absent from PGlite; PGlite has 345 names absent from
SQLite. The raw comparison records both lists without snapshot filtering.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `pglite.json` | 51,030 | `92a873c2b3fedec61aa0fcf499d4cbe5d1964b90d47eb81e3a82a770ae97f508` |
| `rust.json` | 24,384 | `a8a451668adad104bb67118cec5110e420fe5ecc8ce761a398537a58b4e943fb` |
| `comparison.json` | 112,741 | `8ffb8e2135b14f0db481e2850e6894ad730d116a42fb2eb0c2fc31748f7d93aa` |

Thirteen targeted runtime tests pass. They include legacy whole-state reopen,
ordered migration-prefix resume, rejection without journal rewriting, and
transaction rollback after an injected migration interruption.

This checkpoint does not select SQLite as the exact engine. Observable pilot
operations match, but exact database parity remains false. Engine, schema,
dialect, migrations, relational state mutation, and PGlite crash-process
qualification remain blockers.
