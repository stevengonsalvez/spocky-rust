#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
plan=$("$repository_root/scripts/phase2/renderer-platform-linux.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'rust image digest: sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55'
printf '%s\n' "$plan" | grep -F 'container limits: 2 CPUs, 3 GB memory, 1200 seconds'
printf '%s\n' "$plan" | grep -F 'memory swap: 3g total, no swap beyond memory'
printf '%s\n' "$plan" | grep -F 'viewport: 1280x800, scale 1, light theme, en-US, DejaVu Sans'
printf '%s\n' "$plan" | grep -F 'visual: complete SHA-256 membership plus unmasked RGBA RMSE'
printf '%s\n' "$plan" | grep -F 'accessibility: complete AT-SPI tree plus observed focus order'
printf '%s\n' "$plan" | grep -F 'interaction: xdotool Tab then Return on first Plus control'
printf '%s\n' "$plan" | grep -F 'evidence/phase2/renderer-platform-linux/'

runner="$repository_root/scripts/phase2/renderer-platform-linux.sh"
# The plan and the docker run read the same limit variables, never literals.
if grep -E -- '--cpus [0-9]|--memory [0-9]|--memory-swap [0-9]' "$runner"; then
  printf '%s\n' 'runner hardcodes container limits instead of the shared variables' >&2
  exit 1
fi
grep -F -- '--memory-swap "${limit_memory_gb}g"' "$runner"
# An interrupted runner removes only its own exact container name.
grep -F 'docker rm -f "$container_name"' "$runner"
grep -F "trap cleanup EXIT" "$runner"
grep -F "trap 'cleanup; exit 143' TERM" "$runner"

# Dialog detection reads the AT-SPI role, never a substring of the whole tree.
python3 - "$repository_root/scripts/phase2/renderer-platform-linux-tree.py" <<'PY'
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
if grep -F 'json.dumps(tree_after_plus)' "$repository_root/scripts/phase2/renderer-platform-linux-atspi.py"; then
  printf '%s\n' 'atspi script detects dialogs by substring instead of role' >&2
  exit 1
fi

if rg -n '\x{2014}' \
  "$repository_root/scripts/phase2/renderer-platform-linux.sh" \
  "$repository_root/scripts/phase2/renderer-platform-linux.test.sh" \
  "$repository_root/scripts/phase2/renderer-platform-linux-atspi.py" \
  "$repository_root/scripts/phase2/renderer-platform-linux-compare.py" \
  "$repository_root/scripts/phase2/renderer-platform-linux-tree.py"; then
  printf '%s\n' 'renderer Linux scripts contain forbidden em dash' >&2
  exit 1
fi

printf '%s\n' '18 renderer platform script assertions passed'
