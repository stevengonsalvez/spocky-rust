//! The agent half of the daemon's graceful stop, as `bootstrap.ts` `stop()`
//! runs it between `wsServer.prepareForShutdown()` and `wsServer.close()`:
//! freeze agent registration, close every agent (each bounded by
//! `AGENT_CLOSE_TIMEOUT_MS`), flush the manager's tasks, then flush agent
//! storage. A closed agent's record is persisted with `lastStatus: "closed"`.
//!
//! Provider runtime, plugin, terminal, speech, schedule and relay shutdown
//! have no counterpart in this daemon.

use std::future::Future;
use std::task::Poll;
use std::time::Duration;

use crate::session::Services;

/// `AGENT_CLOSE_TIMEOUT_MS`: a provider that never answers a close must not
/// hold the daemon open.
const AGENT_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// `closeAllAgents(logger, agentManager)`: every listed agent closes
/// concurrently; a failed or timed-out close is logged and abandoned.
async fn close_all_agents(services: &Services) {
    let closes = services.manager.list_agents().into_iter().map(|agent| {
        let manager = &services.manager;
        async move {
            let outcome =
                tokio::time::timeout(AGENT_CLOSE_TIMEOUT, manager.close_agent(&agent.id)).await;
            let error = match outcome {
                Ok(Ok(())) => return,
                Ok(Err(error)) => error.message,
                Err(_) => format!(
                    "close agent {} timed out after {}ms",
                    agent.id,
                    AGENT_CLOSE_TIMEOUT.as_millis()
                ),
            };
            eprintln!("Failed to close agent {}: {error}", agent.id);
        }
    });
    join_all(closes).await;
}

/// `Promise.all` over futures that borrow the caller, polled together on
/// this task.
async fn join_all<F: Future<Output = ()>>(futures: impl Iterator<Item = F>) {
    let mut pending: Vec<_> = futures.map(Box::pin).collect();
    std::future::poll_fn(|cx| {
        pending.retain_mut(|future| future.as_mut().poll(cx).is_pending());
        if pending.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}

/// The agent steps of `stop()`: `agentManager.prepareForShutdown()`,
/// `closeAllAgents`, `agentManager.flushForShutdown()`, then
/// `agentStorage.flush()`.
pub async fn stop_agents(services: &Services) {
    services.manager.prepare_for_shutdown();
    close_all_agents(services).await;
    services.manager.flush_for_shutdown().await;
    services.storage.flush().await;
}
