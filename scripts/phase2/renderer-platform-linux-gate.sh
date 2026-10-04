#!/bin/sh
# Linux gate: the shipped Paseo desktop app, CEF host A and Electron host B on Linux,
# inside one pinned image run with --network none.
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
scripts="$repository_root/scripts/phase2"
reference_root=${PASEO_REFERENCE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite}
expected_reference=5de45e208690b0efc51c59a585ae9729325a9204
cef_archive=cef_binary_152.0.7+g83ffcba+chromium-152.0.7977.83_linux64_minimal.tar.bz2
cef_sha256=a75d8956901e1f91bad4f7151af1dc31aaa2d46888cd6af51adb0ecaa77f860f
bundle=${SPOCKY_DIOXUS_BUNDLE:-/private/tmp/spocky-targets/renderer-linux/dx/spocky-ui-web/release/web/public}
image_tag=spocky-renderer-linux-gate:pinned
image_pin_file="$scripts/renderer-platform-linux-gate.image-id"
dockerfile="$scripts/renderer-platform-linux-gate.Dockerfile"
output_dir="$repository_root/evidence/phase2/renderer-platform-linux-gate"
build_gate=/private/tmp/spocky-targets/build-gate.sh
container_name="spocky-renderer-linux-gate-$(date +%s)-$$"
# One source for the limits so the printed plan cannot drift from the docker run.
limit_cpus=2
limit_memory_gb=3
limit_seconds=1800

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'image: node 26.7.0 bookworm by digest, Debian snapshot 2026-10-01T17:00:00Z, every apt package pinned'
  printf '%s\n' 'shipped app: packages/desktop built as shipped, unpacked and unsigned, run on Xvfb 1280x800'
  printf 'host A: CEF 152.0.7+g83ffcba, Chromium 152.0.7977.83, linux64 minimal, SHA-256 %s\n' "$cef_sha256"
  printf '%s\n' 'host B: Electron 44.2.0 from the committed lockfile'
  printf 'container limits: %s CPUs, %s GB memory, %s seconds\n' "$limit_cpus" "$limit_memory_gb" "$limit_seconds"
  printf 'memory swap: %sg total, no swap beyond memory\n' "$limit_memory_gb"
  printf '%s\n' 'network: none at run time'
  printf '%s\n' 'measured: exact full-PNG SHA-256 membership, unmasked RMSE, focus walk, activation, accessibility tree'
  printf '%s\n' 'evidence/phase2/renderer-platform-linux-gate/'
  exit 0
fi
build_image=0
if [ "${1:-}" = "--build-image" ]; then
  build_image=1
  shift
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan | --build-image]\n' "$0" >&2
  exit 2
fi
for command in gtimeout docker git shasum python3; do
  command -v "$command" >/dev/null 2>&1 || { printf '%s is required\n' "$command" >&2; exit 1; }
done
docker info >/dev/null 2>&1 || { printf '%s\n' 'Docker is required' >&2; exit 1; }
actual_reference=$(git -C "$reference_root" rev-parse HEAD)
if [ "$actual_reference" != "$expected_reference" ]; then
  printf 'Paseo reference mismatch: expected %s, got %s\n' "$expected_reference" "$actual_reference" >&2
  exit 1
fi
gate=
if [ -x "$build_gate" ]; then gate=$build_gate; fi
mkdir -p "$output_dir"

bundle_sha=$(cd "$bundle" && find . -type f | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256 | awk '{print $1}')
if [ "$build_image" -eq 1 ]; then
  context=/private/tmp/spocky-targets/linux-gate-context
  rm -rf "$context"
  mkdir -p "$context/reference" "$context/hostb" "$context/cefhost"
  git -C "$reference_root" archive "$actual_reference" | tar -x -C "$context/reference"
  cp "$scripts/renderer-platform-electron/package.json" "$scripts/renderer-platform-electron/package-lock.json" \
    "$scripts/renderer-platform-electron-host.cjs" "$context/hostb/"
  cp "$scripts/renderer-platform-cef/"* "$context/cefhost/"
  cp -R "$bundle" "$context/bundle"
  $gate gtimeout --kill-after=60 21600 docker build --platform linux/amd64 \
    --build-arg "CEF_ARCHIVE=$cef_archive" --build-arg "CEF_SHA256=$cef_sha256" \
    --build-arg "REFERENCE_COMMIT=$actual_reference" --build-arg "BUNDLE_SHA256=$bundle_sha" \
    --file "$dockerfile" --tag "$image_tag" "$context"
  image_id=$(docker image inspect "$image_tag" --format '{{.Id}}')
  printf '%s\n' "$image_id" >"$image_pin_file"
  {
    printf 'image: %s\nimage id: %s\n' "$image_tag" "$image_id"
    docker run --rm --name "spocky-renderer-linux-gate-inspect-$$" --network none "$image_tag" \
      sh -c 'node --version; dpkg-query -W | sort'
  } >"$output_dir/image-packages.txt"
  printf 'pinned %s in %s\n' "$image_id" "$image_pin_file"
  exit 0
fi

[ -f "$image_pin_file" ] || { printf '%s\n' 'No pinned image ID. Run with --build-image first.' >&2; exit 1; }
pinned_id=$(cat "$image_pin_file")
actual_id=$(docker image inspect "$image_tag" --format '{{.Id}}' 2>/dev/null || true)
if [ "$actual_id" != "$pinned_id" ]; then
  printf 'Gate image mismatch: pinned %s, local %s. Run with --build-image.\n' "$pinned_id" "$actual_id" >&2
  exit 1
fi
image_bundle=$(docker image inspect "$image_tag" --format '{{index .Config.Labels "org.spocky.bundle-sha256"}}')
if [ "$image_bundle" != "$bundle_sha" ]; then
  printf 'Bundle changed since the image was built: image %s, bundle %s. Run with --build-image.\n' "$image_bundle" "$bundle_sha" >&2
  exit 1
fi

cleanup() {
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM
set +e
$gate gtimeout --kill-after=30 "$limit_seconds" docker run --name "$container_name" --platform linux/amd64 \
  --cpus "$limit_cpus" --memory "${limit_memory_gb}g" --memory-swap "${limit_memory_gb}g" --shm-size 1g \
  --network none \
  --mount "type=bind,src=$repository_root,dst=/workspace,readonly" \
  --mount "type=bind,src=$output_dir,dst=/output" \
  "$image_tag" sh -c 'sh /workspace/scripts/phase2/renderer-platform-linux-gate-run.sh' \
  >"$output_dir/container.log" 2>&1
status=$?
inspect=$(docker inspect "$container_name" --format '{{json .State}}' 2>/dev/null || true)
host_config=$(docker inspect "$container_name" --format '{"NetworkMode":{{json .HostConfig.NetworkMode}},"Memory":{{json .HostConfig.Memory}},"MemorySwap":{{json .HostConfig.MemorySwap}},"NanoCpus":{{json .HostConfig.NanoCpus}}}' 2>/dev/null || true)
if [ -n "$inspect" ]; then
  printf '{"State":%s,"HostConfig":%s}\n' "$inspect" "$host_config" >"$output_dir/container-state.json"
fi
set -e
if [ "$status" -ne 0 ]; then
  printf 'Linux gate failed with status %s; see %s\n' "$status" "$output_dir/container.log" >&2
  exit "$status"
fi
grep -F -q RENDERER_LINUX_GATE_OK "$output_dir/container.log" || { printf '%s\n' 'Linux gate omitted its success marker' >&2; exit 1; }
printf 'Linux gate evidence: %s\n' "$output_dir"
find "$output_dir" -maxdepth 1 -type f -exec shasum -a 256 {} +
