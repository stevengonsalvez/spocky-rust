#!/usr/bin/env python3
"""Builds a replay fixture from recorded `codex app-server` stdio.

The recordings come from the real-codex tests run with SPOCKY_RECORD_DIR set
(see `RECORD_DIR_ENV` in tests/support/mod.rs). Each launch there left
<record dir>/<root dir name>/<pid>/{in,out}.jsonl, and the root dir name is
`spocky-p3-codex-<label>-<pid>-<nanos>`. This script picks, for each scenario,
the newest launch under that label whose client sent `thread/start`, replaces
the disposable root path by `{root}`, checks every line is JSON, and writes
the fixture with its provenance in `_source`:

  * the pinned codex version and binary SHA-256 (checked against the binary),
  * the exact recording command,
  * the SHA-256 of the raw, un-normalized recording.

The fixture's own SHA-256 lives in tests/fixtures/SHA256SUMS, which the replay
differential checks before it reads a fixture.

usage: record_fixture.py <out.json> <recording command> <record dir> <scenario>=<label>...
"""
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

PINNED_CODEX = "/usr/local/Caskroom/codex/0.159.0/bin/codex"
PINNED_CODEX_VERSION = "codex-cli 0.159.0"
ROOT_PATH = re.compile(r"(/private)?/var/folders/[^\"/]+/[^\"/]+/T/spocky-p3-codex-[a-z0-9-]+-\d+-\d+")


def sha256_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def session_launch(record_dir, label):
    """The newest launch of the root labelled `label` that started a thread."""
    root = re.compile(rf"^spocky-p3-codex-{re.escape(label)}-\d+-\d+$")
    launches = []
    for root_dir in Path(record_dir).iterdir():
        if root.match(root_dir.name):
            launches += [d for d in root_dir.iterdir() if (d / "in.jsonl").is_file()]
    for launch in sorted(launches, key=lambda d: d.stat().st_mtime, reverse=True):
        methods = [json.loads(line).get("method") for line in (launch / "in.jsonl").open()]
        if "thread/start" in methods:
            return launch
    sys.exit(f"no recorded session for label {label} under {record_dir}")


def main():
    out, command, record_dir, *pairs = sys.argv[1:]
    codex_sha = sha256_file(PINNED_CODEX)
    version = subprocess.run([PINNED_CODEX, "--version"], capture_output=True, text=True).stdout.strip()
    if version != PINNED_CODEX_VERSION:
        sys.exit(f"codex is {version!r}, not {PINNED_CODEX_VERSION!r}")
    raw = hashlib.sha256()
    scenarios = {}
    for pair in pairs:
        scenario, label = pair.split("=", 1)
        launch = session_launch(record_dir, label)
        sides = {}
        for name in ("in", "out"):
            text = (launch / f"{name}.jsonl").read_text()
            raw.update(text.encode())
            lines = [ROOT_PATH.sub("{root}", line) for line in text.splitlines()]
            for line in lines:
                json.loads(line)
            sides[name] = lines
        scenarios[scenario] = sides
    leftover = [l for s in scenarios.values() for side in s.values() for l in side if "var/folders" in l]
    if leftover:
        sys.exit(f"unnormalized temp path: {leftover[0][:200]}")
    fixture = {
        "_source": {
            "description": "Stdio of a real codex app-server driven by spocky-provider-codex tests, hermetic, loopback stub.",
            "codex": {"version": PINNED_CODEX_VERSION, "sha256": codex_sha},
            "command": command,
            "recording_sha256": raw.hexdigest(),
            "normalization": "each disposable root path is replaced by {root}",
            "fixture_digest": "tests/fixtures/SHA256SUMS",
        },
        "scenarios": scenarios,
    }
    Path(out).write_text(json.dumps(fixture, indent=1) + "\n")
    print(f"wrote {out}: {', '.join(scenarios)}; codex sha256 {codex_sha}; recording sha256 {raw.hexdigest()}")


if __name__ == "__main__":
    main()
