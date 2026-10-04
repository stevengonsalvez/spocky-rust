# Default locale on Linux and Windows

Node 22.20.0 resolves its default locale (`Intl.DateTimeFormat().resolvedOptions().locale`,
and so no-argument `toLocaleLowerCase()`) through ICU. `spocky_contracts::locale::default_locale`
reads `LC_ALL`, then `LC_MESSAGES`, then `LANG`, and canonicalizes the `@` modifier as ICU does.

## Linux

Run: `node:22.20.0-slim` (glibc, x86_64) under colima, with `env` passed through a probe script
(one node spawn per environment, `spawnSync` with an explicit `env`). The same probe ran on macOS.
Output lines sorted, then compared file against file: no difference.

| probe | environments | sha256 of the sorted output (identical on macOS and Linux) |
|---|---|---|
| mixed A | 1024: `LC_ALL`, `LC_MESSAGES`, `LC_CTYPE`, `LANG`, `LANGUAGE` over unset, `tr_TR.UTF-8`, `C`, `de_DE` | 8d4348776de2f43edb8b9bb376f5fef13fa8682b5ef884bfd30cfb59e92de7ed |
| mixed B | 512: `LC_ALL`, `LC_MESSAGES`, `LANG` over unset, `C.UTF-8`, `en_US.UTF-8`, `tr_TR.ISO-8859-9`, `POSIX`, empty, `sr_Latn_RS`, `az_AZ@latin` | 7120aaa780b3b0cb412846dc13d030a9458310b7c7e8c1ca2a97f4c26094731a |
| modifiers | 70 `LANG` values with `@` modifiers (variants, keywords, `posix`, separators) | e4e2dc96d61903b2334c68ecb2ca68bb00affd9ef75222e7a8305a02e1d77606 |

The tests that keep this true: `default_locale_matches_node` and
`default_locale_mixed_environments_match_node` in `crates/spocky-contracts/tests/js_locale_differential.rs`
(the second is `cfg(unix)`).

## Windows

GitHub Actions run 37189094924 (work branch `ci/p3_contracts/locale`, `windows-latest`, node 22.20.0):
node prints `en-US` for `LC_ALL` set to empty, `tr_TR.UTF-8`, `az_AZ@latin`, `de_DE`, `C`, for `LANG=tr_TR.UTF-8`,
and with nothing set. The environment is not read; ICU takes the Windows user locale
(`GetUserDefaultLocaleName`), which was `en-US` on the runner. `default_locale` has no Windows branch yet:
it needs that call, which `unsafe_code = "forbid"` rules out in this crate.
