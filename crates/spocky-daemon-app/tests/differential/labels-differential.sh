#!/bin/sh
# Workspace label wire differential. A pinned client creates a workspace, then
# drives every label RPC: an empty list, an owned subscription, assignments
# (new, existing with another colour, second, unassign of an unknown label),
# edits (rename and recolour, collision, unknown, no-op, empty name), delete
# inspect and delete (known and unknown), and lists with no cursor, a caught-up
# cursor and an expired one. It records the exact wire text of the server_info
# frame, every label frame and every rpc_error in arrival order (the client's
# raw payload hook, so key order and unknown keys show). That runs once
# against the pinned daemon and once against spocky-daemon.
#
# Checks, in order:
# - the recorded frame sequences are byte-identical after masking, the
#   server_info frame (features.workspaceLabels included) with them;
# - the persisted workspace-labels.json and workspaces.json after SIGTERM are
#   byte-identical after masking, and no transaction journal is left behind.
#
# Masking replaces only the root path, each generated id with a token numbered
# by first appearance (so reuse and rotation stay visible), and generated
# timestamps.
#
# Each daemon runs under sandbox-exec (loopback egress only) in a named tmux
# session on the lane's socket, on a disposable home and a random port that is
# never 6767 or 6768. Only recorded PIDs are signalled, and an exit trap stops
# the daemon and the exact tmux session and removes the root on any failure.
# The whole run is bounded at 600 seconds.
#
# Usage: labels-differential.sh <out-dir>
# Env: CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p4_daemon_services),
#      which holds debug/spocky-daemon.
set -u
GT=/usr/local/bin/gtimeout
if [ -z "${SPOCKY_LABELS_BOUNDED:-}" ]; then
  SPOCKY_LABELS_BOUNDED=1 exec "$GT" --kill-after=30 600 "$0" "$@"
fi
here=$(cd "$(dirname "$0")" && pwd)
top=${1:?usage: labels-differential.sh <out-dir>}
target=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p4_daemon_services}

pid=""; session=""; root=""
SOCK=spocky

cleanup() {
  [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null
  if [ -n "$pid" ] && [ -n "$root" ]; then
    i=0; while [ $i -lt 150 ] && [ ! -s "$root/daemon.exit" ]; do sleep 0.1; i=$((i+1)); done
    [ -s "$root/daemon.exit" ] || kill -KILL "$pid" 2>/dev/null
  fi
  [ -n "$session" ] && tmux -L "$SOCK" kill-session -t "=$session" 2>/dev/null
  case "$root" in /private/tmp/spocky-p4-labels-*) rm -rf "$root" ;; esac
  pid=""; session=""; root=""
}
trap cleanup EXIT
trap 'exit 130' INT TERM

run_side() {
side=$1; out=$2
PASEO_ROOT=/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204
NODE_BIN=$HOME/.nvm/versions/node/v22.20.0/bin
SPOCKY=$target/debug/spocky-daemon
PROFILE='(version 1)(allow default)(deny network-outbound (remote ip "*:*"))(allow network-outbound (remote ip "localhost:*"))'
mkdir -p "$out"
root=$(mktemp -d /private/tmp/spocky-p4-labels-XXXXXXXX)
for d in home paseo-home project tmp; do mkdir -p "$root/$d"; done
(cd "$root/project" && printf 'hello\n' >README.md && env -i PATH=/usr/bin:/bin HOME="$root/home" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t sh -c 'git init -q -b main && git add README.md && git commit -q -m init')
while :; do
  port=$(jot -r 1 20000 60000)
  [ "$port" != 6767 ] && [ "$port" != 6768 ] && ! nc -z 127.0.0.1 "$port" 2>/dev/null && break
done
printf '{"daemon":{"listen":"127.0.0.1:%s","relay":{"enabled":false}},"features":{"dictation":{"enabled":false},"voiceMode":{"enabled":false}}}\n' "$port" >"$root/paseo-home/config.json"
if [ "$side" = original ]; then cmd="$NODE_BIN/node $PASEO_ROOT/packages/cli/dist/index.js daemon run"; else cmd="$SPOCKY"; fi
ENVV="PATH=$NODE_BIN:/usr/bin:/bin:/usr/sbin:/sbin HOME=$root/home PASEO_HOME=$root/paseo-home TMPDIR=$root/tmp TZ=UTC USERPROFILE=$root/home"
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
session="spocky-p4-labels-$side-$(date +%s)"
tmux -L "$SOCK" new-session -d -s "$session" "$root/run.sh"
i=0; pid=""; while [ $i -lt 100 ]; do [ -s "$root/daemon.pid" ] && pid=$(cat "$root/daemon.pid") && break; sleep 0.1; i=$((i+1)); done
[ -n "$pid" ] || { echo "FAIL: $side daemon did not start"; exit 1; }
echo "$pid" >"$out/pids"
cli() { env -i $ENVV $GT --kill-after=5 120 "$PASEO_ROOT/packages/cli/bin/paseo" "$@"; }
i=0; while [ $i -lt 60 ]; do cli ls --host "127.0.0.1:$port" --json >/dev/null 2>&1 && break; sleep 1; i=$((i+1)); done
env -i $ENVV $GT --kill-after=5 200 "$NODE_BIN/node" "$here/labels-subscriber.mjs" "$PASEO_ROOT" "127.0.0.1:$port" "$root/project" >"$out/frames.jsonl" 2>"$out/subscriber.err"; echo "subscriber exit=$?"
# SIGTERM by recorded PID: the graceful stop must leave the persisted files.
kill -TERM "$pid" 2>/dev/null
i=0; while [ $i -lt 150 ] && [ ! -s "$root/daemon.exit" ]; do sleep 0.1; i=$((i+1)); done
[ -s "$root/daemon.exit" ] || { echo "FAIL: $side daemon did not exit within 15s of SIGTERM"; exit 1; }
for file in workspace-labels.json workspaces.json; do
  [ -f "$root/paseo-home/projects/$file" ] || { echo "FAIL: $side left no projects/$file"; exit 1; }
  cp "$root/paseo-home/projects/$file" "$out/$file"
done
[ ! -e "$root/paseo-home/projects/workspace-labels.transaction.json" ] ||
  { echo "FAIL: $side left a label transaction journal"; exit 1; }
cp "$root/daemon.out" "$out/daemon.out"; cp "$root/paseo-home/daemon.log" "$out/daemon.log" 2>/dev/null
echo "daemon exit=$(cat "$out/daemon.exit" 2>/dev/null)"
cleanup
echo "$side: recorded $(wc -l <"$out/frames.jsonl" | tr -d ' ') frames"
}

# Numbers each distinct generated id by first appearance, one counter per kind.
mask() {
  perl -pe '
    BEGIN { %m = (); %n = () }
    sub tok { my ($kind, $value) = @_; $m{"$kind$value"} //= "<$kind:" . (++$n{$kind}) . ">"; }
    s#/private/tmp/spocky-p4-labels-[A-Za-z0-9]+#<ROOT>#g;
    s/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/tok("UUID", $1)/ge;
    s/(wks_[0-9a-f]+)/tok("WKS", $1)/ge;
    s/(prj_[0-9a-f]+)/tok("PRJ", $1)/ge;
    s/(srv_[A-Za-z0-9_-]{12})/tok("SRV", $1)/ge;
    s/20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g;
  ' "$1"
}

# The mask numbers a reused id once and a new one next.
[ "$(printf 'a 123e4567-e89b-12d3-a456-426614174000 b 123e4567-e89b-12d3-a456-426614174001 c 123e4567-e89b-12d3-a456-426614174000\n' | mask /dev/stdin)" = 'a <UUID:1> b <UUID:2> c <UUID:1>' ] ||
  { echo "FAIL: mask does not number ids by first appearance"; exit 1; }

echo "commit: $(git -C "$here" rev-parse HEAD)"
status=0
run_side original "$top/original"
run_side spocky "$top/spocky"
for side in original spocky; do
  [ -s "$top/$side/frames.jsonl" ] || { echo "FAIL: $side recorded no frames"; exit 1; }
  mask "$top/$side/frames.jsonl" >"$top/$side/masked.jsonl"
  for file in workspace-labels.json workspaces.json; do mask "$top/$side/$file" >"$top/$side/$file.masked"; done
  head -n 1 "$top/$side/subscriber.err" >/dev/null
done
frames_server_info() { grep -c '"status":"server_info"' "$1"; }
[ "$(frames_server_info "$top/original/masked.jsonl")" = 1 ] && [ "$(frames_server_info "$top/spocky/masked.jsonl")" = 1 ] ||
  { echo "FAIL: each side must record exactly one server_info frame"; exit 1; }
echo "frames: original $(wc -l <"$top/original/masked.jsonl" | tr -d ' ') spocky $(wc -l <"$top/spocky/masked.jsonl" | tr -d ' ')"
echo "outcomes original: $(tail -n 1 "$top/original/subscriber.err")"
echo "outcomes spocky:   $(tail -n 1 "$top/spocky/subscriber.err")"
for side in original spocky; do
  for file in frames.jsonl masked.jsonl workspace-labels.json workspaces.json; do
    printf 'sha256 %s  %s/%s\n' "$(shasum -a 256 "$top/$side/$file" | cut -d' ' -f1)" "$side" "$file"
  done
done
if cmp -s "$top/original/masked.jsonl" "$top/spocky/masked.jsonl"; then
  echo "PASS: server_info and label frames are byte-identical after masking"
else
  echo "FAIL: frame sequences differ"; diff "$top/original/masked.jsonl" "$top/spocky/masked.jsonl" | head -40; status=1
fi
for file in workspace-labels.json workspaces.json; do
  if cmp -s "$top/original/$file.masked" "$top/spocky/$file.masked"; then
    echo "PASS: persisted $file is byte-identical after masking"
  else
    echo "FAIL: persisted $file differs"; diff "$top/original/$file.masked" "$top/spocky/$file.masked" | head -20; status=1
  fi
done
exit $status
