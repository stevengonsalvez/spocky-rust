#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
log_file="$raw_dir/plugin-linux-runtime.log"
image_digest=sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
docker_image="rust@$image_digest"

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' "rust image digest: $image_digest"
  printf '%s\n' 'repository mount: read-only'
  printf '%s\n' 'platform: linux/amd64'
  printf '%s\n' 'bound: 1200 seconds'
  printf '%s\n' 'tests: runtime_acquisition, process_protocol, plugin_lifecycle, settings_lifecycle'
  printf '%s\n' 'evidence/raw/phase2/plugin-linux-runtime.log'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Linux qualification\n' >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  printf 'Docker is required for Linux qualification\n' >&2
  exit 1
fi

mkdir -p "$raw_dir"
linux_command='set -eu
export PATH=/usr/local/cargo/bin:$PATH
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git nodejs npm ca-certificates >/tmp/apt.log
rustc --version
cargo --version
git --version
node --version
npm --version
cargo test --locked -p spocky-plugin-pilot \
  --test runtime_acquisition \
  --test process_protocol \
  --test plugin_lifecycle \
  --test settings_lifecycle
printf "%s\\n" PLUGIN_LINUX_RUNTIME_OK'

set +e
gtimeout 1200 docker run --rm --platform linux/amd64 \
  --mount "type=bind,src=$repository_root,dst=/workspace,readonly" \
  --workdir /workspace \
  --env CARGO_TARGET_DIR=/tmp/spocky-target \
  "$docker_image" bash -c "$linux_command" >"$log_file" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
  printf 'Linux plugin qualification failed with status %s; see %s\n' "$status" "$log_file" >&2
  exit "$status"
fi
if ! grep -F 'PLUGIN_LINUX_RUNTIME_OK' "$log_file" >/dev/null; then
  printf 'Linux plugin qualification omitted completion marker: %s\n' "$log_file" >&2
  exit 1
fi

printf 'Linux plugin qualification passed: %s\n' "$log_file"
shasum -a 256 "$log_file"
