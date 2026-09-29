//! Deterministic in-memory pilot for distributed relay contracts.
//!
//! This crate exercises ownership, failure, flow-control, and opacity decisions.
//! It is not a production network relay and makes no parity claim.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id!(NodeId);
string_id!(SessionId);
string_id!(LinkId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClusterConfig {
    pub minimum_cluster_size: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeConfig {
    pub max_links: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkConfig {
    pub max_queued_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerToken {
    session: SessionId,
    pub node: NodeId,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimDecision {
    Local(OwnerToken),
    Reroute { target: NodeId, owner: OwnerToken },
    Unowned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseCode {
    SessionOwnerMoved,
    TryAgainLater,
}

impl CloseCode {
    #[must_use]
    pub const fn websocket_code(self) -> u16 {
        match self {
            Self::SessionOwnerMoved => 1012,
            Self::TryAgainLater => 1013,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShedReason {
    SlowConsumer,
    MemoryPressure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseEvent {
    pub link: LinkId,
    pub code: CloseCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShedEvent {
    pub link: LinkId,
    pub reason: ShedReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayError {
    NodeUnavailable,
    ClusterUnready,
    Draining,
    Capacity,
    StaleOwner,
    LinkExists,
    LinkNotFound,
    Shed(ShedReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueCiphertext(Vec<u8>);

impl OpaqueCiphertext {
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn wire_bytes(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameObservation {
    pub link: LinkId,
    pub sequence: u64,
    pub wire_bytes: usize,
}

impl FrameObservation {
    /// Observations deliberately contain metadata only.
    #[must_use]
    pub const fn exposes_payload_bytes(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug)]
struct NodeState {
    config: NodeConfig,
    live: bool,
    draining: bool,
}

#[derive(Clone, Debug)]
struct LinkState {
    owner: OwnerToken,
    config: LinkConfig,
    queue: VecDeque<OpaqueCiphertext>,
    queued_bytes: usize,
    admitted_order: u64,
    next_sequence: u64,
}

#[derive(Debug)]
pub struct RelayPilot {
    config: ClusterConfig,
    nodes: BTreeMap<NodeId, NodeState>,
    owners: BTreeMap<SessionId, OwnerToken>,
    generations: BTreeMap<SessionId, u64>,
    links: BTreeMap<LinkId, LinkState>,
    next_admission_order: u64,
    observations: Vec<FrameObservation>,
    closes: Vec<CloseEvent>,
}

impl RelayPilot {
    #[must_use]
    pub fn new(config: ClusterConfig) -> Self {
        Self {
            config,
            nodes: BTreeMap::new(),
            owners: BTreeMap::new(),
            generations: BTreeMap::new(),
            links: BTreeMap::new(),
            next_admission_order: 0,
            observations: Vec::new(),
            closes: Vec::new(),
        }
    }

    pub fn add_node(&mut self, node: NodeId, config: NodeConfig) {
        self.nodes.insert(
            node,
            NodeState {
                config,
                live: true,
                draining: false,
            },
        );
    }

    #[must_use]
    pub fn is_live(&self, node: &NodeId) -> bool {
        self.nodes.get(node).is_some_and(|state| state.live)
    }

    #[must_use]
    pub fn is_ready(&self, node: &NodeId) -> bool {
        let Some(state) = self.nodes.get(node) else {
            return false;
        };
        state.live
            && !state.draining
            && self.live_node_count() >= self.config.minimum_cluster_size
            && self.active_links(node) < state.config.max_links
    }

    /// Converges simultaneous candidates onto the lexicographically first
    /// eligible node and allocates a monotonic per-session generation.
    ///
    /// # Errors
    ///
    /// Returns the applicable readiness, drain, capacity, or node error when
    /// none of the proposed nodes may own new work.
    pub fn converge_claims<I>(
        &mut self,
        session: SessionId,
        candidates: I,
    ) -> Result<OwnerToken, RelayError>
    where
        I: IntoIterator<Item = NodeId>,
    {
        if let Some(owner) = self.owners.get(&session)
            && self.is_live(&owner.node)
        {
            return Ok(owner.clone());
        }

        let candidates: BTreeSet<NodeId> = candidates.into_iter().collect();
        let mut saw_node = false;
        let mut saw_draining = false;
        let mut saw_capacity = false;
        let mut saw_cluster_unready = false;

        let winner = candidates.iter().find(|node| {
            let Some(state) = self.nodes.get(*node) else {
                return false;
            };
            if !state.live {
                return false;
            }
            saw_node = true;
            if state.draining {
                saw_draining = true;
                return false;
            }
            if self.live_node_count() < self.config.minimum_cluster_size {
                saw_cluster_unready = true;
                return false;
            }
            if self.active_links(node) >= state.config.max_links {
                saw_capacity = true;
                return false;
            }
            true
        });

        let winner = match winner {
            Some(winner) => winner.clone(),
            None if saw_draining => return Err(RelayError::Draining),
            None if saw_cluster_unready => return Err(RelayError::ClusterUnready),
            None if saw_capacity => return Err(RelayError::Capacity),
            None if saw_node => return Err(RelayError::NodeUnavailable),
            None => return Err(RelayError::NodeUnavailable),
        };

        let generation = self.generations.entry(session.clone()).or_default();
        *generation = generation.saturating_add(1);
        let owner = OwnerToken {
            session: session.clone(),
            node: winner,
            generation: *generation,
        };
        self.owners.insert(session, owner.clone());
        Ok(owner)
    }

    #[must_use]
    pub fn route(&self, session: &SessionId, landing: &NodeId) -> ClaimDecision {
        let Some(owner) = self.owners.get(session) else {
            return ClaimDecision::Unowned;
        };
        if &owner.node == landing {
            ClaimDecision::Local(owner.clone())
        } else {
            ClaimDecision::Reroute {
                target: owner.node.clone(),
                owner: owner.clone(),
            }
        }
    }

    /// Admits a link against the current owner generation.
    ///
    /// # Errors
    ///
    /// Returns an error for stale ownership, unavailable nodes, duplicate links,
    /// or exhausted node capacity.
    pub fn open_link(
        &mut self,
        link: LinkId,
        session: &SessionId,
        owner: OwnerToken,
        config: LinkConfig,
    ) -> Result<(), RelayError> {
        self.validate_owner(session, &owner)?;
        if self.links.contains_key(&link) {
            return Err(RelayError::LinkExists);
        }
        let node = self
            .nodes
            .get(&owner.node)
            .filter(|node| node.live)
            .ok_or(RelayError::NodeUnavailable)?;
        if self.active_links(&owner.node) >= node.config.max_links {
            return Err(RelayError::Capacity);
        }
        self.next_admission_order = self.next_admission_order.saturating_add(1);
        self.links.insert(
            link,
            LinkState {
                owner,
                config,
                queue: VecDeque::new(),
                queued_bytes: 0,
                admitted_order: self.next_admission_order,
                next_sequence: 0,
            },
        );
        Ok(())
    }

    /// Queues one opaque ciphertext frame without inspecting or changing bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for stale ownership, missing links, or slow-consumer
    /// shedding when the per-link byte bound would be exceeded.
    pub fn enqueue(
        &mut self,
        link: &LinkId,
        owner: &OwnerToken,
        ciphertext: OpaqueCiphertext,
    ) -> Result<(), RelayError> {
        self.validate_owner(&owner.session, owner)?;
        let Some(state) = self.links.get_mut(link) else {
            return Err(RelayError::LinkNotFound);
        };
        if &state.owner != owner {
            return Err(RelayError::StaleOwner);
        }
        let wire_bytes = ciphertext.wire_bytes();
        if state.queued_bytes.saturating_add(wire_bytes) > state.config.max_queued_bytes {
            self.remove_link_with_close(link, CloseCode::TryAgainLater);
            return Err(RelayError::Shed(ShedReason::SlowConsumer));
        }
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.queued_bytes += wire_bytes;
        state.queue.push_back(ciphertext);
        self.observations.push(FrameObservation {
            link: link.clone(),
            sequence: state.next_sequence,
            wire_bytes,
        });
        Ok(())
    }

    #[must_use]
    pub fn dequeue(&mut self, link: &LinkId) -> Option<OpaqueCiphertext> {
        let state = self.links.get_mut(link)?;
        let frame = state.queue.pop_front()?;
        state.queued_bytes -= frame.wire_bytes();
        Some(frame)
    }

    #[must_use]
    pub fn queued_bytes(&self, link: &LinkId) -> Option<usize> {
        self.links.get(link).map(|state| state.queued_bytes)
    }

    #[must_use]
    pub fn observations(&self) -> &[FrameObservation] {
        &self.observations
    }

    #[must_use]
    pub fn last_close(&self) -> Option<&CloseEvent> {
        self.closes.last()
    }

    /// Marks a node unavailable, releases its ownership, and closes its links.
    #[must_use]
    pub fn lose_node(&mut self, node: &NodeId) -> Vec<CloseEvent> {
        if let Some(state) = self.nodes.get_mut(node) {
            state.live = false;
        }
        self.owners.retain(|_, owner| &owner.node != node);
        let affected: Vec<LinkId> = self
            .links
            .iter()
            .filter(|(_, state)| &state.owner.node == node)
            .map(|(link, _)| link.clone())
            .collect();
        let mut closed = Vec::with_capacity(affected.len());
        for link in affected {
            closed.push(self.remove_link_with_close(&link, CloseCode::SessionOwnerMoved));
        }
        closed
    }

    /// Sheds one link, preferring the oldest blocked link, then newest idle link.
    #[must_use]
    pub fn shed_one(&mut self, node: &NodeId) -> Option<ShedEvent> {
        let blocked = self
            .links
            .iter()
            .filter(|(_, state)| &state.owner.node == node && state.queued_bytes > 0)
            .min_by_key(|(_, state)| state.admitted_order)
            .map(|(link, _)| link.clone());
        let victim = blocked.or_else(|| {
            self.links
                .iter()
                .filter(|(_, state)| &state.owner.node == node)
                .max_by_key(|(_, state)| state.admitted_order)
                .map(|(link, _)| link.clone())
        })?;
        self.remove_link_with_close(&victim, CloseCode::TryAgainLater);
        Some(ShedEvent {
            link: victim,
            reason: ShedReason::MemoryPressure,
        })
    }

    /// Starts a process-local drain while preserving established links.
    ///
    /// # Errors
    ///
    /// Returns an error when the node is unknown or unavailable.
    pub fn begin_drain(&mut self, node: &NodeId) -> Result<(), RelayError> {
        let state = self
            .nodes
            .get_mut(node)
            .filter(|state| state.live)
            .ok_or(RelayError::NodeUnavailable)?;
        state.draining = true;
        Ok(())
    }

    /// Cancels a process-local drain.
    ///
    /// # Errors
    ///
    /// Returns an error when the node is unknown or unavailable.
    pub fn cancel_drain(&mut self, node: &NodeId) -> Result<(), RelayError> {
        let state = self
            .nodes
            .get_mut(node)
            .filter(|state| state.live)
            .ok_or(RelayError::NodeUnavailable)?;
        state.draining = false;
        Ok(())
    }

    fn validate_owner(&self, session: &SessionId, owner: &OwnerToken) -> Result<(), RelayError> {
        if &owner.session == session && self.owners.get(session) == Some(owner) {
            Ok(())
        } else {
            Err(RelayError::StaleOwner)
        }
    }

    fn live_node_count(&self) -> usize {
        self.nodes.values().filter(|node| node.live).count()
    }

    fn active_links(&self, node: &NodeId) -> usize {
        self.links
            .values()
            .filter(|link| &link.owner.node == node)
            .count()
    }

    fn remove_link_with_close(&mut self, link: &LinkId, code: CloseCode) -> CloseEvent {
        self.links.remove(link);
        let event = CloseEvent {
            link: link.clone(),
            code,
        };
        self.closes.push(event.clone());
        event
    }
}
