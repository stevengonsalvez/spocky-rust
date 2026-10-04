#!/usr/bin/python3
"""Compare a candidate runtime capture with a baseline capture, with no masking.

usage: renderer-platform-runtime-compare.py BASELINE_JSON CANDIDATE_JSON OUTPUT_JSON

Each input is a JSON written by renderer-platform-cdp-capture.cjs next to its PNG.
Membership is exact full-PNG SHA-256 equality. RMSE and the difference box are
reported unmasked. The focus walk, activations, and accessibility tree are
compared field for field.
"""
import hashlib
import json
import math
import sys
from pathlib import Path

from PIL import Image, ImageChops


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def tree_paths(node, prefix=""):
    """Flatten an AX tree to (path, role, name) rows so differences can be listed."""
    here = f"{prefix}/{node['role']}"
    rows = [(here, node["role"], node["name"])]
    for index, child in enumerate(node["children"]):
        rows.extend(tree_paths(child, f"{here}[{index}]"))
    return rows


def tree_differences(a, b, path=""):
    """List every field difference between two AX trees, in document order."""
    found = []
    for key in ("role", "name", "ignored"):
        if a[key] != b[key]:
            found.append({"path": path or "/", "field": key, "baseline": a[key], "candidate": b[key]})
    if a["properties"] != b["properties"]:
        pa = {name: value for name, value in a["properties"]}
        pb = {name: value for name, value in b["properties"]}
        changed = {n: [pa.get(n), pb.get(n)] for n in sorted(set(pa) | set(pb)) if pa.get(n) != pb.get(n)}
        found.append({"path": path or "/", "field": "properties", "changed": changed})
    if len(a["children"]) != len(b["children"]):
        found.append({"path": path or "/", "field": "childCount", "baseline": len(a["children"]), "candidate": len(b["children"])})
    for index, (child_a, child_b) in enumerate(zip(a["children"], b["children"])):
        found.extend(tree_differences(child_a, child_b, f"{path}/{index}"))
    return found


def main():
    baseline_json, candidate_json, output = map(Path, sys.argv[1:4])
    baseline = json.loads(baseline_json.read_text(encoding="utf-8"))
    candidate = json.loads(candidate_json.read_text(encoding="utf-8"))
    baseline_png = baseline_json.with_name(baseline["screenshot"]["file"])
    candidate_png = candidate_json.with_name(candidate["screenshot"]["file"])
    a = Image.open(baseline_png).convert("RGBA")
    b = Image.open(candidate_png).convert("RGBA")
    comparable = a.size == b.size
    visual = {
        "baselineSha256": sha256(baseline_png),
        "candidateSha256": sha256(candidate_png),
        "exactMembership": sha256(baseline_png) == sha256(candidate_png),
        "baselineSize": list(a.size),
        "candidateSize": list(b.size),
        "comparable": comparable,
    }
    if comparable:
        diff = ImageChops.difference(a, b)
        histogram = diff.histogram()
        squared = sum((value % 256) ** 2 * count for value, count in enumerate(histogram))
        channels = a.width * a.height * 4
        visual["normalizedRmse"] = math.sqrt(squared / channels) / 255.0
        # A pixel differs when any of its four channels differs.
        r, g, b_, a_ = diff.split()
        any_channel = ImageChops.lighter(ImageChops.lighter(r, g), ImageChops.lighter(b_, a_))
        visual["differentPixels"] = sum(any_channel.histogram()[1:])
        visual["differenceBoundingBox"] = list(diff.convert("RGB").getbbox() or [])
    base_focus = baseline["keyboardFocus"]
    cand_focus = candidate["keyboardFocus"]
    focus = {
        "baselineEntries": len(base_focus["entries"]),
        "candidateEntries": len(cand_focus["entries"]),
        "baselineCompleted": base_focus["completed"],
        "candidateCompleted": cand_focus["completed"],
        "equal": base_focus == cand_focus,
    }
    if not focus["equal"]:
        pairs = list(zip(base_focus["entries"], cand_focus["entries"]))
        focus["firstDifferentIndex"] = next((i for i, (x, y) in enumerate(pairs) if x != y), None)
    activations = {}
    for key in ("addProjectActivation", "plusActivation"):
        x, y = baseline[key], candidate[key]
        activations[key] = {
            "baselineChanged": x["changed"],
            "candidateChanged": y["changed"],
            "baselineDialogs": x["after"]["dialogs"],
            "candidateDialogs": y["after"]["dialogs"],
            "dialogsEqual": x["after"]["dialogs"] == y["after"]["dialogs"],
            "baselinePathAfter": x["urlAfter"].split("://", 1)[-1].split("/", 1)[-1],
            "candidatePathAfter": y["urlAfter"].split("://", 1)[-1].split("/", 1)[-1],
        }
    base_rows = tree_paths(baseline["axTree"])
    cand_rows = tree_paths(candidate["axTree"])
    accessibility = {
        "baselineNodes": len(base_rows),
        "candidateNodes": len(cand_rows),
        "equal": baseline["axTree"] == candidate["axTree"],
        "rolesAndNamesEqual": [r[1:] for r in base_rows] == [r[1:] for r in cand_rows],
        "differences": tree_differences(baseline["axTree"], candidate["axTree"])[:40],
    }
    def raw_ax(document, path):
        # Only generated ids are normalized (to their order of appearance): nodeId,
        # backendDOMNodeId, parentId, childIds, frameId. Every other field is compared raw.
        nodes = json.loads((path.with_name(document["axRawFile"])).read_text(encoding="utf-8"))["nodes"]
        index = {node["nodeId"]: i for i, node in enumerate(nodes)}
        out = []
        for node in nodes:
            copy = {k: v for k, v in node.items() if k not in ("nodeId", "backendDOMNodeId", "parentId", "childIds", "frameId")}
            copy["childIndexes"] = [index[c] for c in node.get("childIds", [])]
            out.append(copy)
        return out

    base_raw = raw_ax(baseline, baseline_json)
    cand_raw = raw_ax(candidate, candidate_json)
    raw_differences = [i for i, (x, y) in enumerate(zip(base_raw, cand_raw)) if x != y]
    raw_differences += list(range(min(len(base_raw), len(cand_raw)), max(len(base_raw), len(cand_raw))))
    page_output = {
        key: {"baseline": baseline.get(key), "candidate": candidate.get(key), "equal": baseline.get(key) == candidate.get(key)}
        for key in ("consoleMessages", "pageErrors", "failedRequests", "errorResponses")
    }
    # Diagnostic only: page focus, hover and window geometry do not decide membership.
    focus_keys = ("hasFocus", "visibilityState", "activeElement", "hoverMatches", "focusVisibleMatches")
    page_focus = {
        "diagnosticOnly": True,
        "baseline": baseline.get("pageFocus"),
        "candidate": candidate.get("pageFocus"),
        "stateEqual": all((baseline.get("pageFocus") or {}).get(k) == (candidate.get("pageFocus") or {}).get(k) for k in focus_keys),
        "stateKeys": list(focus_keys),
    }
    report = {
        "method": {
            "membership": "complete PNG SHA-256 equality",
            "rmse": "sqrt(mean squared RGBA byte difference) / 255",
            "normalization": "none",
            "mask": "none",
            "threshold": "none",
        },
        "baseline": {"name": baseline["name"], "browser": baseline["browser"], "storageSeeding": baseline.get("storageSeeding")},
        "candidate": {"name": candidate["name"], "browser": candidate["browser"], "storageSeeding": candidate.get("storageSeeding")},
        "visual": visual,
        "focusWalk": focus,
        "activations": activations,
        "accessibilityTree": accessibility,
        "accessibilityRaw": {
            "baselineNodes": len(base_raw),
            "candidateNodes": len(cand_raw),
            "equal": base_raw == cand_raw,
            "differentNodeIndexes": raw_differences[:100],
            "normalized": "nodeId, backendDOMNodeId, parentId, childIds, frameId only (generated ids)",
        },
        "pageOutputRaw": page_output,
        "pageFocus": page_focus,
        "inExactMembership": visual["exactMembership"],
    }
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(
        f"membership={visual['exactMembership']} rmse={visual.get('normalizedRmse')} "
        f"focusEqual={focus['equal']} axEqual={accessibility['equal']}"
    )


main()
