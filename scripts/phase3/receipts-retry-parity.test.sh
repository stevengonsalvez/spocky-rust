#!/bin/sh
# Proves scripts/phase3/receipts-retry-parity.sh exits 0 only on a clean match
# and nonzero on every defect. A fake gate writes evidence in the real gate's
# layout (verdicts, side.json with byte-array stdout, stub records, receipts)
# with one injected defect per case:
#
#   good, and good with SPOCKY_ALLOW_SKIP=1 exported (the runner must unset it)
#   gate exits nonzero, unsupported gate, no evidence line
#   self-check or parity verdict failed, missing, with differences, check
#     failures, or a discovery error
#   wrong daemon kind, failed probe step
#   stub turns 4 (a retry started a turn) and 2
#   probe outcomes differ between sides, or conflict text is not the expected
#   send receipt pending, count 3, count 1, fingerprints differ, and both sides
#     alike pending or alike holding three receipts
#   the probe's three wire blocks ("# recording client", "# retry-other
#     connection", "# race second connection") missing, or out of order
#   a block's server_info frame missing, twice (recording client), not first
#     on both sides, differing in any of the three blocks, in another key
#     order, the original lacking features.workspaceLabels, or spocky
#     advertising it as false; and spocky advertising it as true (the gap
#     closed) or sending extra pongs is accepted
#   dirty worktree
#
# Every case runs the committed runner from a throwaway detached worktree at
# HEAD, removed on exit, so the clean-tree check sees a clean tree.
# SPOCKY_RETRY_TEST_TREE names a clean git repository to run the runner from
# instead; it exists so a mutated runner can be tried in a throwaway repository.
#
# Usage: scripts/phase3/receipts-retry-parity.test.sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

work=$(mktemp -d /tmp/spocky-retry-parity-test.XXXXXX)
tree=${SPOCKY_RETRY_TEST_TREE:-}
cleanup() {
  if [ -z "${SPOCKY_RETRY_TEST_TREE:-}" ] && [ -d "$work/tree" ]; then
    git -C "$repository_root" worktree remove --force "$work/tree" || true
  fi
  case "$work" in
    /tmp/spocky-retry-parity-test.*) rm -rf "$work" ;;
    *) printf 'refusing to remove unexpected test directory: %s\n' "$work" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

if [ -z "$tree" ]; then
  tree=$work/tree
  git -C "$repository_root" worktree add --quiet --detach "$tree" HEAD
fi
runner="$tree/scripts/phase3/receipts-retry-parity.sh"

cat >"$work/fake-gate.py" <<'EOF'
import json, os, sys

case = os.environ["FAKE_CASE"]
root = os.environ["FAKE_ROOT"]
outcomes = [
    {"step": "created", "ok": True, "error": None},
    {"step": "first", "ok": True, "error": None},
    {"step": "retry", "ok": True, "error": None},
    {"step": "retry-other", "ok": True, "error": None},
    {"step": "conflict", "ok": False, "error": "agent_request_key_conflict"},
    {"step": "race", "ok": True, "error": None},
]
verdict = {
    "gate": "g4-retry", "pass": True, "differences": [], "discoveryError": None,
    "comparisonError": None, "checkFailures": [], "survivors": [], "harnessErrors": [],
}


def compact(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def server_info(kind, block="client"):
    """The handshake frame: spocky omits features.workspaceLabels (DWLABEL-001)."""
    features = {"workspaceLabels": True, "voice": False}
    if kind == "spocky":
        del features["workspaceLabels"]
    frame = {"type": "status", "payload": {
        "status": "server_info", "serverId": "srv_" + ("aB-_cD0123xy" if kind == "original" else "zZ9_-Y876543"),
        "hostname": "host", "version": "1.2.3", "features": features}}
    if case == "server-info-gap-closed" and kind == "spocky":
        frame["payload"]["features"] = {"workspaceLabels": True, "voice": False}
    if case == "server-info-spocky-false" and kind == "spocky":
        frame["payload"]["features"] = {"workspaceLabels": False, "voice": False}
    if case == "server-info-original-no-labels" and kind == "original":
        frame["payload"]["features"] = {"voice": False}
    if case == "server-info-differs" and kind == "spocky" and block == "client":
        frame["payload"]["version"] = "9.9.9"
    if case == "other-info-differs" and kind == "spocky" and block == "other":
        frame["payload"]["version"] = "9.9.9"
    if case == "race-info-differs" and kind == "spocky" and block == "race":
        frame["payload"]["version"] = "9.9.9"
    if case == "server-info-key-order" and kind == "spocky":
        frame["payload"] = {"serverId": frame["payload"]["serverId"], "status": "server_info",
                            "hostname": "host", "version": "1.2.3", "features": frame["payload"]["features"]}
    return compact(frame)


def side(kind, mutate_outcomes=None):
    shown = json.loads(json.dumps(outcomes))
    if mutate_outcomes:
        mutate_outcomes(shown)
    pong = '{"type":"pong"}'
    # The probe leaves out the bare heartbeat pong, so a clean side has none.
    client = [server_info(kind, "client"), '{"type":"session","message":{"type":"fetch_agent_response"}}']
    other = [server_info(kind, "other")]
    race = [server_info(kind, "race")]
    if case == "server-info-both-late":
        client = [client[1], client[0]]
    if kind == "spocky":
        if case == "server-info-missing":
            client = client[1:]
        if case == "server-info-twice":
            client = [client[0], client[0], client[1]]
        if case == "other-info-missing":
            other = []
        if case == "race-info-missing":
            race = []
        if case == "pongs-differ":
            client = [client[0], pong, pong, client[1]]
    lines = [compact({"outcomes": shown, "workspaceId": "wks_0123456789abcdef"})]
    blocks = [("# recording client", client), ("# retry-other connection", other), ("# race second connection", race)]
    if kind == "spocky" and case == "no-blocks":
        lines += client + other + race
    elif kind == "spocky" and case == "blocks-reversed":
        lines += [line for label, frames in reversed(blocks) for line in [label] + frames]
    elif kind == "spocky" and case == "race-block-missing":
        lines += [line for label, frames in blocks[:2] for line in [label] + frames]
    else:
        lines += [line for label, frames in blocks for line in [label] + frames]
    stdout = "\n".join(lines) + "\n"
    turns = 4 if case == "turns-4" and kind == "spocky" else 2 if case == "turns-2" and kind == "spocky" else 3
    exit_code = 1 if case == "probe-exit" and kind == "spocky" else 0
    return {
        "kind": "original" if case == "wrong-kind" and kind == "spocky" else kind,
        "steps": [{
            "name": "probe", "argv": [], "stderr": [],
            "exit": {"kind": "code", "value": exit_code},
            "stdout": list(stdout.encode()), "stub_requests": turns,
        }],
        "stub_records": [{"n": n} for n in range(turns)],
    }


def receipts(kind):
    prints = ["a" * 64, "b" * 64]
    if case == "receipt-fp" and kind == "spocky":
        prints[1] = "c" * 64
    states = ["completed", "completed"]
    if case == "receipt-pending" and kind == "spocky":
        states[1] = "pending"
    if case == "receipts-both-pending":
        states[1] = "pending"
    files = [{"fingerprint": prints[0], "agentId": ("11111111-1111-4111-8111-111111111111" if kind == "original" else "22222222-2222-4222-8222-222222222222"), "state": states[0]},
             {"fingerprint": prints[1], "agentId": ("11111111-1111-4111-8111-111111111111" if kind == "original" else "22222222-2222-4222-8222-222222222222"), "state": states[1]}]
    if (case == "receipt-3" and kind == "spocky") or case == "receipts-both-3":
        files.append({"fingerprint": "d" * 64, "agentId": ("11111111-1111-4111-8111-111111111111" if kind == "original" else "22222222-2222-4222-8222-222222222222"), "state": "completed"})
    if case == "receipt-1" and kind == "spocky":
        files = files[:1]
    return files


def write(path, data, raw=False):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb" if raw else "w") as handle:
        handle.write(data if raw else (data if isinstance(data, str) else json.dumps(data)))


evidence = os.path.join(root, "g4-retry-evidence")
for half, left, right in (("self-check", "original", "original"), ("parity", "original", "spocky")):
    value = dict(verdict, left=left, right=right)
    if half == "self-check" and case == "selfcheck-fail":
        value["pass"] = False
    if half == "parity":
        if case == "parity-fail":
            value["pass"] = False
        if case == "parity-diff":
            value["differences"] = ["probe: stdout differs"]
        if case == "parity-check":
            value["checkFailures"] = ["spocky: probe/outcomes/4/ok is None"]
        if case == "parity-discovery":
            value["discoveryError"] = "wall-clock format iso-frac3 equality structure differs"
    if not (half == "parity" and case == "parity-missing"):
        write(os.path.join(evidence, half, "verdict.json"), value)
    for kind, side_dir in ((left, "left-original"), (right, "right-" + right)):
        mutate = None
        if case == "outcome-diff" and kind == "spocky":
            mutate = lambda shown: shown[2].update(ok=False, error="boom")
        if case == "conflict-text":
            mutate = lambda shown: shown[4].update(error="Failed to send agent message")
        write(os.path.join(evidence, half, side_dir, "side.json"), side(kind, mutate))
        for index, receipt in enumerate(receipts(kind)):
            write(os.path.join(evidence, half, side_dir, "files", "paseo-home", "agent-requests", "%064x.json" % index),
                  json.dumps(receipt, indent=2))
print("gate g4-retry evidence: " + evidence)
EOF

cat >"$work/fake-gate.sh" <<EOF
#!/bin/sh
# Fake gate. Exits 97 when the runner leaked SPOCKY_ALLOW_SKIP.
[ -z "\${SPOCKY_ALLOW_SKIP+set}" ] || exit 97
case "\$FAKE_CASE" in
  unsupported)
    printf 'unsupported gate: g4-retry\n' >&2
    exit 1
    ;;
esac
mkdir -p "\$FAKE_ROOT"
if [ "\$FAKE_CASE" = no-evidence-line ]; then
  printf 'gate g4-retry ran\n'
  exit 0
fi
python3 "$work/fake-gate.py"
[ "\$FAKE_CASE" != gate-exit ] || exit 1
EOF
chmod +x "$work/fake-gate.sh"

# run_case <label> <expected: 0 or nonzero> <env assignment>...
run_case() {
  label=$1
  expected=$2
  shift 2
  if env "$@" FAKE_CASE="$label" FAKE_ROOT="$work/$label-gate" \
    SPOCKY_RETRY_GATE="$work/fake-gate.sh" SPOCKY_RETRY_EVIDENCE_ROOT="$work/$label" \
    "$runner" >"$work/$label.log" 2>&1; then
    actual=0
  else
    actual=nonzero
  fi
  if [ "$actual" != "$expected" ]; then
    cat "$work/$label.log" >&2
    p3_fail "case $label: expected exit $expected, got $actual"
  fi
  printf 'ok %s (exit %s)\n' "$label" "$actual"
}

run_case good 0
grep -F 'passed' "$work/good.log" >/dev/null
run_case allow-skip 0 SPOCKY_ALLOW_SKIP=1
run_case server-info-gap-closed 0
run_case pongs-differ 0
for label in gate-exit unsupported no-evidence-line selfcheck-fail parity-fail parity-missing \
  parity-diff parity-check parity-discovery wrong-kind probe-exit turns-4 turns-2 \
  outcome-diff conflict-text receipt-pending receipt-3 receipt-1 receipt-fp \
  receipts-both-pending receipts-both-3 server-info-missing server-info-twice \
  server-info-differs server-info-key-order server-info-original-no-labels \
  server-info-both-late server-info-spocky-false other-info-missing other-info-differs \
  no-blocks blocks-reversed race-block-missing race-info-missing race-info-differs; do
  run_case "$label" nonzero
done
grep -F 'expected 3' "$work/turns-4.log" >/dev/null
grep -F 'not completed' "$work/receipt-pending.log" >/dev/null
grep -F 'outcomes differ from the expected outcomes' "$work/outcome-diff.log" >/dev/null

printf 'probe\n' >"$tree/retry-dirty-probe"
run_case dirty-tree nonzero
grep -F 'worktree is not clean' "$work/dirty-tree.log" >/dev/null
rm "$tree/retry-dirty-probe"

printf 'receipts-retry-parity.test.sh: 41 cases passed\n'
