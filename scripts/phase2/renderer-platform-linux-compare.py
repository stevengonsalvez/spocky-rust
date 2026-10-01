#!/usr/bin/python3
"""Compare the Linux desktop capture with both immutable browser images."""

import hashlib
import json
import math
import sys
from pathlib import Path

from PIL import Image, ImageChops


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def comparison(candidate: Image.Image, baseline_path: Path) -> dict:
    baseline = Image.open(baseline_path).convert("RGBA")
    if baseline.size != candidate.size:
        return {
            "baseline": baseline_path.name,
            "baselineSha256": sha256(baseline_path),
            "dimensions": list(baseline.size),
            "comparable": False,
            "reason": "dimension mismatch",
        }

    difference = ImageChops.difference(candidate, baseline)
    histogram = difference.histogram()
    squared_error = sum((value % 256) ** 2 * count for value, count in enumerate(histogram))
    channels = candidate.width * candidate.height * 4
    different_pixels = sum(
        1 for candidate_pixel, baseline_pixel in zip(candidate.getdata(), baseline.getdata())
        if candidate_pixel != baseline_pixel
    )
    return {
        "baseline": baseline_path.name,
        "baselineSha256": sha256(baseline_path),
        "dimensions": list(baseline.size),
        "comparable": True,
        "differentPixels": different_pixels,
        "normalizedRmse": math.sqrt(squared_error / channels) / 255.0,
        "differenceBoundingBox": list(difference.convert("RGB").getbbox() or []),
    }


candidate_path = Path(sys.argv[1])
baseline_paths = [Path(argument) for argument in sys.argv[2:4]]
output_path = Path(sys.argv[4])
candidate = Image.open(candidate_path).convert("RGBA")
candidate_sha = sha256(candidate_path)
comparisons = [comparison(candidate, path) for path in baseline_paths]
accepted_hashes = [item["baselineSha256"] for item in comparisons]
report = {
    "method": {
        "membership": "complete PNG SHA-256 equality against either pinned desktop image",
        "rmse": "sqrt(mean squared RGBA byte difference) / 255",
        "normalization": "none",
        "mask": "none",
        "threshold": "none",
    },
    "candidate": {
        "path": candidate_path.name,
        "sha256": candidate_sha,
        "dimensions": list(candidate.size),
    },
    "acceptedBaselineSha256": accepted_hashes,
    "exactMembership": candidate_sha in accepted_hashes,
    "comparisons": comparisons,
}
output_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

