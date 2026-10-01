# Phase 3 slice runtime pins. Sourced by scripts/phase3/*.sh; never executed.
# Every value here is a fact recorded in evidence/phase3/slice-plan.md section 1
# or measured on the pinned host when the harness lane installed the runtime.

P3_PASEO_COMMIT=5de45e208690b0efc51c59a585ae9729325a9204
P3_PASEO_LOCK_SHA256=844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6

# Node 22.20.0 from Paseo .tool-versions, installed by nvm from
# https://nodejs.org/dist/v22.20.0/node-v22.20.0-darwin-x64.tar.xz
# (nvm verified the tarball against SHASUMS256.txt before install).
P3_NODE_VERSION=22.20.0
P3_NODE_TARBALL_SHA256=2a291f0a9555f5d6685d96ce9429da3d0ea3cd896c012702c6ee0015f818684e
P3_NODE_BINARY_SHA256=1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931

P3_CODEX_BINARY=/usr/local/Caskroom/codex/0.159.0/bin/codex
P3_CODEX_VERSION="codex-cli 0.159.0"
P3_CODEX_SHA256=1ad71e5ed117114f9d04cdd8d5dd411515b5ab7ebc725b8ca2f484695d71c838

# Ports the slice must never bind or target.
P3_FORBIDDEN_PORTS="6767 6768"

p3_fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

p3_sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

# Prints the Node 22.20.0 bin directory after verifying the binary digest.
p3_node_bin_dir() {
  p3_node_dir=${NVM_DIR:-"$HOME/.nvm"}/versions/node/v$P3_NODE_VERSION/bin
  [ -x "$p3_node_dir/node" ] || p3_fail "missing Node $P3_NODE_VERSION at $p3_node_dir/node; install with: nvm install $P3_NODE_VERSION"
  [ "$(p3_sha256 "$p3_node_dir/node")" = "$P3_NODE_BINARY_SHA256" ] ||
    p3_fail "Node $P3_NODE_VERSION binary digest mismatch at $p3_node_dir/node"
  [ "$("$p3_node_dir/node" --version)" = "v$P3_NODE_VERSION" ] ||
    p3_fail "Node at $p3_node_dir/node does not report v$P3_NODE_VERSION"
  printf '%s\n' "$p3_node_dir"
}

# Verifies the pinned codex binary and prints its path.
p3_codex_binary() {
  [ -x "$P3_CODEX_BINARY" ] || p3_fail "missing pinned codex at $P3_CODEX_BINARY"
  [ "$(p3_sha256 "$P3_CODEX_BINARY")" = "$P3_CODEX_SHA256" ] ||
    p3_fail "codex binary digest mismatch at $P3_CODEX_BINARY"
  [ "$("$P3_CODEX_BINARY" --version)" = "$P3_CODEX_VERSION" ] ||
    p3_fail "codex at $P3_CODEX_BINARY does not report $P3_CODEX_VERSION"
  printf '%s\n' "$P3_CODEX_BINARY"
}

# Prints the read-only pinned Paseo runtime checkout after verifying HEAD and
# a clean tracked tree. Defaults to .baselines/paseo-runtime of the main checkout
# that owns this worktree's git common directory.
p3_paseo_baseline() {
  if [ -n "${SPOCKY_PASEO_RUNTIME_BASELINE:-}" ]; then
    p3_baseline=$SPOCKY_PASEO_RUNTIME_BASELINE
  else
    p3_common=$(git -C "$1" rev-parse --path-format=absolute --git-common-dir)
    p3_baseline=$(dirname "$p3_common")/.baselines/paseo-runtime
  fi
  [ -d "$p3_baseline" ] || p3_fail "missing pinned Paseo runtime baseline: $p3_baseline"
  [ "$(git -C "$p3_baseline" rev-parse HEAD)" = "$P3_PASEO_COMMIT" ] ||
    p3_fail "Paseo runtime baseline HEAD is not $P3_PASEO_COMMIT: $p3_baseline"
  [ -z "$(git -C "$p3_baseline" status --porcelain --untracked-files=no)" ] ||
    p3_fail "Paseo runtime baseline tracked tree is dirty: $p3_baseline"
  printf '%s\n' "$p3_baseline"
}

# Rejects any port the slice must never use.
p3_check_port() {
  case "$1" in
    '' | *[!0-9]*) p3_fail "port is not numeric: $1" ;;
  esac
  for p3_forbidden in $P3_FORBIDDEN_PORTS; do
    [ "$1" != "$p3_forbidden" ] || p3_fail "refusing forbidden port $1"
  done
}

# Prints a free loopback port, never a forbidden one.
p3_free_port() {
  while :; do
    p3_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
    case " $P3_FORBIDDEN_PORTS " in
      *" $p3_port "*) continue ;;
    esac
    printf '%s\n' "$p3_port"
    return 0
  done
}
