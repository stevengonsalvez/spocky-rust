# Plugin settings schema parity

Status: requested pinned Paseo schema slice implemented and differentially proven.

## Provenance

- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`
- Rust base: `68287d903b25572e99d678781b64bba33d352bcc`
- Baseline source: `packages/server/src/server/plugins/settings/index.ts`
- Baseline tests: `packages/server/src/server/plugins/settings/index.test.ts`
- Baseline documentation: `docs/plugins.md`

The immutable reference checkout had no `node_modules`. The capture used an
untracked disposable copy under `crates/spocky-plugin-pilot/`, installed exactly
from the pinned `package-lock.json` with lifecycle scripts disabled. The source
checkout remained tracked-clean at the pinned commit.

## Baseline schema inventory

The source accepts any `ZodType`, calls `parseAsync`, and then calls
`z.json().parse` on the parsed output. Exact supported boundary is therefore any
Zod input whose asynchronous parsed output is JSON serializable. Functions,
symbols, bigints, dates, maps, sets, `undefined`, `NaN`, and infinities are not
valid stored outputs.

| Shape | Baseline source or test evidence | Rust form |
|---|---|---|
| arbitrary JSON | parsed output passes `z.json()` | `SettingsSchema::json()` |
| boolean | pinned `enabled` field | `boolean()` |
| integer | pinned `count` field with `int` and `min` | `integer()` |
| number | accepted Zod JSON number | `number()` |
| string | accepted Zod JSON string | `string()` |
| enum | Zod string enum with exact options | `enumeration()` |
| object | root and migrated object tests | `object()` |
| array | accepted Zod JSON array | `array()` |
| defaults | pinned boolean and number defaults | `default()` |
| async refinement | source uses `parseAsync` | `refine_async()` |
| multiple errors | source joins issue messages with newline | aggregated in schema order |

Objects strip unknown keys because the baseline uses ordinary `z.object`.
Missing defaulted fields receive defaults. Missing required fields, invalid
types, bounds, enum choices, and refinement failures preserve pinned Zod 4.4.3
message text for this slice.

## Differential cases

`scripts/phase2/plugin-settings-capture.sh` executes the original TypeScript
store through pinned Vitest, executes the Rust case through the permitted
`settings_lifecycle` target, and compares the two emitted JSON strings without
normalization.

Eight byte-identical cases pass:

1. Array minimum failure.
2. Enum option failure.
3. Missing required nested string.
4. String minimum failure.
5. Number minimum failure.
6. Asynchronous refinement failure.
7. Multiple issue ordering and newline joining.
8. Unknown-key stripping, defaults, fractional number persistence, exact
   revision, and asynchronous subscriber rejection reporting.

## Preserved baseline defects

- Subscriber callbacks are isolated from writes. A thrown or rejected callback
  is reported, but the write remains saved.
- Subscriber failures are side-channel reports only. Callers cannot observe
  them in the saved response.
- Validation errors discard issue paths and codes, retaining only messages
  joined with newline.

## Verification

- `cargo test -p spocky-plugin-pilot --test settings_lifecycle`: 9 passed.
- `cargo test -p spocky-plugin-pilot --test plugin_lifecycle`: 3 passed.
- `cargo test -p spocky-plugin-pilot --test protocol_manifest`: 2 passed.
- `cargo test -p spocky-plugin-pilot --test process_protocol`: 2 passed.
- `cargo test -p spocky-plugin-pilot --test runtime_acquisition`: 9 passed.
- `cargo fmt --package spocky-plugin-pilot -- --check`: passed.
- `cargo clippy -p spocky-plugin-pilot --all-targets -- -D warnings`: passed.
- `scripts/phase2/plugin-settings-capture.sh`: 8 matched, 0 mismatched.

Raw differential log SHA-256:
`a61bb77f9b252e4696de7f4add9d49532b9808ffcc301a007786fc4d66acb06f`.

Targeted verification log SHA-256:
`d341df863e2e27fed28a711f3a0a1e856dffc98ff34f446ed1ce6c93359b2a02`.

## Remaining gaps

- Daemon RPC integration remains outside this task and unchanged.
- Native Windows runtime qualification remains outside this task.
- Existing `scripts/phase2/plugin-runtime-capture.sh` could not run because this
  worktree lacks `.baselines/paseo-runtime` and `.baselines/import`. Exact error:
  `fatal: cannot change to '.baselines/paseo-runtime': No such file or directory`.
  The settings-specific pinned Vitest differential completed independently.
