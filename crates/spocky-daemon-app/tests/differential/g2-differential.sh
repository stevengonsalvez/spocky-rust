#!/bin/sh
# G2 wire differential (permission and cancel). One pinned client
# subscribes to fetch_agents, creates a codex agent in auto mode, then:
#   1. waits for the pending exec approval and allows it;
#   2. sends a second prompt and denies its approval;
#   3. sends a third prompt whose reply the stub holds open, then cancels
#      the turn mid-flight.
# It records the exact wire text of every frame it receives except pong
# heartbeats, the server_info frame included. That runs once against the pinned daemon and once
# against spocky-daemon. Per-run ids, timestamps and root paths are masked.
#
# Checks, in order:
# - each side's stub answered exactly the script's requests, with nothing
#   left loopback;
# - the persisted agent record after SIGTERM matches;
# - the server_info frame is byte-identical, key order included, except
#   features.workspaceLabels, the tracked OPEN gap DWLABEL-001;
# - the agent_update stream and the stream of every other frame must each
#   be byte-identical to that stream in at least one pinned run (the pinned
#   daemon itself varies between runs in a few update frames, so no single
#   pinned run is the reference);
# - the interleave of the two streams: the pinned daemon is run
#   ORIGINAL_RUNS times (default 5), the frame pairs whose order holds in
#   every pinned run are kept (the same stable-pair partial order the
#   harness uses for codex-io), and spocky must keep every one of them. A
#   pair that pinned itself reorders between runs is not compared. Only
#   frames present in every pinned run form pairs; spocky must contain all
#   of those and no frame that no pinned run produced.
#
# Every check runs; any failure makes the exit status 1.
#
# Each daemon runs under sandbox-exec (loopback egress only) in a named tmux
# session on the lane's own socket, on a disposable home and a random port
# that is never 6767 or 6768. Only recorded PIDs are signalled, and an exit
# trap cleans up on any failure. The whole run is bounded at 900 seconds.
#
# Usage: g2-differential.sh <out-dir>
# Env: ORIGINAL_RUNS, how many pinned runs give the stable pairs (default 5).
#      STUB_SCRIPT, the harness G2 stub script JSON (gates.rs g2_script).
#      STUB_BIN, a responses stub that honours hold_ms (default
#      $CARGO_TARGET_DIR/debug/spocky-responses-stub).
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_g1_wiring).
set -u
GT=/usr/local/bin/gtimeout
if [ -z "${SPOCKY_G2_BOUNDED:-}" ]; then
  SPOCKY_G2_BOUNDED=1 exec "$GT" --kill-after=30 900 "$0" "$@"
fi
here=$(cd "$(dirname "$0")" && pwd)
STUB_SCRIPT=${STUB_SCRIPT:?}
SUBSCRIBER=$here/g2-subscriber.mjs
top=${1:?usage: g2-differential.sh <out-dir>}
target=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_g1_wiring}

stub_pid=""; pid=""; tracker_pid=""; session=""; root=""
SOCK=spocky-p3_g1_wiring

# Stops what this side started, by recorded PID and exact session name only.
cleanup() {
  [ -n "$tracker_pid" ] && kill -TERM "$tracker_pid" 2>/dev/null
  [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null
  if [ -n "$pid" ] && [ -n "$root" ]; then
    i=0; while [ $i -lt 150 ] && [ ! -s "$root/daemon.exit" ]; do sleep 0.1; i=$((i+1)); done
    [ -s "$root/daemon.exit" ] || kill -KILL "$pid" 2>/dev/null
  fi
  [ -n "$stub_pid" ] && kill -TERM "$stub_pid" 2>/dev/null
  [ -n "$session" ] && tmux -L "$SOCK" kill-session -t "=$session" 2>/dev/null
  case "$root" in /private/tmp/spocky-p3-wiring-au-*) rm -rf "$root" ;; esac
  stub_pid=""; pid=""; tracker_pid=""; session=""; root=""
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Records the daemon's process tree (worker, plugins, codex) until stopped.
track_tree() {
  while :; do
    ps -A -o pid=,ppid= | awk -v seeds="$(tr '\n' ' ' <"$1")" '
      BEGIN { n = split(seeds, s, " "); for (i = 1; i <= n; i++) keep[s[i]] = 1 }
      { parent[$1] = $2 }
      END { do { grew = 0; for (p in parent) if (!(p in keep) && (parent[p] in keep)) { keep[p] = 1; grew = 1 } } while (grew); for (p in keep) print p }
    ' >"$1.next" && mv "$1.next" "$1"
    sleep 0.5
  done
}

run_side() {
side=$1; out=$2
started=$(date +%s)
PASEO_ROOT=/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204
NODE_BIN=$HOME/.nvm/versions/node/v22.20.0/bin
CODEX=/usr/local/Caskroom/codex/0.159.0/bin/codex
STUB=${STUB_BIN:-$target/debug/spocky-responses-stub}
SPOCKY=$target/debug/spocky-daemon
PROFILE='(version 1)(allow default)(deny network-outbound (remote ip "*:*"))(allow network-outbound (remote ip "localhost:*"))'
mkdir -p "$out"
root=$(mktemp -d /private/tmp/spocky-p3-wiring-au-XXXXXXXX)
for d in home paseo-home codex-home project bin tmp stub; do mkdir -p "$root/$d"; done
ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time*1000'; }
mark() { echo "$(ms) STEP $*" >>"$out/timeline"; }
: >"$out/timeline"
cat >"$root/bin/codex" <<EOS
#!/bin/sh
echo "\$(perl -MTime::HiRes=time -e 'printf "%d", time*1000') CODEX \$*" >>$out/timeline
echo \$\$ >>$out/pids
exec $CODEX "\$@"
EOS
chmod +x "$root/bin/codex"
(cd "$root/project" && printf 'hello\n' >README.md && env -i PATH=/usr/bin:/bin HOME="$root/home" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t sh -c 'git init -q -b main && git add README.md && git commit -q -m init')
cp "$STUB_SCRIPT" "$root/stub/script.json"
env -i "$STUB" "$root/stub/script.json" "$root/stub/record.jsonl" "$root/stub/port" >"$root/stub/stub.log" 2>&1 &
stub_pid=$!
i=0; while [ $i -lt 100 ] && [ ! -s "$root/stub/port" ]; do sleep 0.1; i=$((i+1)); done
stub_port=$(cat "$root/stub/port")
while :; do
  port=$(jot -r 1 20000 60000)
  [ "$port" != 6767 ] && [ "$port" != 6768 ] && [ "$port" != "$stub_port" ] && ! nc -z 127.0.0.1 "$port" 2>/dev/null && break
done
printf 'model_provider = "spocky-stub"\n\n[model_providers.spocky-stub]\nname = "spocky-stub"\nbase_url = "http://127.0.0.1:%s/v1"\nenv_key = "OPENAI_API_KEY"\nwire_api = "responses"\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n\n[analytics]\nenabled = false\n\n[features]\nplugins = false\n' "$stub_port" >"$root/codex-home/config.toml"
printf '{"daemon":{"listen":"127.0.0.1:%s","relay":{"enabled":false}},"features":{"dictation":{"enabled":false},"voiceMode":{"enabled":false}},"agents":{"providers":{"codex":{"env":{"CODEX_HOME":"%s","OPENAI_BASE_URL":"http://127.0.0.1:%s/v1","OPENAI_API_KEY":"test-key"}}}}}' "$port" "$root/codex-home" "$stub_port" >"$root/paseo-home/config.json"
if [ "$side" = original ]; then cmd="$NODE_BIN/node $PASEO_ROOT/packages/cli/dist/index.js daemon run"; else cmd="$SPOCKY"; fi
ENVV="PATH=$root/bin:$NODE_BIN:/usr/bin:/bin:/usr/sbin:/sbin CODEX_HOME=$root/codex-home HOME=$root/home PASEO_HOME=$root/paseo-home TMPDIR=$root/tmp TZ=UTC USERPROFILE=$root/home"
cat >"$root/run.sh" <<EOS
#!/bin/sh
cd $root
env -i $ENVV /usr/bin/sandbox-exec -p '$PROFILE' $cmd >$root/daemon.out 2>&1 &
echo \$! >$root/daemon.pid
wait \$!
echo \$? >$root/daemon.exit
cp $root/daemon.exit $out/daemon.exit
EOS
chmod +x "$root/run.sh"
session="spocky-p3-wiring-au-$side-$(date +%s)"
mark daemon-start
tmux -L "$SOCK" new-session -d -s "$session" "$root/run.sh"
i=0; pid=""; while [ $i -lt 100 ]; do [ -s "$root/daemon.pid" ] && pid=$(cat "$root/daemon.pid") && break; sleep 0.1; i=$((i+1)); done
[ -n "$pid" ] || { echo "FAIL: $side daemon did not start"; exit 1; }
echo "$pid" >"$out/pids"
track_tree "$out/pids" &
tracker_pid=$!
cli() { env -i $ENVV $GT --kill-after=5 120 "$PASEO_ROOT/packages/cli/bin/paseo" "$@"; }
i=0; while [ $i -lt 60 ]; do cli ls --host "127.0.0.1:$port" --json >/dev/null 2>&1 && break; sleep 1; i=$((i+1)); done
mark ready
mark subscriber; env -i $ENVV $GT --kill-after=5 200 "$NODE_BIN/node" "$SUBSCRIBER" "$PASEO_ROOT" "127.0.0.1:$port" "$root/project" "Run the G2 allow probe." "Run the G2 deny probe." "Start the G2 cancel probe." >"$out/frames.jsonl" 2>"$out/subscriber.err"; echo "subscriber exit=$?"
mark stop
# SIGTERM by recorded PID: the graceful stop closes every agent and persists
# its record, which must then match across sides.
kill -TERM "$pid" 2>/dev/null
i=0; while [ $i -lt 150 ] && [ ! -s "$root/daemon.exit" ]; do sleep 0.1; i=$((i+1)); done
[ -s "$root/daemon.exit" ] || { echo "FAIL: $side daemon did not exit within 15s of SIGTERM"; exit 1; }
records=$(find "$root/paseo-home/agents" -name '*.json' -type f | sort)
[ "$(printf '%s\n' "$records" | grep -c .)" = 1 ] || { echo "FAIL: $side persisted $(printf '%s\n' "$records" | grep -c .) agent records"; exit 1; }
cp $records "$out/agent-record.json"
cp "$root/stub/record.jsonl" "$out/stub-record.jsonl"
cp "$root/daemon.out" "$out/daemon.out"; cp "$root/paseo-home/daemon.log" "$out/daemon.log" 2>/dev/null
daemon_root=$root
cleanup
echo "daemon exit=$(cat "$out/daemon.exit" 2>/dev/null)"
# The stub answered exactly its one scripted model request.
requests=$(wc -l <"$out/stub-record.jsonl" | tr -d ' '); expected=$(jq ".responses | length" "$STUB_SCRIPT")
unscripted=$(jq -s '[.[] | select(.scripted == null or .error != null)] | length' "$out/stub-record.jsonl")
[ "$requests" = "$expected" ] && [ "$unscripted" = 0 ] ||
  { echo "FAIL: $side stub saw $requests requests, $unscripted unscripted"; exit 1; }
# No sandbox network-outbound denial for this side's process tree.
window=$(( $(date +%s) - started + 5 ))
/usr/bin/log show --last "${window}s" --style compact \
  --predicate 'eventMessage CONTAINS "deny" AND eventMessage CONTAINS "network-outbound"' >"$out/egress.log" ||
  { echo "FAIL: $side egress check could not read the kernel log"; exit 1; }
violations=$(awk -v pids="$(tr '\n' ' ' <"$out/pids")" '
  BEGIN { n = split(pids, p, " "); for (i = 1; i <= n; i++) mine[p[i]] = 1 }
  /Sandbox: / { if (match($0, /\(([0-9]+)\) deny/)) { pid = substr($0, RSTART + 1, RLENGTH - 7); if (pid in mine) print } }
' "$out/egress.log")
[ -z "$violations" ] || { echo "FAIL: $side egress: $violations"; exit 1; }
echo "$side: stub $requests scripted requests, no egress from $(wc -l <"$out/pids" | tr -d ' ') tracked pids ($daemon_root)"
}

mask() {
  sed -E -e 's#/private/tmp/spocky-p3-wiring-au-[A-Za-z0-9]+#<ROOT>#g' \
    -e 's/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/<UUID>/g' \
    -e 's/wks_[0-9a-f]+/<WKS>/g' \
    -e 's/prj_[0-9a-f]+/<PRJ>/g' \
    -e 's/srv_[A-Za-z0-9_-]{12}/<SRV>/g' \
    -e 's/20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g' "$1"
}

# `generateServerId` is srv_ plus 12 base64url characters, which can include
# - and _.
[ "$(printf 'x srv_aB-_cD0123xy y\n' | mask /dev/stdin)" = 'x <SRV> y' ] ||
  { echo "FAIL: mask misses a base64url server id"; exit 1; }

echo "commit: $(git -C "$here" rev-parse HEAD)"
original_runs=${ORIGINAL_RUNS:-5}
status=0
run_side original "$top/original"
n=2
while [ "$n" -le "$original_runs" ]; do
  run_side original "$top/original-$n"
  n=$((n + 1))
done
run_side spocky "$top/spocky"
for side in original spocky; do
  [ -s "$top/$side/frames.jsonl" ] || { echo "FAIL: $side recorded no frames"; exit 1; }
  head -1 "$top/$side/frames.jsonl" >"$top/$side/server-info.json"
  tail -n +2 "$top/$side/frames.jsonl" >"$top/$side/frames-only.jsonl"
  mask "$top/$side/frames-only.jsonl" >"$top/$side/masked.jsonl"
  mask "$top/$side/agent-record.json" >"$top/$side/agent-record.masked.json"
  [ "$(jq -r .lastStatus "$top/$side/agent-record.json")" = closed ] ||
    { echo "FAIL: $side agent record lastStatus is $(jq -r .lastStatus "$top/$side/agent-record.json"), not closed"; exit 1; }
done
if cmp -s "$top/original/agent-record.masked.json" "$top/spocky/agent-record.masked.json"; then
  echo "PASS: persisted agent records are byte-identical after masking (lastStatus closed)"
else
  echo "FAIL: persisted agent records differ"
  diff "$top/original/agent-record.masked.json" "$top/spocky/agent-record.masked.json" | head -20
  status=1
fi
# The server_info frame must be byte-identical, wire key order included,
# except one tracked key: features.workspaceLabels (OPEN gap DWLABEL-001: the
# workspace-labels service is not ported, so spocky-daemon does not
# advertise it). jq -c keeps key order; any other difference fails.
info_path='if .message then .message.payload.features else .payload.features end'
for side in original spocky; do
  mask "$top/$side/server-info.json" |
    jq -c 'if .message then del(.message.payload.features.workspaceLabels) else del(.payload.features.workspaceLabels) end' \
    >"$top/$side/server-info-compared.json"
done
if cmp -s "$top/original/server-info-compared.json" "$top/spocky/server-info-compared.json" &&
  [ "$(jq "$info_path | .workspaceLabels" "$top/original/server-info.json")" = true ] &&
  [ "$(jq "$info_path | .workspaceLabels" "$top/spocky/server-info.json")" = null ]; then
  echo "PASS: server_info frame byte-identical except features.workspaceLabels (DWLABEL-001)"
else
  echo "FAIL: server_info frame differs beyond the tracked workspaceLabels gap"
  diff "$top/original/server-info-compared.json" "$top/spocky/server-info-compared.json"
  status=1
fi
echo "frames: original $(wc -l <"$top/original/masked.jsonl") spocky $(wc -l <"$top/spocky/masked.jsonl")"
for side in original spocky; do
  for file in frames.jsonl masked.jsonl agent-record.json agent-record.masked.json; do
    printf 'sha256 %s  %s/%s\n' "$(shasum -a 256 "$top/$side/$file" | cut -d' ' -f1)" "$side" "$file"
  done
done
split_streams() {
  grep -F '"type":"agent_update"' "$1/masked.jsonl" >"$1/updates.jsonl"
  grep -vF '"type":"agent_update"' "$1/masked.jsonl" >"$1/replies.jsonl"
}
pinned_dirs="$top/original"
n=2
while [ "$n" -le "$original_runs" ]; do pinned_dirs="$pinned_dirs $top/original-$n"; n=$((n + 1)); done
for dir in $pinned_dirs "$top/spocky"; do
  [ "$dir" = "$top/original" ] || [ "$dir" = "$top/spocky" ] || {
    tail -n +2 "$dir/frames.jsonl" >"$dir/frames-only.jsonl"
    mask "$dir/frames-only.jsonl" >"$dir/masked.jsonl"
  }
  split_streams "$dir"
done
for stream in updates replies; do
  matched=""
  for dir in $pinned_dirs; do
    if cmp -s "$dir/$stream.jsonl" "$top/spocky/$stream.jsonl"; then matched="$matched $(basename "$dir")"; fi
  done
  if [ -n "$matched" ]; then
    echo "PASS: $stream stream byte-identical to pinned run(s):$matched ($(wc -l <"$top/spocky/$stream.jsonl" | tr -d ' ') frames)"
  else
    echo "FAIL: $stream stream matches none of the $original_runs pinned runs; against the first:"
    diff "$top/original/$stream.jsonl" "$top/spocky/$stream.jsonl" | head -20
    status=1
  fi
done
# Stable-pair partial order. A frame is its masked text plus how many equal
# frames came before it; a pair (a, b) is stable when a precedes b in every
# pinned run.
runs="$top/original/masked.jsonl"
for n in $(seq 2 "$original_runs"); do runs="$runs $top/original-$n/masked.jsonl"; done
python3 - "$top/spocky/masked.jsonl" $runs <<'PY' || status=1
import collections, sys

def keyed(path):
    seen = collections.Counter()
    keys = []
    for line in open(path):
        line = line.rstrip("\n")
        seen[line] += 1
        keys.append((line, seen[line]))
    return keys

spocky = keyed(sys.argv[1])
pinned = [keyed(path) for path in sys.argv[2:]]
if len(pinned) < 5:
    print(f"FAIL: {len(pinned)} pinned runs; the stable-pair check needs at least 5")
    sys.exit(1)
common = set(pinned[0]).intersection(*map(set, pinned[1:]))
seen = set(pinned[0]).union(*map(set, pinned[1:]))
if not common <= set(spocky):
    print("FAIL: spocky lacks frames every pinned run has:")
    for key in sorted(common - set(spocky))[:5]:
        print("  missing:", key[0][:110])
    sys.exit(1)
if not set(spocky) <= seen:
    print("FAIL: spocky sent frames no pinned run produced:")
    for key in sorted(set(spocky) - seen)[:5]:
        print("  extra:", key[0][:110])
    sys.exit(1)
frames = common
positions = [{key: index for index, key in enumerate(run)} for run in pinned]
where = {key: index for index, key in enumerate(spocky)}
stable = 0
broken = []
for first in frames:
    for second in frames:
        if first != second and all(run[first] < run[second] for run in positions):
            stable += 1
            if not where[first] < where[second]:
                broken.append((first, second))
print(f"interleave: {len(pinned)} pinned runs, {len(common)} frames in all of them "
      f"({len(seen) - len(common)} in only some), {stable} stable ordered pairs; "
      f"spocky broke {len(broken)}")
for (first, _), (second, _) in broken[:10]:
    print("  spocky reorders:", first[:90], "->", second[:90])
if broken:
    print("FAIL: spocky does not keep every stable pinned frame order")
    sys.exit(1)
print("PASS: spocky keeps every stable pinned frame order")
PY
exit "$status"
