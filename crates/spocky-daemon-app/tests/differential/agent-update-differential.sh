#!/bin/sh
# agent_update wire differential. A pinned client subscribes to fetch_agents,
# then creates a workspace and a codex full-access agent that runs the G1
# prompt against the loopback Responses stub. It records the
# fetch_agents_response, agent_update and agent.create.response frames in
# arrival order. That runs once against the pinned daemon and once against
# spocky-daemon. Per-run ids, timestamps and root paths are masked, and the
# two frame sequences must then be byte-identical.
#
# Each daemon runs under sandbox-exec (loopback egress only) in a named tmux
# session on the lane's own socket, on a disposable home and a random port
# that is never 6767 or 6768. Only recorded PIDs are signalled.
#
# Usage: agent-update-differential.sh <out-dir>
# Env: CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_g1_wiring),
#      which holds debug/spocky-daemon and debug/spocky-responses-stub.
set -u
here=$(cd "$(dirname "$0")" && pwd)
top=${1:?usage: agent-update-differential.sh <out-dir>}
target=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_g1_wiring}

run_side() {
side=$1; out=$2
PASEO_ROOT=/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204
NODE_BIN=$HOME/.nvm/versions/node/v22.20.0/bin
CODEX=/usr/local/Caskroom/codex/0.159.0/bin/codex
STUB=$target/debug/spocky-responses-stub
SPOCKY=$target/debug/spocky-daemon
GT=/usr/local/bin/gtimeout
SOCK=spocky-p3_g1_wiring
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
exec $CODEX "\$@"
EOS
chmod +x "$root/bin/codex"
(cd "$root/project" && printf 'hello\n' >README.md && env -i PATH=/usr/bin:/bin HOME="$root/home" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t sh -c 'git init -q -b main && git add README.md && git commit -q -m init')
cat >"$root/stub/script.json" <<'EOS'
{"responses":[{"status":200,"events":[{"type":"response.created","response":{"id":"resp_g1_1"}},{"type":"response.output_item.done","item":{"type":"message","role":"assistant","id":"msg_g1_1","content":[{"type":"output_text","text":"READY"}]}},{"type":"response.completed","response":{"id":"resp_g1_1","usage":{"input_tokens":0,"input_tokens_details":null,"output_tokens":0,"output_tokens_details":null,"total_tokens":0}}}],"json":null}]}
EOS
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
EOS
chmod +x "$root/run.sh"
session="spocky-p3-wiring-au-$side-$(date +%s)"
mark daemon-start
tmux -L "$SOCK" new-session -d -s "$session" "$root/run.sh"
i=0; pid=""; while [ $i -lt 100 ]; do [ -s "$root/daemon.pid" ] && pid=$(cat "$root/daemon.pid") && break; sleep 0.1; i=$((i+1)); done
cli() { env -i $ENVV $GT --kill-after=5 120 "$PASEO_ROOT/packages/cli/bin/paseo" "$@"; }
i=0; while [ $i -lt 60 ]; do cli ls --host "127.0.0.1:$port" --json >/dev/null 2>&1 && break; sleep 1; i=$((i+1)); done
mark ready
mark subscriber; env -i $ENVV $GT --kill-after=5 200 "$NODE_BIN/node" $here/agent-update-subscriber.mjs "$PASEO_ROOT" "127.0.0.1:$port" "$root/project" >"$out/frames.jsonl" 2>"$out/subscriber.err"; echo "subscriber exit=$?"
mark stop
[ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null
i=0; while [ $i -lt 150 ] && [ ! -s "$root/daemon.exit" ]; do sleep 0.1; i=$((i+1)); done
[ -n "$pid" ] && [ ! -s "$root/daemon.exit" ] && kill -KILL "$pid"
kill -TERM "$stub_pid" 2>/dev/null
tmux -L "$SOCK" kill-session -t "=$session" 2>/dev/null
cp "$root/daemon.out" "$out/daemon.out"; cp "$root/paseo-home/daemon.log" "$out/daemon.log" 2>/dev/null
echo "daemon exit=$(cat "$root/daemon.exit" 2>/dev/null)"
rm -rf "$root"
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
  mask "$top/$side/frames.jsonl" >"$top/$side/masked.jsonl"
done
echo "frames: original $(wc -l <"$top/original/masked.jsonl") spocky $(wc -l <"$top/spocky/masked.jsonl")"
if cmp -s "$top/original/masked.jsonl" "$top/spocky/masked.jsonl"; then
  echo "PASS: agent_update frame sequences are byte-identical after masking"
else
  echo "FAIL: frame sequences differ"
  diff "$top/original/masked.jsonl" "$top/spocky/masked.jsonl" | head -40
  exit 1
fi
