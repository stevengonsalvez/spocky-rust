#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
session="spocky-hub-postgres-$$"
container="spocky-hub-postgres-$$"
log_file="$repository_root/evidence/raw/phase2/hub-postgres-runtime.log"
test_log="$repository_root/evidence/raw/phase2/hub-postgres-test.log"

if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded PostgreSQL runtime evidence\n' >&2
  exit 1
fi

cleanup() {
  docker stop "$container" >/dev/null 2>&1 || true
  tmux kill-session -t "$session" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$repository_root/evidence/raw/phase2"
tmux new-session -d -s "$session" -n postgres
tmux send-keys -t "$session:postgres" \
  "docker run --rm --name $container -e POSTGRES_PASSWORD=paseo-test -e POSTGRES_DB=paseo_hub -p 127.0.0.1::5432 postgres:17-alpine 2>&1 | tee '$log_file'" C-m

attempt=0
while [ "$attempt" -lt 60 ]; do
  if docker exec "$container" pg_isready -U postgres -d paseo_hub >/dev/null 2>&1; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done
if [ "$attempt" -ge 60 ]; then
  printf 'PostgreSQL did not become ready within 60 seconds\n' >&2
  exit 1
fi

port=$(docker port "$container" 5432/tcp | sed -n 's/.*://p')
if [ -z "$port" ]; then
  printf 'Could not resolve disposable PostgreSQL port\n' >&2
  exit 1
fi

cd "$repository_root"
set +e
PASEO_TEST_POSTGRES_URL="postgres://postgres:paseo-test@127.0.0.1:$port/paseo_hub" \
  gtimeout 120 cargo test -p spocky-hub-pilot \
    --test postgres_runtime --test relational_api_keys --test relational_invitations \
    --test relational_sessions -- --nocapture \
  >"$test_log" 2>&1
test_status=$?
set -e
cat "$test_log"
if [ "$test_status" -ne 0 ]; then
  exit "$test_status"
fi
