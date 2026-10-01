# Session gap: cross-agent same-tick event interleave

Status: OPEN, tracked. Not an accepted divergence.

## Gap

When two agents' sessions emit events within one synchronous turn (one
event-loop tick), the agent manager publishes their stream events and state
changes in a different order from the pinned Paseo build. Run results,
timelines and stored records match; only the cross-agent order of the
subscriber feed differs.

- Scope: `spocky-session` agent manager, the session event pipeline
  (`agent_manager/events.rs`, per-agent session event queues).
- Closing condition: the cross-agent same-tick differential (below) matches
  node.
- Blocks: the gates for opencode and for any plugin provider that
  multiplexes several sessions on one event stream. Reopen when that provider
  lane starts; decide then between an exact port and a narrower
  deterministic per-chunk dispatch.
- Does not block: codex and G1 (see Reachability).

## Reproduction

Scenario `interleave` of `crates/spocky-session/tests/agent_manager_differential.rs`,
kept as `evidence/phase3/session-interleave-gap.patch` because it fails while
the gap is open. Rerun with `scripts/phase3/session-interleave-gap.sh` (exit 0
once the orders match).

Two agents (A, B) on one fake provider each start a held turn (`turn_started`
only). Once both turns have started, one synchronous loop, with no await in
between, emits to A and B:

1. A `timeline` assistant `a1`
2. B `timeline` assistant `b1`
3. A `timeline` tool call (running)
4. B `usage_updated`
5. B `timeline` reasoning `b2`
6. A `turn_completed`
7. B `timeline` assistant `b3`
8. B `turn_completed`

Feed order, node v22.20.0 against the pinned dist and Rust at `383d381`
(3 of 3 runs, deterministic on both sides):

| # | node (pinned) | rust |
|---|---|---|
| 9 | A timeline a1 | A timeline a1 |
| 10 | B timeline b1 | A timeline tool_call |
| 11 | B state running | A state idle |
| 12 | B usage_updated | A turn_completed |
| 13 | A timeline tool_call | B timeline b1 |
| 14 | A state idle | B state running |
| 15 | A turn_completed | B usage_updated |
| 16 | B timeline b2 | B timeline b2 |
| 17 | B timeline b3 | B timeline b3 |

Rows 1 to 8 (create, idle, turn start) and 18 onward are identical.

## Cause

The baseline queues each agent's session events on its own promise chain
(`enqueueSessionEvent`: `tail.catch(() => undefined).then(async () => await
dispatchSessionEvent(...))`). `dispatchSessionEvent`, `handleStreamEvent` and the
stream coalescer's flush cross await points, so two agents' chains interleave
at microtask granularity, with a hop count per event that depends on the code
path taken. The Rust manager drains each agent's queue in its own task and
handles an event synchronously, so a whole agent's queued events go first.

## Reachability

- codex: unreachable. `CodexAppServerAgent.establishConnection` spawns one
  `codex app-server` child per session, so each agent's events arrive on its
  own pipe, in separate macrotasks; microtasks drain between them.
  Interleaving across macrotasks while one event is being handled depends on
  real I/O (`dispatchSessionEvent` awaits `registry.get`), which is
  timing-dependent in node as well.
- opencode: reachable. One shared event stream carries every session's events
  and is filtered by `sessionID` (`opencode-agent.ts` around line 2318), so one
  chunk can deliver two sessions' events in the same tick.
- Plugin providers that multiplex sessions: reachable for the same reason.
