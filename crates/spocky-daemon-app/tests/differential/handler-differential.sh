#!/bin/sh
# Read-handler wire differential: the agent_update harness with a recorder
# that asks fetch_agents, fetch_agent, fetch_agent_timeline and
# wait_for_finish about a finished agent and a missing one. The response
# frames must be byte-identical between the pinned daemon and spocky-daemon
# after masking, with the same persisted-record, stub and egress checks.
#
# Usage: handler-differential.sh <out-dir>
here=$(cd "$(dirname "$0")" && pwd)
SPOCKY_AU_SUBSCRIBER=$here/handler-subscriber.mjs exec "$here/agent-update-differential.sh" "$@"
