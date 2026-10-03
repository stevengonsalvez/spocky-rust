#!/bin/sh
# Unarchive wire differential: a send to an archived agent. The pinned daemon
# creates an agent and archives it (the seed); each side then starts from a
# copy of that home, and a client sends the archived agent a message. The raw
# wire frames, in arrival order, and the stored record the send leaves must be
# byte-identical between the pinned daemon and spocky-daemon after masking,
# with the same stub and egress checks. One class of pinned nondeterminism is
# allowed, only here and only for the send reply's place among the turn's last
# two updates (see unarchive-compare.mjs).
#
# Usage: unarchive-differential.sh <out-dir>
here=$(cd "$(dirname "$0")" && pwd)
SPOCKY_AU_SEED_SUBSCRIBER=$here/unarchive-seed-subscriber.mjs \
SPOCKY_AU_RUN_STUB=$here/unarchive-run-stub.json \
SPOCKY_AU_SUBSCRIBER=$here/unarchive-subscriber.mjs \
SPOCKY_AU_COMPARE=$here/unarchive-compare.mjs \
  exec "$here/agent-update-differential.sh" "$@"
