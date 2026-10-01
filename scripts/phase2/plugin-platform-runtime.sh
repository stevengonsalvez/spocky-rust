#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
linux_image_digest=sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
linux_image="rust@$linux_image_digest"
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
linux_esbuild_version=0.27.3
linux_esbuild_sha512=0b38bccb35d458841802d2ffdb2eafa20111f29bdcb0eb24e5ca702f81a4e6726a1f9519895072218f04a2b9b9475de0abe0d0298834f89c90a32b4b41ab3874
expected_tests=25

print_plan() {
  printf '%s\n' 'macOS: current host, bound 600 seconds'
  printf 'Linux: rust@%s, linux/amd64, bound 1200 seconds\n' "$linux_image_digest"
  printf '%s\n' 'Windows: native PowerShell runner, bound 1200 seconds'
  printf 'Paseo baseline: %s\n' "$expected_baseline"
  printf '%s\n' 'Linux source mount: /workspace/source, read-only'
  printf '%s\n' 'Linux baseline mount: /workspace/paseo-rust/.baselines/paseo-runtime, read-only'
  printf 'Linux esbuild: %s, sha512 %s\n' "$linux_esbuild_version" "$linux_esbuild_sha512"
  printf '%s\n' 'boundaries: acquisition, process, update recovery, restart, settings, migration, binary IPC, client contributions'
  printf '%s\n' 'tests: runtime_acquisition=9 selected_server_runtime=7 client_runtime=2 client_contribution_runtime=2 settings_lifecycle=5'
}

resolve_baseline_root() {
  common_git_dir=$(git -C "$repository_root" rev-parse --git-common-dir)
  case "$common_git_dir" in
    /*) ;;
    *) common_git_dir="$repository_root/$common_git_dir" ;;
  esac
  main_root=$(CDPATH='' cd -- "$(dirname "$common_git_dir")" && pwd)
  printf '%s/.baselines/paseo-runtime\n' "$main_root"
}

assert_baseline() {
  baseline_root=$1
  actual=$(git -C "$baseline_root" rev-parse HEAD)
  if [ "$actual" != "$expected_baseline" ]; then
    printf 'Paseo baseline mismatch: expected %s, got %s\n' \
      "$expected_baseline" "$actual" >&2
    exit 1
  fi
  if [ -n "$(git -C "$baseline_root" status --porcelain)" ]; then
    printf 'Paseo baseline is dirty: %s\n' "$baseline_root" >&2
    exit 1
  fi
}

test_arguments='--test runtime_acquisition --test selected_server_runtime --test client_runtime --test client_contribution_runtime --test settings_lifecycle'

assert_test_count() {
  log_file=$1
  platform=$2
  actual=$(grep -Ec '^test .* \.\.\. ok$' "$log_file" || true)
  if [ "$actual" -ne "$expected_tests" ]; then
    printf '%s qualification executed %s passing tests, expected %s: %s\n' \
      "$platform" "$actual" "$expected_tests" "$log_file" >&2
    exit 1
  fi
}

run_macos() {
  if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'macOS qualification requires a Darwin host' >&2
    exit 1
  fi
  log_file="$raw_dir/plugin-platform-macos.log"
  baseline_root=$(resolve_baseline_root)
  assert_baseline "$baseline_root"
  mkdir -p "$raw_dir"
  : >"$log_file"
  {
    uname -a
    rustc --version
    cargo --version
    git --version
    node --version
    npm --version
  } >>"$log_file" 2>&1
  # shellcheck disable=SC2086
  gtimeout 600 cargo test --locked --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-plugin-pilot $test_arguments -- --test-threads=1 \
    >>"$log_file" 2>&1
  assert_test_count "$log_file" macOS
  printf '%s\n' PLUGIN_PLATFORM_MACOS_OK >>"$log_file"
  printf 'macOS plugin qualification passed: %s\n' "$log_file"
  shasum -a 256 "$log_file"
}

run_linux() {
  if ! docker info >/dev/null 2>&1; then
    printf '%s\n' 'Docker is required for Linux qualification' >&2
    exit 1
  fi
  baseline_root=$(resolve_baseline_root)
  assert_baseline "$baseline_root"
  if [ ! -x "$baseline_root/node_modules/.bin/esbuild" ]; then
    printf 'pinned baseline esbuild missing: %s\n' "$baseline_root" >&2
    exit 1
  fi
  log_file="$raw_dir/plugin-platform-linux.log"
  mkdir -p "$raw_dir"
  linux_command='set -eu
export PATH=/usr/local/cargo/bin:$PATH
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git nodejs npm ca-certificates curl >/tmp/plugin-platform-apt.log
curl -fsSL https://registry.npmjs.org/@esbuild/linux-x64/-/linux-x64-0.27.3.tgz -o /tmp/esbuild-linux-x64.tgz
actual_esbuild_sha512=$(sha512sum /tmp/esbuild-linux-x64.tgz | cut -d" " -f1)
expected_esbuild_sha512=0b38bccb35d458841802d2ffdb2eafa20111f29bdcb0eb24e5ca702f81a4e6726a1f9519895072218f04a2b9b9475de0abe0d0298834f89c90a32b4b41ab3874
test "$actual_esbuild_sha512" = "$expected_esbuild_sha512"
mkdir -p /tmp/spocky-pinned-esbuild
tar -xzf /tmp/esbuild-linux-x64.tgz -C /tmp/spocky-pinned-esbuild
export PASEO_ESBUILD_BIN=/tmp/spocky-pinned-esbuild/package/bin/esbuild
test "$("$PASEO_ESBUILD_BIN" --version)" = 0.27.3
rustc --version
cargo --version
git --version
node --version
npm --version
printf "esbuild %s sha512 %s\n" "$PASEO_ESBUILD_BIN" "$actual_esbuild_sha512"
cargo test --locked -p spocky-plugin-pilot --test runtime_acquisition --test selected_server_runtime --test client_runtime --test client_contribution_runtime --test settings_lifecycle -- --test-threads=1'
  set +e
  gtimeout 1200 docker run --rm --platform linux/amd64 \
    --mount "type=bind,src=$repository_root,dst=/workspace/source,readonly" \
    --mount "type=bind,src=$baseline_root,dst=/workspace/paseo-rust/.baselines/paseo-runtime,readonly" \
    --workdir /workspace/source \
    --env CARGO_TARGET_DIR=/tmp/spocky-plugin-platform-target \
    "$linux_image" bash -c "$linux_command" >"$log_file" 2>&1
  status=$?
  set -e
  if [ "$status" -ne 0 ]; then
    printf 'Linux plugin qualification failed with status %s: %s\n' "$status" "$log_file" >&2
    exit "$status"
  fi
  assert_test_count "$log_file" Linux
  printf '%s\n' PLUGIN_PLATFORM_LINUX_OK >>"$log_file"
  printf 'Linux plugin qualification passed: %s\n' "$log_file"
  shasum -a 256 "$log_file"
}

case "${1:-}" in
  --print-plan)
    print_plan
    ;;
  --macos)
    run_macos
    ;;
  --linux)
    run_linux
    ;;
  *)
    printf 'usage: %s --print-plan|--macos|--linux\n' "$0" >&2
    exit 2
    ;;
esac
