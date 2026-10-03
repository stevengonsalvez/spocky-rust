//! `getMetricsSnapshot` from pinned Paseo `agent/agent-manager.ts`.

use super::AgentManager;

/// `AgentMetricsSnapshot`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentMetricsSnapshot {
    pub total: usize,
    pub subscription_count: usize,
    /// `byLifecycle`, in the order each lifecycle was first seen.
    pub by_lifecycle: Vec<(String, usize)>,
    pub with_active_foreground_turn: usize,
    /// `timelineStats.totalItems`.
    pub timeline_total_items: usize,
    /// `timelineStats.maxItemsPerAgent`.
    pub timeline_max_items_per_agent: usize,
}

impl AgentManager {
    /// `getMetricsSnapshot()`.
    #[must_use]
    pub fn metrics_snapshot(&self) -> AgentMetricsSnapshot {
        let state = self.lock();
        let mut metrics = AgentMetricsSnapshot {
            total: state.agents.len(),
            subscription_count: state.subscribers.len(),
            ..AgentMetricsSnapshot::default()
        };
        for (id, agent) in &state.agents {
            let lifecycle = agent.snapshot.lifecycle.as_str();
            match metrics
                .by_lifecycle
                .iter_mut()
                .find(|(name, _)| name == lifecycle)
            {
                Some((_, count)) => *count += 1,
                None => metrics.by_lifecycle.push((lifecycle.to_owned(), 1)),
            }
            if agent.snapshot.active_foreground_turn_id.is_some() {
                metrics.with_active_foreground_turn += 1;
            }
            let Ok(rows) = state.timeline.rows(id) else {
                continue;
            };
            metrics.timeline_total_items += rows.len();
            metrics.timeline_max_items_per_agent =
                metrics.timeline_max_items_per_agent.max(rows.len());
        }
        metrics
    }
}
