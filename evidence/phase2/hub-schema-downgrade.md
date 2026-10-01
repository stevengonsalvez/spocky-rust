# Hub schema downgrade evidence

Pinned Hub commit: `28f6c78833065fd282f9064f92a9aa61875dd359`.

## Observed contract

- Pinned journal version is `7`, with 49 entries from `0000_phase_0_spine` through `0048_execution_authority`.
- Pinned runtime and retained candidate order migrations by journal `when` and compare only the maximum recorded `created_at`.
- Candidate with one additive future migration created a 50-row journal. Pinned runtime opened it, applied zero older migrations, and preserved both writers' probe rows.
- Pinned runtime with the same additive future migration created a 50-row journal. Candidate opened it, applied zero older migrations, and preserved both writers' probe rows.
- Candidate transaction containing one valid future migration followed by invalid SQL failed. Journal remained at 49 rows and the first migration's table was absent.

Both implementations accept an unknown future maximum timestamp. Neither validates a journal prefix or migration hash before skipping known older migrations. Results qualify only the synthetic additive schema used by this harness. Destructive or semantic future migrations remain incompatible and no general downgrade parity is claimed.

## Bounds and cleanup

The harness archives the clean pinned tree into a disposable directory, installs dependencies there, uses three disposable PGlite directories, and removes the exact fixture on exit. Every external phase has a finite `gtimeout` and 30-second forced-kill bound. No listener is started and port 6767 is untouched.

## Hashes

- Pinned journal: `cb4130fb028cb2f1077d72252b9fdb456026ed613b197e8f3968304fc9dd7f41`
- Pinned embedded runtime: `5883459a421c34338e5c8f35365b9cedc8fb2001707384c13bae761e67b62210`
- Candidate retained adapter: `b1e490eba9bffcdf049355d54758b76785b056cd789bccb5cecda6ee0b9ff0a6`
- Raw report: `5de06f6bf72036a1d5970e7a59566265f3aefa770ad3e452c569cdc186d8e317`

Run: `gtimeout --kill-after=30 1250 scripts/phase2/hub-schema-downgrade.test.sh`.
