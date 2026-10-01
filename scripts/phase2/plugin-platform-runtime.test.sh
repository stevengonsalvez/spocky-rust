#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)

plan=$("$repository_root/scripts/phase2/plugin-platform-runtime.sh" --print-plan)
printf '%s\n' "$plan" | grep -F 'macOS: current host, bound 600 seconds'
printf '%s\n' "$plan" | grep -F 'Linux: rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55, linux/amd64, bound 1200 seconds'
printf '%s\n' "$plan" | grep -F 'Windows: native PowerShell runner, bound 1200 seconds'
printf '%s\n' "$plan" | grep -F 'Paseo baseline: 5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$plan" | grep -F 'Linux source mount: /workspace/source, read-only'
printf '%s\n' "$plan" | grep -F 'Linux baseline mount: /workspace/paseo-rust/.baselines/paseo-runtime, read-only'
printf '%s\n' "$plan" | grep -F 'Linux esbuild: 0.27.3, sha512 0b38bccb35d458841802d2ffdb2eafa20111f29bdcb0eb24e5ca702f81a4e6726a1f9519895072218f04a2b9b9475de0abe0d0298834f89c90a32b4b41ab3874'
printf '%s\n' "$plan" | grep -F 'boundaries: acquisition, process, update recovery, restart, settings, migration, binary IPC, client contributions'
printf '%s\n' "$plan" | grep -F 'tests: runtime_acquisition=9 selected_server_runtime=7 client_runtime=2 client_contribution_runtime=2 settings_lifecycle=5'

windows_plan=$(sed -n '/if ($PrintPlan)/,/^}/p' \
  "$repository_root/scripts/phase2/plugin-platform-windows.ps1")
printf '%s\n' "$windows_plan" | grep -F 'Windows: native x86_64-pc-windows-msvc runtime'
printf '%s\n' "$windows_plan" | grep -F 'bound: 1200 seconds'
printf '%s\n' "$windows_plan" | grep -F 'npm executable: npm.cmd'
printf '%s\n' "$windows_plan" | grep -F 'raw log: evidence/raw/phase2/plugin-platform-windows.log'

grep -F 'taskkill.exe' "$repository_root/crates/spocky-plugin-pilot/src/lib.rs" >/dev/null
grep -F 'npm.cmd' "$repository_root/crates/spocky-plugin-pilot/src/lib.rs" >/dev/null
grep -F '"esbuild.cmd"' \
  "$repository_root/crates/spocky-plugin-pilot/tests/selected_server_runtime.rs" >/dev/null
test "$(rg -o 'var_os\("PASEO_ESBUILD_BIN"\)' \
  "$repository_root/crates/spocky-plugin-pilot/tests/client_runtime.rs" \
  "$repository_root/crates/spocky-plugin-pilot/tests/selected_server_runtime.rs" | wc -l | tr -d ' ')" = '2'
