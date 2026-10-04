#!/bin/sh
# Tests for the shipped-app baseline runner, the CEF/Electron runner, the CDP driver,
# and the runtime comparison. Includes failure paths, not only string checks.
set -eu

scripts="$(CDPATH='' cd -- "$(dirname "$0")" && pwd)"
desktop_runner="$scripts/renderer-platform-desktop-capture-macos.sh"
cef_runner="$scripts/renderer-platform-cef-capture-macos.sh"
compare="$scripts/renderer-platform-runtime-compare.py"
driver="$scripts/renderer-platform-cdp-capture.cjs"
count=0
tmp=$(mktemp -d "${TMPDIR:-/tmp}/spocky-cef-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}
expect_in() {
  grep -F -q -- "$2" "$1" || fail "$1 missing: $2"
  count=$((count + 1))
}
expect_output() {
  printf '%s\n' "$1" | grep -F -q -- "$2" || fail "output missing: $2"
  count=$((count + 1))
}

# Plans.
expect_output "$("$desktop_runner" --print-plan)" 'storage: no seeded storage, the app opens its own page'
expect_output "$("$desktop_runner" --print-plan)" 'isolation: disposable PASEO_HOME, userData and HOME, daemon on a random port, never 6767'
expect_output "$("$cef_runner" --print-plan)" 'SHA-256 c4c07276991f64004201282bc2237c8679444b6a38788253f10e77d72911ddd5'
expect_output "$("$cef_runner" --print-plan)" 'compare: each host against the shipped desktop app, and CEF against Electron on the same bundle'

# Failure path: a reference that is not the pinned commit is refused before any work.
git init -q "$tmp/wrong-reference"
git -C "$tmp/wrong-reference" -c user.name=t -c user.email=t@t commit -q --allow-empty -m wrong
if [ "$(uname -s)" = Darwin ]; then
  if PASEO_REFERENCE_ROOT="$tmp/wrong-reference" "$desktop_runner" >"$tmp/out" 2>&1; then
    fail 'desktop runner accepted a wrong reference commit'
  fi
  grep -F -q 'Paseo reference mismatch' "$tmp/out" || fail 'desktop runner gave no reference mismatch message'
  count=$((count + 2))
  # Failure path: an archive whose SHA-256 is not the pinned one is refused before any work.
  mkdir "$tmp/pins"
  printf 'not the pinned archive\n' >"$tmp/pins/cef_binary_152.0.7+g83ffcba+chromium-152.0.7977.83_macosx64_minimal.tar.bz2"
  if SPOCKY_CEF_PINS="$tmp/pins" "$cef_runner" >"$tmp/out" 2>&1; then
    fail 'CEF runner accepted an archive with the wrong SHA-256'
  fi
  grep -F -q 'CEF archive SHA-256 mismatch' "$tmp/out" || fail 'CEF runner gave no SHA-256 mismatch message'
  count=$((count + 2))
fi

# Behavior of the comparison: identical captures are in exact membership, one differing
# pixel is not, and the difference box is exact. Nothing is masked or thresholded.
python3 - "$compare" "$tmp" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image

compare, tmp = sys.argv[1], Path(sys.argv[2])
tree = {"role": "RootWebArea", "name": "x", "ignored": False, "properties": [["url", "u"]], "children": []}
focus = {"entries": [{"tag": "button", "role": "button", "label": "A", "text": "A", "disabled": False}], "completed": True}
activation = {"changed": True, "urlBefore": "http://h/a", "urlAfter": "http://h/b", "after": {"dialogs": [], "status": []}}


def write(name, pixels, ax_name="x"):
    image = Image.new("RGBA", (8, 6), (255, 255, 255, 255))
    for position, value in pixels.items():
        image.putpixel(position, value)
    image.save(tmp / f"{name}.png")
    nodes = [
        {"nodeId": f"id-{name}-1", "role": {"value": "RootWebArea"}, "name": {"value": ax_name}, "childIds": [f"id-{name}-2"], "backendDOMNodeId": 7 + len(name)},
        {"nodeId": f"id-{name}-2", "role": {"value": "button"}, "name": {"value": "A"}, "childIds": [], "parentId": f"id-{name}-1"},
    ]
    (tmp / f"{name}.ax.json").write_text(json.dumps({"nodes": nodes}))
    (tmp / f"{name}.json").write_text(json.dumps({
        "axRawFile": f"{name}.ax.json", "consoleMessages": [], "pageErrors": [], "failedRequests": [], "errorResponses": [],
        "name": name, "browser": {"product": "Chrome/1"}, "screenshot": {"file": f"{name}.png"},
        "keyboardFocus": focus, "addProjectActivation": activation, "plusActivation": activation, "axTree": tree,
    }))


def run(left, right):
    out = tmp / f"{left}-{right}.json"
    subprocess.run([sys.executable, compare, str(tmp / f"{left}.json"), str(tmp / f"{right}.json"), str(out)],
                   check=True, capture_output=True)
    return json.loads(out.read_text())


write("base", {})
write("same", {})
write("one", {(3, 2): (254, 255, 255, 255)})
write("renamed", {}, ax_name="y")
same = run("base", "same")
assert same["visual"]["exactMembership"] and same["visual"]["differentPixels"] == 0
# Raw accessibility compare: ids differ between captures but are generated, so they are normalized.
assert same["accessibilityRaw"]["equal"] and same["accessibilityRaw"]["differentNodeIndexes"] == []
renamed = run("base", "renamed")
assert not renamed["accessibilityRaw"]["equal"] and renamed["accessibilityRaw"]["differentNodeIndexes"] == [0]
assert same["pageOutputRaw"]["consoleMessages"]["equal"]
assert same["visual"]["differenceBoundingBox"] == []
one = run("base", "one")
assert not one["visual"]["exactMembership"]
assert one["visual"]["differentPixels"] == 1
assert one["visual"]["differenceBoundingBox"] == [3, 2, 4, 3]
assert one["visual"]["normalizedRmse"] > 0
assert one["method"]["normalization"] == "none" and one["method"]["threshold"] == "none"
PY
count=$((count + 11))

# The driver refuses port 6767 and unknown modes.
expect_in "$driver" 'refusing port 6767'
if node "$driver" 1 - "$tmp" n bogus >/dev/null 2>&1; then fail 'driver accepted an unknown mode'; fi
count=$((count + 1))

# Pinned output is recorded raw: no truncation or whitespace collapsing of page text.
if grep -F -q -e '.slice(0, 120)' -e 'replace(/\s+/g' "$driver"; then fail 'driver truncates or collapses page text'; fi
count=$((count + 1))
expect_in "$driver" 'consoleMessages'
expect_in "$driver" 'Accessibility.getFullAXTree'

# The desktop and candidate modes seed no storage on either side.
expect_in "$driver" 'storageSeeding: mode === "original"'
expect_in "$driver" 'if (mode === "original") await page.addInitScript('

if rg -n '\x{2014}' "$scripts"/renderer-platform-cef* "$scripts"/renderer-platform-desktop* "$scripts"/renderer-platform-runtime* ; then
  fail 'renderer CEF scripts contain forbidden em dash'
fi
count=$((count + 1))

printf '%s renderer CEF script assertions passed\n' "$count"
