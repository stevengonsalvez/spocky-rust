#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
script="$repository_root/scripts/phase2/delivery-linux-runtime.sh"

plan=$("$script" --print-plan)
printf '%s\n' "$plan" | grep -F 'baseline: paseo@5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$plan" | grep -F 'image: rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55'
printf '%s\n' "$plan" | grep -F 'lanes: AppImage stable-name update/rollback/uninstall, deb dpkg install/update/rollback/remove/purge'
printf '%s\n' "$plan" | grep -F 'timeout: 1500s hard, then docker rm -f of the exact container name'
printf '%s\n' "$plan" | grep -F 'safety: no host install, no port 6767, no publication, signing, or deployment'

if "$script" --unknown >/dev/null 2>&1; then
  printf 'unknown argument was accepted\n' >&2
  exit 1
fi

# A stub docker that never finishes proves the hard timeout removes only the exact container.
work=$(mktemp -d /tmp/spocky-delivery-linux-test.XXXXXX)
cleanup() {
  case "$work" in
    /tmp/spocky-delivery-linux-test.*) rm -rf "$work" ;;
    *) printf 'refusing to remove unexpected test directory: %s\n' "$work" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM
mkdir "$work/bin"
cat >"$work/bin/docker" <<STUB
#!/bin/sh
case "\$1" in
  info) exit 0 ;;
  run) exec sleep 60 ;;
  rm) printf '%s\n' "\$*" >>"$work/docker-rm.log"; exit 0 ;;
esac
exit 1
STUB
chmod +x "$work/bin/docker"

set +e
PATH="$work/bin:$PATH" SPOCKY_DELIVERY_LINUX_TIMEOUT=2 \
  "$script" --output "$work/out" >"$work/run.out" 2>&1
status=$?
set -e
[ "$status" -eq 124 ] || { printf 'expected timeout status 124, got %s\n' "$status" >&2; exit 1; }
test -f "$work/out/run-metadata.json"
jq -e '.exitStatus == 124 and .timeoutSeconds == 2' "$work/out/run-metadata.json" >/dev/null
container=$(jq -r .container "$work/out/run-metadata.json")
[ "$(cat "$work/docker-rm.log")" = "rm -f $container" ] || {
  printf 'timeout did not remove exactly the recorded container\n' >&2
  exit 1
}
