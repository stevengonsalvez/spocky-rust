#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
common_git_dir=$(git -C "$repository_root" rev-parse --git-common-dir)
case "$common_git_dir" in
  /*) ;;
  *) common_git_dir="$repository_root/$common_git_dir" ;;
esac
main_root=$(CDPATH='' cd -- "$(dirname "$common_git_dir")" && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite}
expected_reference=5de45e208690b0efc51c59a585ae9729325a9204
image_digest=sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
docker_image="rust@$image_digest"
baseline_dir="$main_root/evidence/raw/phase2/browser-runtime-comparison"
baseline_a="$baseline_dir/original-desktop.png"
baseline_b="$baseline_dir/original-repeat-desktop.png"
baseline_json="$main_root/evidence/raw/phase2/browser-runtime-comparison.json"
output_dir="$repository_root/evidence/phase2/renderer-platform-linux"
# Colima shares only $HOME with the VM, so caches live in named Docker volumes.
target_volume=spocky-renderer-linux-target
cargo_volume=spocky-renderer-linux-cargo-home
build_gate=/private/tmp/spocky-targets/build-gate.sh
container_name="spocky-renderer-linux-$(date +%s)-$$"
# One source for the limits so the printed plan cannot drift from the docker run.
limit_cpus=2
limit_memory_gb=3
limit_seconds=1200

if [ "${1:-}" = "--print-plan" ]; then
  printf 'rust image digest: %s\n' "$image_digest"
  printf 'container limits: %s CPUs, %s GB memory, %s seconds\n' "$limit_cpus" "$limit_memory_gb" "$limit_seconds"
  printf 'memory swap: %sg total, no swap beyond memory\n' "$limit_memory_gb"
  printf '%s\n' 'viewport: 1280x800, scale 1, light theme, en-US, DejaVu Sans'
  printf '%s\n' 'visual: complete SHA-256 membership plus unmasked RGBA RMSE'
  printf '%s\n' 'accessibility: complete AT-SPI tree plus observed focus order'
  printf '%s\n' 'interaction: xdotool Tab then Return on first Plus control'
  printf '%s\n' 'evidence/phase2/renderer-platform-linux/'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf '%s\n' 'gtimeout is required for bounded Linux qualification' >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  printf '%s\n' 'Docker is required for Linux qualification' >&2
  exit 1
fi
actual_reference=$(git -C "$reference_root" rev-parse HEAD)
if [ "$actual_reference" != "$expected_reference" ]; then
  printf 'Paseo reference mismatch: expected %s, got %s\n' "$expected_reference" "$actual_reference" >&2
  exit 1
fi
for baseline in "$baseline_a" "$baseline_b" "$baseline_json"; do
  if [ ! -f "$baseline" ]; then
    printf 'Pinned browser baseline missing: %s\n' "$baseline" >&2
    exit 1
  fi
done

mkdir -p "$output_dir"
linux_command='set -eu
export PATH=/usr/local/cargo/bin:$PATH
export LANG=en_US.UTF-8
export LC_ALL=en_US.UTF-8
export GTK_THEME=Adwaita:light
export GDK_SCALE=1
export GDK_DPI_SCALE=1
export NO_AT_BRIDGE=0
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev librsvg2-dev xvfb xauth x11-utils xdotool imagemagick at-spi2-core python3-pyatspi python3-pil dbus-x11 locales fonts-dejavu-core >/tmp/apt.log
sed -i "s/^# *en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/" /etc/locale.gen
locale-gen en_US.UTF-8 >/tmp/locale.log
rustc --version
cargo --version
dpkg-query -W xvfb x11-utils xdotool imagemagick at-spi2-core python3-pyatspi python3-pil fonts-dejavu-core
cargo build --locked -p spocky-ui-renderer-pilot --bin spocky-ui-desktop --no-default-features --features desktop
sha256sum /target/debug/spocky-ui-desktop
dbus-run-session -- sh -eu -c '\''
  export DISPLAY=:99
  export GTK_MODULES=gail:atk-bridge
  Xvfb :99 -screen 0 1280x800x24 -dpi 96 -nolisten tcp >/tmp/xvfb.log 2>&1 &
  xvfb_pid=$!
  trap "kill $xvfb_pid 2>/dev/null || true; wait $xvfb_pid 2>/dev/null || true" EXIT HUP INT TERM
  n=0
  until xdpyinfo -display :99 >/dev/null 2>&1; do n=$((n + 1)); [ "$n" -lt 100 ] || exit 1; sleep 0.1; done
  /target/debug/spocky-ui-desktop >/output/application.log 2>&1 &
  app_pid=$!
  trap "kill $app_pid 2>/dev/null || true; wait $app_pid 2>/dev/null || true; kill $xvfb_pid 2>/dev/null || true; wait $xvfb_pid 2>/dev/null || true" EXIT HUP INT TERM
  n=0
  window_id=
  until [ "$n" -ge 100 ]; do
    window_id=$(xdotool search --onlyvisible --pid "$app_pid" 2>/dev/null | head -n 1 || true)
    [ -n "$window_id" ] && break
    sleep 0.1
    n=$((n + 1))
  done
  [ -n "$window_id" ]
  xdotool windowmove --sync "$window_id" 0 0
  xdotool windowsize --sync "$window_id" 1280 800
  sleep 10
  import -display :99 -window root /output/candidate.png
  /usr/bin/python3 /workspace/scripts/phase2/renderer-platform-linux-compare.py /output/candidate.png /baseline/baseline-a.png /baseline/baseline-b.png /output/visual.json
  /usr/bin/python3 /workspace/scripts/phase2/renderer-platform-linux-atspi.py /output/atspi.json "$window_id" /baseline/browser-runtime-comparison.json
  kill "$app_pid"
  wait "$app_pid" || true
'\''
printf "%s\\n" RENDERER_PLATFORM_LINUX_OK'

gate=
if [ -x "$build_gate" ]; then gate=$build_gate; fi
# Remove only this exact container name when the runner is interrupted or exits.
cleanup() {
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM
set +e
$gate gtimeout --kill-after=30 "$limit_seconds" docker run --name "$container_name" --platform linux/amd64 \
  --cpus "$limit_cpus" --memory "${limit_memory_gb}g" --memory-swap "${limit_memory_gb}g" \
  --mount "type=bind,src=$repository_root,dst=/workspace,readonly" \
  --mount "type=bind,src=$baseline_a,dst=/baseline/baseline-a.png,readonly" \
  --mount "type=bind,src=$baseline_b,dst=/baseline/baseline-b.png,readonly" \
  --mount "type=bind,src=$baseline_json,dst=/baseline/browser-runtime-comparison.json,readonly" \
  --mount "type=bind,src=$output_dir,dst=/output" \
  --workdir /workspace \
  --mount "type=volume,src=$target_volume,dst=/target" \
  --mount "type=volume,src=$cargo_volume,dst=/cargo-home" \
  --env CARGO_TARGET_DIR=/target \
  --env CARGO_HOME=/cargo-home \
  --env CARGO_BUILD_JOBS=2 \
  "$docker_image" bash -c "$linux_command" >"$output_dir/container.log" 2>&1
status=$?
inspect=$(docker inspect "$container_name" --format '{{json .State}}' 2>/dev/null || true)
if [ -n "$inspect" ]; then
  printf '%s\n' "$inspect" >"$output_dir/container-state.json"
fi
docker rm -f "$container_name" >/dev/null 2>&1 || true
set -e
if [ "$status" -ne 0 ]; then
  if printf '%s' "$inspect" | grep -F '"OOMKilled":true' >/dev/null; then
    printf 'Linux renderer qualification OOM-killed; see %s\n' "$output_dir/container.log" >&2
  else
    printf 'Linux renderer qualification failed with status %s; see %s\n' "$status" "$output_dir/container.log" >&2
  fi
  exit "$status"
fi
if ! grep -F 'RENDERER_PLATFORM_LINUX_OK' "$output_dir/container.log" >/dev/null; then
  printf 'Linux renderer qualification omitted success marker: %s\n' "$output_dir/container.log" >&2
  exit 1
fi

printf 'Linux renderer evidence captured: %s\n' "$output_dir"
find "$output_dir" -maxdepth 1 -type f -exec shasum -a 256 {} +

