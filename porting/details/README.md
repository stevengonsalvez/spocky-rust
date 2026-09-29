# Detailed Baseline Indexes

These indexes make the family-level capability matrix auditable against exact source surfaces.

| Index | Rows | Meaning |
|---|---:|---|
| `paseo-tree.txt` | 5,069 | Git mode, blob identity, and path at the pinned Paseo commit |
| `hub-tree.txt` | 823 | Git mode, blob identity, and path at the pinned Hub commit |
| `relay-tree.txt` | 60 | Git mode, blob identity, and path at the pinned relay commit |
| `import-tree.txt` | 32 | Git mode, blob identity, and path at the pinned importer commit |
| `paseo-tests.txt` | 1,659 | Paseo test and specification paths |
| `hub-tests.txt` | 213 | Hub test and specification paths |
| `relay-tests.txt` | 14 | Relay test and support paths |
| `import-tests.txt` | 5 | Importer test paths |
| `protocol-literals.txt` | 628 | Protocol literal declarations with source locations |
| `server-features.txt` | 78 | Server feature boolean declarations in the pinned schema block |
| `cli-commands.txt` | 90 | CLI command registration sites |
| `app-routes.txt` | 25 | App route source files |
| `hub-routes.txt` | 48 | Hub route source files |
| `compat-sites.txt` | 440 | Package compatibility tags with source locations |
| `delivery-inputs.txt` | 64 | Workflow, Fastlane, Nix, and Docker inputs |

Tree indexes come from `git ls-tree -r --full-tree HEAD` in each pinned checkout. Path indexes come from `git ls-files`. Literal, feature, command, and compatibility indexes come from line-numbered `rg` extraction against the pinned Paseo checkout. All output is byte-sorted with `LC_ALL=C`.

Counts are inventory coverage, not executed-test counts or parity proof. Differential manifests record executed tests and fixtures separately.
