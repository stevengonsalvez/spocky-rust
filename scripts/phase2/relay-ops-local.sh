#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
main_root=${PASEO_RUST_MAIN_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust}
baseline=${PASEO_RELAY_BASELINE_ROOT:-$main_root/.baselines/relay}
target_dir=${CARGO_TARGET_DIR:-$root/.target-relay-ops}
run_id=$$
container="spocky-relay-ops-baseline-$run_id"
image="spocky-relay-ops-baseline:$run_id"
rust_session="spocky-relay-ops-rust-$run_id"
rust_tls_session="spocky-relay-ops-rust-tls-$run_id"
baseline_tls_session="spocky-relay-ops-baseline-tls-$run_id"
rust_sample_session="spocky-relay-ops-rust-sample-$run_id"
baseline_sample_session="spocky-relay-ops-baseline-sample-$run_id"
temp_home=$(mktemp -d "${TMPDIR:-/tmp}/spocky-relay-ops.XXXXXX")
rust_fifo="$temp_home/rust.stdin"
rust_log="$temp_home/rust.log"
rust_samples="$temp_home/rust-samples.tsv"
baseline_samples="$temp_home/baseline-samples.tsv"

cleanup() {
  status=$?
  trap - EXIT INT TERM
  exec 9>&- 2>/dev/null || true
  for session in "$rust_session" "$rust_tls_session" "$baseline_tls_session" \
    "$rust_sample_session" "$baseline_sample_session"; do
    if tmux has-session -t "$session" 2>/dev/null; then
      tmux kill-session -t "$session"
    fi
  done
  docker rm --force "$container" >/dev/null 2>&1 || true
  rm -rf "$temp_home"
  exit "$status"
}
trap cleanup EXIT INT TERM

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("", 0)); print(s.getsockname()[1]); s.close()'
}

wait_http() {
  url=$1
  attempts=0
  until curl --noproxy '*' --fail --silent --show-error "$url" >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 100 ]; then
      echo "endpoint did not become ready: $url" >&2
      return 1
    fi
    sleep 0.1
  done
}

wait_tls() {
  port=$1
  attempts=0
  until curl --noproxy '*' --fail --silent --show-error \
    --cacert "$temp_home/ca.pem" --resolve "relay.local:$port:$local_ip" \
    "https://relay.local:$port/health" >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 100 ]; then
      echo "TLS endpoint did not become ready on port $port" >&2
      return 1
    fi
    sleep 0.1
  done
}

check_wss_upgrade() {
  port=$1
  output="$temp_home/wss-$port.txt"
  set +e
  curl --noproxy '*' --http1.1 --include --silent --show-error --max-time 2 \
    --cacert "$temp_home/ca.pem" --resolve "relay.local:$port:$local_ip" \
    --header 'Connection: Upgrade' --header 'Upgrade: websocket' \
    --header 'Sec-WebSocket-Version: 13' \
    --header 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
    "https://relay.local:$port/ws?serverId=tls-check&role=server&v=1" > "$output"
  status=$?
  set -e
  if [ "$status" -ne 0 ] && [ "$status" -ne 28 ]; then
    cat "$output" >&2
    return "$status"
  fi
  grep -q '^HTTP/1.1 101' "$output"
}

wait_log() {
  pattern=$1
  file=$2
  attempts=0
  until grep -q "$pattern" "$file" 2>/dev/null; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 100 ]; then
      echo "log did not contain $pattern: $file" >&2
      return 1
    fi
    sleep 0.1
  done
}

local_interface=$(route -n get default | awk '/interface:/{print $2; exit}')
local_ip=${SPOCKY_RELAY_TEST_HOST:-$(ipconfig getifaddr "$local_interface")}
case "$local_ip" in
  127.*|"") echo "non-loopback local IPv4 address required" >&2; exit 1 ;;
esac

test "$(git -C "$baseline" rev-parse HEAD)" = "3fc41c96c8c63f3a7109e832899cc57d473c4531"
test -z "$(git -C "$baseline" status --short)"

cd "$root"
CARGO_TARGET_DIR="$target_dir" cargo build -p spocky-relay-pilot --bin spocky-relay-network-node

rust_peer_port=$(free_port)
rust_port=$(free_port)
baseline_port=$(free_port)
rust_tls_port=$(free_port)
baseline_tls_port=$(free_port)
mkfifo "$rust_fifo"
tmux new-session -d -s "$rust_session" \
  "exec env SPOCKY_RELAY_HOST='$local_ip' SPOCKY_RELAY_MAX_WEBSOCKETS=256 '$target_dir/debug/spocky-relay-network-node' rust < '$rust_fifo' 2>&1 | tee '$rust_log'"
exec 9>"$rust_fifo"
wait_log '^READY' "$rust_log"
rust_ready=$(grep '^READY' "$rust_log" | head -1)
rust_pid=$(printf '%s\n' "$rust_ready" | cut -f3)
actual_rust_peer=$(printf '%s\n' "$rust_ready" | cut -f4)
actual_rust_endpoint=$(printf '%s\n' "$rust_ready" | cut -f5)
rust_peer_port=${actual_rust_peer##*:}
rust_port=${actual_rust_endpoint##*:}

docker build --memory 2g --cpu-quota 200000 --tag "$image" "$baseline"
docker run --detach --name "$container" --cpus 2 --memory 2g \
  --env PASEO_RELAY_HOST=0.0.0.0 \
  --env PASEO_RELAY_PORT=4000 \
  --env PASEO_RELAY_ACCEPTORS=1 \
  --env PASEO_RELAY_CONNECTIONS_PER_ACCEPTOR=256 \
  --env PASEO_RELAY_DELIVERY_TIMEOUT_MS=500 \
  --env PASEO_RELAY_TRANSPORT_SEND_TIMEOUT_MS=1000 \
  --publish "$baseline_port:4000" "$image" >/dev/null
wait_http "http://$local_ip:$baseline_port/health"
wait_http "http://$local_ip:$rust_port/health"

HOME="$temp_home" openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -subj '/CN=Spocky Relay Ops Local CA' \
  -keyout "$temp_home/ca.key" -out "$temp_home/ca.pem" >/dev/null 2>&1
HOME="$temp_home" openssl req -newkey rsa:2048 -nodes \
  -subj '/CN=relay.local' \
  -keyout "$temp_home/relay.key" -out "$temp_home/relay.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:relay.local,IP:%s\nextendedKeyUsage=serverAuth\n' "$local_ip" > "$temp_home/relay.ext"
HOME="$temp_home" openssl x509 -req -days 1 -sha256 \
  -in "$temp_home/relay.csr" -CA "$temp_home/ca.pem" -CAkey "$temp_home/ca.key" \
  -CAcreateserial -extfile "$temp_home/relay.ext" -out "$temp_home/relay.pem" >/dev/null 2>&1

proxy="$root/scripts/phase2/relay-ops-tls-proxy.py"
tmux new-session -d -s "$rust_tls_session" \
  "exec python3 '$proxy' --listen-host '$local_ip' --listen-port '$rust_tls_port' --upstream-host '$local_ip' --upstream-port '$rust_port' --cert '$temp_home/relay.pem' --key '$temp_home/relay.key'"
tmux new-session -d -s "$baseline_tls_session" \
  "exec python3 '$proxy' --listen-host '$local_ip' --listen-port '$baseline_tls_port' --upstream-host '$local_ip' --upstream-port '$baseline_port' --cert '$temp_home/relay.pem' --key '$temp_home/relay.key'"
wait_tls "$rust_tls_port"
wait_tls "$baseline_tls_port"
check_wss_upgrade "$rust_tls_port"
check_wss_upgrade "$baseline_tls_port"

load_client="$baseline/scripts/relay-load.mjs"
tmux new-session -d -s "$rust_sample_session" \
  "for i in \$(seq 1 12); do ps -o rss=,%cpu= -p '$rust_pid'; sleep 0.5; done > '$rust_samples'"
tmux new-session -d -s "$baseline_sample_session" \
  "for i in \$(seq 1 12); do docker stats --no-stream --format '{{.MemUsage}}\t{{.CPUPerc}}' '$container'; sleep 0.5; done > '$baseline_samples'"

node "$load_client" --endpoints "ws://$local_ip:$baseline_port/ws" \
  --scenario sustained --pairs 100 --batch-size 50 --rate 10 --duration 3 \
  --payload-bytes 1024 --cleanup-grace 5 --drain-timeout 5 > "$temp_home/baseline-load.json" || {
    cat "$temp_home/baseline-load.json" >&2
    exit 1
  }
node "$load_client" --endpoints "ws://$local_ip:$rust_port/ws" \
  --scenario sustained --pairs 100 --batch-size 50 --rate 10 --duration 3 \
  --payload-bytes 1024 --cleanup-grace 5 --drain-timeout 5 --relay-pid "$rust_pid" > "$temp_home/rust-load.json" || {
    cat "$temp_home/rust-load.json" >&2
    exit 1
  }

set +e
node "$load_client" --endpoints "ws://$local_ip:$baseline_port/ws" \
  --scenario ownership --servers 280 --batch-size 50 --duration 0 \
  --cleanup-grace 5 --drain-timeout 5 > "$temp_home/baseline-admission.json"
baseline_admission_status=$?
node "$load_client" --endpoints "ws://$local_ip:$rust_port/ws" \
  --scenario ownership --servers 280 --batch-size 50 --duration 0 \
  --cleanup-grace 5 --drain-timeout 5 > "$temp_home/rust-admission.json"
rust_admission_status=$?
set -e
test "$baseline_admission_status" -eq 1
test "$rust_admission_status" -eq 1
jq -e '.connection_successes == 256 and .connection_failures == 24' \
  "$temp_home/baseline-admission.json" "$temp_home/rust-admission.json" >/dev/null

attempts=0
while tmux has-session -t "$rust_sample_session" 2>/dev/null || tmux has-session -t "$baseline_sample_session" 2>/dev/null; do
  attempts=$((attempts + 1))
  if [ "$attempts" -ge 60 ]; then
    echo "resource samplers exceeded 30 seconds" >&2
    exit 1
  fi
  sleep 0.5
done

baseline_metrics=$(curl --noproxy '*' --fail --silent --show-error "http://$local_ip:$baseline_port/metrics")
rust_metrics=$(curl --noproxy '*' --fail --silent --show-error "http://$local_ip:$rust_port/metrics")
printf '%s\n' "$baseline_metrics" | grep -q '^paseo_relay_inflight_delivery_bytes 0$'
printf '%s\n' "$baseline_metrics" | grep -q '^paseo_relay_backpressured_sources 0$'
printf '%s\n' "$rust_metrics" | grep -q '^spocky_relay_inflight_delivery_bytes 0$'
printf '%s\n' "$rust_metrics" | grep -q '^spocky_relay_backpressured_sources 0$'

rust_sample_count=$(wc -l < "$rust_samples" | tr -d ' ')
baseline_sample_count=$(wc -l < "$baseline_samples" | tr -d ' ')
rust_resource_range=$(awk '
  NR==1 {rss_min=$1; rss_max=$1; cpu_min=$2; cpu_max=$2}
  {if($1<rss_min)rss_min=$1; if($1>rss_max)rss_max=$1; if($2<cpu_min)cpu_min=$2; if($2>cpu_max)cpu_max=$2}
  END {printf "rss_kib_min=%s,rss_kib_max=%s,cpu_min=%s,cpu_max=%s",rss_min,rss_max,cpu_min,cpu_max}
' "$rust_samples")
baseline_resource_range=$(awk '
  {gsub(/MiB/, "", $1); gsub(/%/, "", $4)}
  NR==1 {mem_min=$1; mem_max=$1; cpu_min=$4; cpu_max=$4}
  {if($1<mem_min)mem_min=$1; if($1>mem_max)mem_max=$1; if($4<cpu_min)cpu_min=$4; if($4>cpu_max)cpu_max=$4}
  END {printf "memory_mib_min=%s,memory_mib_max=%s,cpu_min=%s,cpu_max=%s",mem_min,mem_max,cpu_min,cpu_max}
' "$baseline_samples")

echo "non_loopback_ip=$local_ip"
echo "tls_termination=edge-proxy relay_transport=clear-http"
echo "baseline_load=$(cat "$temp_home/baseline-load.json")"
echo "rust_load=$(cat "$temp_home/rust-load.json")"
echo "baseline_admission=$(cat "$temp_home/baseline-admission.json")"
echo "rust_admission=$(cat "$temp_home/rust-admission.json")"
echo "rust_resource_samples=$rust_sample_count $rust_resource_range"
echo "baseline_resource_samples=$baseline_sample_count $baseline_resource_range"
