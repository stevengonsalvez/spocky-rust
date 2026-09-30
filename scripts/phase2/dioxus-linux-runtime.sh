#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
log_file="$raw_dir/dioxus-linux-runtime.log"
image_digest=sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
docker_image="rust@$image_digest"

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' "rust image digest: $image_digest"
  printf '%s\n' 'repository mount: read-only'
  printf '%s\n' 'display: disposable Xvfb'
  printf '%s\n' 'launch assertion: exact PID alive for 10 seconds, then exact PID stop'
  printf '%s\n' 'evidence/raw/phase2/dioxus-linux-runtime.log'
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
# Shell variables in this command expand inside the Linux container.
# shellcheck disable=SC2016
linux_command='set -eu
export PATH=/usr/local/cargo/bin:$PATH
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev librsvg2-dev xvfb xauth >/tmp/apt.log
rustc --version
cargo --version
dpkg-query -W pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev librsvg2-dev xvfb xauth
cargo build --locked -p paseo-ui-renderer-pilot --bin paseo-ui-desktop --no-default-features --features desktop
sha256sum /tmp/paseo-target/debug/paseo-ui-desktop
xvfb-run -a sh -c '\''/tmp/paseo-target/debug/paseo-ui-desktop >/tmp/paseo-ui-linux.log 2>&1 & app_pid=$!; sleep 10; kill -0 "$app_pid"; kill "$app_pid"; wait "$app_pid" || true; test ! -s /tmp/paseo-ui-linux.log'\''
printf "%s\\n" LINUX_DESKTOP_LAUNCH_OK'

set +e
gtimeout 1200 docker run --rm --platform linux/amd64 \
  --mount "type=bind,src=$repository_root,dst=/workspace,readonly" \
  --workdir /workspace \
  --env CARGO_TARGET_DIR=/tmp/paseo-target \
  "$docker_image" bash -c "$linux_command" >"$log_file" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
  printf 'Linux qualification failed with status %s; see %s\n' "$status" "$log_file" >&2
  exit "$status"
fi
if ! grep -F 'LINUX_DESKTOP_LAUNCH_OK' "$log_file" >/dev/null; then
  printf 'Linux qualification omitted launch marker: %s\n' "$log_file" >&2
  exit 1
fi

printf 'Linux desktop qualification passed: %s\n' "$log_file"
shasum -a 256 "$log_file"
