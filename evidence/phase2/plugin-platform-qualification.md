# Plugin platform qualification

Status: macOS and Linux runtime boundaries pass. The Windows runner and portable
process changes are ready, but no native Windows runtime was available in this
workspace. A locked MSVC all-target cross-check passes. Windows runtime
qualification remains open.

## Worker and provenance

- Worker assignment: `gpt-5.6-sol`, medium effort, no delegation.
- Approval policy: `never`.
- Sandbox: `disabled/unrestricted` with full filesystem access.
- Branch base: `4e0ccaf` from current `main`.
- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`, clean; mounted read-only for Linux.
- Linux image: `rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55`, `linux/amd64`.
- Linux esbuild: pinned baseline version `0.27.3`; Linux package SHA-512
  `0b38bccb35d458841802d2ffdb2eafa20111f29bdcb0eb24e5ca702f81a4e6726a1f9519895072218f04a2b9b9475de0abe0d0298834f89c90a32b4b41ab3874`.

## Boundaries and counts

The same five targeted suites cover acquisition, process execution, reviewed
update recovery, restart, settings persistence, settings migration, binary IPC,
and headless client contributions.

| Suite | macOS | Linux |
|---|---:|---:|
| `client_contribution_runtime` | 2/2 | 2/2 |
| `client_runtime` | 2/2 | 2/2 |
| `runtime_acquisition` | 9/9 | 9/9 |
| `selected_server_runtime` | 7/7 | 7/7 |
| `settings_lifecycle` | 5/5 | 5/5 |
| Total | 25/25 | 25/25 |

macOS used Darwin 24.6.0 x86_64, Rust and Cargo 1.94.0, Git 2.52.0,
Node 26.7.0, and npm 11.19.0. Linux used Rust and Cargo 1.94.0, Git
2.39.5, Node 18.20.4, and npm 9.2.0.

## Raw evidence

| Artifact | Result | Bytes | SHA-256 |
|---|---|---:|---|
| `plugin-platform-macos.log` | 25 passed, 0 failed | 3,506 | `97ddc6e7a117a0ad48d01b74e8350f7d35d68fe8e61995dd52181c87626ebee7` |
| `plugin-platform-linux-attempt-1.log` | 0 tests, read-only nested mount rejected | 632 | `501421d627b714a2549caebd2d56e0879b4d6f8905c516a22cda607485302802` |
| `plugin-platform-linux-attempt-2.log` | 2 passed, 2 failed, Darwin esbuild rejected on Linux | 8,042 | `5aabfef78cf0d6040b25a5d0812473ac8a25ba6b8867aaeb2e1dbfcc2c36b83f` |
| `plugin-platform-linux.log` | 25 passed, 0 failed | 5,111 | `5497b3144311e61b58bd2153283e7130d3bea0383ee07159976a445f770cccfa` |
| `plugin-platform-windows-cross-check.log` | locked MSVC all-target compile passed | 192 | `96c2533a0067b61d37de3a60daa24990f9d925a316edcab03929ea555a97cecc` |

Attempt 1 established that a nested bind target cannot be created beneath the
read-only source mount. The repaired layout mounts source at `/workspace/source`
and the pinned baseline at
`/workspace/paseo-rust/.baselines/paseo-runtime`, both read-only.

Attempt 2 established that the pinned baseline's installed Darwin esbuild
cannot execute on Linux. The passing run downloads the exact Linux package from
the pinned lockfile outside the baseline, verifies its SHA-512 and version, and
passes its executable through `PASEO_ESBUILD_BIN`. The baseline stays unchanged.

## Portable process changes

- Windows resolves Node as `node.exe` and npm as `npm.cmd`.
- Windows bounded-command timeout uses `taskkill.exe /T /F` for the exact child
  tree. Unix process-group termination is unchanged.
- Windows selected-server tests resolve `esbuild.cmd` or the explicit
  `PASEO_ESBUILD_BIN` override.
- `plugin-platform-windows.ps1` bounds the exact Cargo process tree at 1,200
  seconds, records tool versions, requires the clean pinned baseline, enforces
  all 25 passing tests, and writes a raw log digest.

## Windows blocker

No native Windows host, Wine runtime, PowerShell, remote, or workflow runner was
available. A locked MSVC cross-check can prove compilation but cannot qualify
runtime acquisition, process cleanup, migration, restart, or binary IPC. The
cross-check passed for `x86_64-pc-windows-msvc` with `cargo-xwin 0.23.1`, Rust
and Cargo 1.94.0, and `spocky-plugin-pilot --all-targets --locked`. Run the
PowerShell runner on native Windows before claiming Windows qualification.

The first raw-capture shell wrapper returned 1 after the successful cached
cross-check because zsh reserves `status`. The corrected wrapper used
`exit_code`, returned 0, and produced the same raw-log digest. The Cargo check
itself did not fail.

## Final checks

- `cargo clippy --locked -p spocky-plugin-pilot --all-targets -- -D warnings`:
  passed after moving existing error trait implementations before the test
  module.
- `cargo fmt --package spocky-plugin-pilot -- --check`: passed.
- `sh scripts/phase2/plugin-platform-runtime.test.sh`: passed.
- `git diff --check`: passed.

## Reproduction

```sh
sh scripts/phase2/plugin-platform-runtime.test.sh
scripts/phase2/plugin-platform-runtime.sh --macos
scripts/phase2/plugin-platform-runtime.sh --linux
gtimeout 1200 env \
  XWIN_CACHE_DIR=/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust/.tools/xwin-cache \
  /Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust/.tools/bin/cargo-xwin \
  xwin check --locked --target x86_64-pc-windows-msvc \
  -p spocky-plugin-pilot --all-targets \
  --target-dir /tmp/spocky-plugin-windows-target
```

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\phase2\plugin-platform-windows.ps1
```

No production daemon, port `6767`, deployment, publication, paid service, or
remote production state was used.
