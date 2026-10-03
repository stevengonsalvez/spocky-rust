#!/bin/sh
set -eu

scripts="$(CDPATH='' cd -- "$(dirname "$0")" && pwd)"
runner="$scripts/renderer-platform-linux.sh"
dockerfile="$scripts/renderer-platform-linux.Dockerfile"
plan=$("$runner" --print-plan)
count=0

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}
expect_plan() {
  printf '%s\n' "$plan" | grep -F -q -- "$1" || fail "plan missing: $1"
  count=$((count + 1))
}
expect_in() {
  grep -F -q -- "$2" "$1" || fail "$1 missing: $2"
  count=$((count + 1))
}
expect_absent() {
  if grep -E -q -- "$2" "$1"; then fail "$1 must not contain: $2"; fi
  count=$((count + 1))
}

expect_plan 'rust image digest: sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55'
expect_plan 'derived image: pinned by local image ID in renderer-platform-linux.image-id'
expect_plan 'network: none at run time, dependencies baked into the derived image'
expect_plan 'container limits: 2 CPUs, 3 GB memory, 1200 seconds'
expect_plan 'memory swap: 3g total, no swap beyond memory'
expect_plan 'viewport: 1280x800, scale 1, light theme, en-US, DejaVu Sans'
expect_plan 'visual: complete SHA-256 membership plus unmasked RGBA RMSE'
expect_plan 'accessibility: complete AT-SPI tree plus observed focus order'
expect_plan 'interaction: xdotool Tab then Return on first Plus control'
expect_plan 'evidence/phase2/renderer-platform-linux/'

# The plan and the docker run read the same limit variables, never literals.
expect_absent "$runner" '--cpus [0-9]|--memory [0-9]|--memory-swap [0-9]'
expect_in "$runner" '--memory-swap "${limit_memory_gb}g"'

# An interrupted runner removes only its own exact container name.
expect_in "$runner" 'docker rm -f "$container_name"'
expect_in "$runner" 'trap cleanup EXIT'
expect_in "$runner" "trap 'cleanup; exit 143' TERM"

# Runs are offline: packages and crates come from the pinned derived image.
expect_in "$runner" '--network none'
expect_in "$runner" 'cargo build --locked --offline'
expect_absent "$runner" 'apt-get'
expect_in "$runner" 'Derived image mismatch'
if ! grep -E -q '^sha256:[0-9a-f]{64}$' "$scripts/renderer-platform-linux.image-id"; then
  fail 'image pin file is not a sha256 image ID'
fi
count=$((count + 1))
base_in_runner=$(sed -n 's/^image_digest=//p' "$runner")
expect_in "$dockerfile" "FROM rust@$base_in_runner"
# rust-toolchain.toml pins a version the base lacks, so the image must carry it.
expect_in "$dockerfile" 'COPY rust-toolchain.toml'
expect_in "$dockerfile.dockerignore" '!rust-toolchain.toml'

# Offline resolution needs every workspace package, so a stale image must be refused.
expect_in "$dockerfile" 'cargo fetch --locked'
expect_in "$dockerfile" 'LABEL org.spocky.cargo-lock-sha256=$CARGO_LOCK_SHA256'
expect_in "$runner" '--build-arg "CARGO_LOCK_SHA256=$cargo_lock_sha"'
expect_in "$runner" 'Cargo.lock changed since the image was built'

# The image rebuilds to the same package versions: snapshot sources, every package pinned.
expect_in "$dockerfile" 'snapshot.debian.org/archive/debian/20261001T170000Z/'
expect_in "$dockerfile" 'snapshot.debian.org/archive/debian-security/20261001T170000Z/'
pinned=$(grep -c -E '^    [a-z0-9.+-]+=[^ ]+ \\$' "$dockerfile")
unpinned=$(sed -n '/apt-get install/,/&& sed/p' "$dockerfile" | grep -c -E '^    [a-z0-9.+-]+ \\$' || true)
[ "$pinned" -eq 17 ] || fail "expected 17 pinned apt packages, found $pinned"
[ "$unpinned" -eq 0 ] || fail "found $unpinned apt packages without a version"
count=$((count + 2))

# The run records the network and limits Docker applied, not only the exit state.
expect_in "$runner" '.HostConfig.NetworkMode'
expect_in "$runner" '.HostConfig.Memory}}'
expect_in "$runner" '.HostConfig.MemorySwap'
expect_in "$runner" '.HostConfig.NanoCpus'

# Activation is compared from the same focus state, snapshotted right before Return.
expect_in "$scripts/renderer-platform-linux-atspi.py" 'tree_before_plus != tree_after_plus'
expect_in "$scripts/renderer-platform-linux-atspi.py" 'tree_before_add != tree_after_add'
expect_absent "$scripts/renderer-platform-linux-atspi.py" 'tree_before != tree_after'

# Dialog detection reads the AT-SPI role, never a substring of the whole tree.
python3 - "$scripts/renderer-platform-linux-tree.py" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("tree", sys.argv[1])
tree = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tree)
dialog = {"role": "frame", "name": None, "children": [{"role": "dialog", "name": "Add project", "children": []}]}
decoy = {"role": "frame", "name": "dialog", "children": [{"role": "push button", "name": "Open dialog", "children": []}]}
assert tree.contains_role(dialog, "dialog")
assert not tree.contains_role(decoy, "dialog")
assert not tree.contains_role({"role": "frame", "children": []}, "dialog")
PY
count=$((count + 3))
expect_absent "$scripts/renderer-platform-linux-atspi.py" 'json\.dumps\(tree_after_plus\)'

if rg -n '\x{2014}' "$scripts"/renderer-platform-linux* ; then
  fail 'renderer Linux scripts contain forbidden em dash'
fi
count=$((count + 1))

printf '%s renderer platform script assertions passed\n' "$count"
