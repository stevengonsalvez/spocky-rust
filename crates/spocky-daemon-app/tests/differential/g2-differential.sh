#!/bin/sh
# G2 wire differential (permission and cancel). One pinned client
# subscribes to fetch_agents, creates a codex agent in auto mode, then:
#   1. waits for the pending exec approval and allows it;
#   2. sends a second prompt and denies its approval;
#   3. sends a third prompt whose reply the stub holds open, then cancels
#      the turn mid-flight.
# It records every frame it receives except pong heartbeats, plus the
# server_info features. That runs once against the pinned daemon and once
# against spocky-daemon. Per-run ids, timestamps and root paths are masked.
#
# Checks, in order:
# - each side's stub answered exactly the script's requests, with nothing
#   left loopback;
# - the persisted agent record after SIGTERM matches;
# - server_info.features are byte-identical except workspaceLabels, the
#   tracked OPEN gap DWLABEL-001;
# - the agent_update stream and the stream of every other frame must each
#   be byte-identical;
# - the full interleave of the two streams is compared and reported, and a
#   difference fails the run.
#
# Each daemon runs under sandbox-exec (loopback egress only) in a named tmux
# session on the lane's own socket, on a disposable home and a random port
# that is never 6767 or 6768. Only recorded PIDs are signalled, and an exit
# trap cleans up on any failure. The whole run is bounded at 900 seconds.
#
# Usage: g2-differential.sh <out-dir>
# Env: STUB_SCRIPT, the harness G2 stub script JSON (gates.rs g2_script).
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
    -e 's/20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g' "$1"
}

run_side original "$top/original"
run_side spocky "$top/spocky"
for side in original spocky; do
  [ -s "$top/$side/frames.jsonl" ] || { echo "FAIL: $side recorded no frames"; exit 1; }
  head -1 "$top/$side/frames.jsonl" | jq -S .serverInfoFeatures >"$top/$side/features.json"
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
  exit 1
fi
# server_info.features must be byte-identical. The one tracked exception is
# workspaceLabels (OPEN gap DWLABEL-001: the workspace-labels service is not
# ported, so spocky-daemon does not advertise it); any other difference fails.
for side in original spocky; do
  jq -S 'del(.workspaceLabels)' "$top/$side/features.json" >"$top/$side/features-compared.json"
done
if cmp -s "$top/original/features-compared.json" "$top/spocky/features-compared.json" &&
  [ "$(jq '.workspaceLabels' "$top/original/features.json")" = true ] &&
  [ "$(jq '.workspaceLabels' "$top/spocky/features.json")" = null ]; then
  echo "PASS: server_info.features byte-identical except workspaceLabels (DWLABEL-001)"
else
  echo "FAIL: server_info.features differ beyond the tracked workspaceLabels gap"
  diff "$top/original/features.json" "$top/spocky/features.json"
  exit 1
fi
echo "frames: original $(wc -l <"$top/original/masked.jsonl") spocky $(wc -l <"$top/spocky/masked.jsonl")"
for side in original spocky; do
  for file in frames.jsonl masked.jsonl agent-record.json agent-record.masked.json; do
    printf 'sha256 %s  %s/%s\n' "$(shasum -a 256 "$top/$side/$file" | cut -d' ' -f1)" "$side" "$file"
  done
done
status=0
for stream in updates replies; do
  for side in original spocky; do
    if [ "$stream" = updates ]; then
      grep -F '"type":"agent_update"' "$top/$side/masked.jsonl" >"$top/$side/$stream.jsonl"
    else
      grep -vF '"type":"agent_update"' "$top/$side/masked.jsonl" >"$top/$side/$stream.jsonl"
    fi
  done
  if cmp -s "$top/original/$stream.jsonl" "$top/spocky/$stream.jsonl"; then
    echo "PASS: $stream stream byte-identical ($(wc -l <"$top/original/$stream.jsonl" | tr -d ' ') frames)"
  else
    echo "FAIL: $stream stream differs"
    diff "$top/original/$stream.jsonl" "$top/spocky/$stream.jsonl" | head -20
    status=1
  fi
done
if cmp -s "$top/original/masked.jsonl" "$top/spocky/masked.jsonl"; then
  echo "PASS: full interleave byte-identical"
else
  echo "FAIL: interleave differs (agent_update position relative to replies):"
  jq -c '.type' "$top/original/masked.jsonl" >"$top/original/types.txt"
  jq -c '.type' "$top/spocky/masked.jsonl" >"$top/spocky/types.txt"
  diff "$top/original/types.txt" "$top/spocky/types.txt"
  status=1
fi
exit "$status"
