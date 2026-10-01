# Hub legacy storage directory handoff

Pinned baseline: `28f6c78833065fd282f9064f92a9aa61875dd359`

Run:

```sh
gtimeout --kill-after=30 1200 scripts/phase2/hub-legacy-handoff.test.sh
```

The runner archives the pinned baseline into a disposable directory. It installs
dependencies and creates the database only inside that copy. The baseline source
tree and `.baselines` remain unchanged.

The pinned baseline created and migrated the directory to 49 journal rows, then
wrote an exact marker payload. The retained candidate opened the same directory,
read that exact payload, ran a no-op migration with zero applied entries, kept 49
journal rows, and wrote its exact marker payload. Data preservation passed.

The pinned baseline then reopened the candidate-mutated directory within the
300-second bound. It read both marker rows and kept 49 journal rows before and
after its migration call. Reverse rollback is supported for this observed
same-schema directory. The harness enforces exact payload and journal values
before publishing evidence. Every long command has a forced-kill fallback. No
schema downgrade was exercised.

This evidence does not qualify concurrent legacy and candidate ownership. It
does not change compatibility acceptance. The retained PGlite exception remains
`required-not-accepted`.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `hub-legacy-handoff-baseline-produce.json` | 131 | `8a77d935886601b6e995ffe632f0a89a0c2927775f8422cadd3e2fe59b955c67` |
| `hub-legacy-handoff-candidate-forward.json` | 641 | `2669e90fe5ab4fe6f48bea8d6085fcac473d100cf6f484e364b1d5e0d2ecb5ca` |
| `hub-legacy-handoff-baseline-reverse.json` | 258 | `2e0a079ce63eacf643ccd65b8b11d40764a75268bfffd7770462c0e626b1e95f` |
| `hub-legacy-handoff-report.json` | 1,566 | `39af6309591221cf2ef6c7924b6e522d7d806232fe99aad372e40255af077c35` |
