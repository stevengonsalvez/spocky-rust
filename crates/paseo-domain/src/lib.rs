use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentId(String);

impl AgentId {
    /// Creates a nonempty agent identifier.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyIdentifier`] for an empty identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyIdentifier);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequestId(String);

impl PermissionRequestId {
    /// Creates a nonempty permission request identifier.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::EmptyIdentifier`] for an empty identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::EmptyIdentifier);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLifecycle {
    Initializing,
    Idle,
    Running,
    Error,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStateBucket {
    NeedsInput,
    Failed,
    Running,
    Attention,
    Done,
}

impl AgentStateBucket {
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::NeedsInput => 0,
            Self::Failed => 1,
            Self::Running => 2,
            Self::Attention => 3,
            Self::Done => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionReason {
    Finished,
    Error,
    Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationOutcome {
    Settled,
    AlreadySettled,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    Archived,
    InvalidTransition,
    PermissionRequestNotFound,
    CancellationRefused { agent_id: String },
    EmptyIdentifier,
}

impl Display for DomainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Archived => "agent is archived",
            Self::InvalidTransition => "invalid agent lifecycle transition",
            Self::PermissionRequestNotFound => "permission request not found",
            Self::CancellationRefused { agent_id } => {
                return write!(
                    formatter,
                    "Cannot stop agent {agent_id} because its active run cancellation was not acknowledged"
                );
            }
            Self::EmptyIdentifier => "identifier must not be empty",
        };
        formatter.write_str(message)
    }
}

impl Error for DomainError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLifecycleMachine {
    id: AgentId,
    lifecycle: AgentLifecycle,
    pending_permission: Option<PermissionRequestId>,
    requires_attention: bool,
    attention_reason: Option<AttentionReason>,
    last_error: Option<String>,
    archived: bool,
}

impl AgentLifecycleMachine {
    #[must_use]
    pub fn create(id: AgentId) -> Self {
        Self {
            id,
            lifecycle: AgentLifecycle::Initializing,
            pending_permission: None,
            requires_attention: false,
            attention_reason: None,
            last_error: None,
            archived: false,
        }
    }

    #[must_use]
    pub const fn lifecycle(&self) -> AgentLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn is_archived(&self) -> bool {
        self.archived
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[must_use]
    pub const fn attention_reason(&self) -> Option<AttentionReason> {
        self.attention_reason
    }

    #[must_use]
    pub const fn bucket(&self) -> AgentStateBucket {
        if self.pending_permission.is_some()
            || matches!(self.attention_reason, Some(AttentionReason::Permission))
        {
            AgentStateBucket::NeedsInput
        } else if matches!(self.lifecycle, AgentLifecycle::Error)
            || matches!(self.attention_reason, Some(AttentionReason::Error))
        {
            AgentStateBucket::Failed
        } else if matches!(self.lifecycle, AgentLifecycle::Running) {
            AgentStateBucket::Running
        } else if self.requires_attention {
            AgentStateBucket::Attention
        } else {
            AgentStateBucket::Done
        }
    }

    /// Completes agent initialization.
    ///
    /// # Errors
    ///
    /// Returns an error unless the agent is initializing and not archived.
    pub fn initialization_succeeded(&mut self) -> Result<(), DomainError> {
        self.require_live_state(AgentLifecycle::Initializing)?;
        self.lifecycle = AgentLifecycle::Idle;
        self.last_error = None;
        Ok(())
    }

    /// Records a failed resume while preserving a recoverable closed record.
    ///
    /// # Errors
    ///
    /// Returns an error unless the agent is initializing and not archived.
    pub fn initialization_failed(&mut self, message: impl Into<String>) -> Result<(), DomainError> {
        self.require_live_state(AgentLifecycle::Initializing)?;
        self.lifecycle = AgentLifecycle::Closed;
        self.last_error = Some(message.into());
        Ok(())
    }

    /// Starts a turn from idle.
    ///
    /// # Errors
    ///
    /// Returns an error for archived agents or non-idle states.
    pub fn send(&mut self) -> Result<(), DomainError> {
        self.require_live_state(AgentLifecycle::Idle)?;
        self.lifecycle = AgentLifecycle::Running;
        self.requires_attention = false;
        self.attention_reason = None;
        Ok(())
    }

    /// Records a pending permission request during a running turn.
    ///
    /// # Errors
    ///
    /// Returns an error outside a running turn or when another request is pending.
    pub fn request_permission(&mut self, id: PermissionRequestId) -> Result<(), DomainError> {
        self.require_live_state(AgentLifecycle::Running)?;
        if self.pending_permission.is_some() {
            return Err(DomainError::InvalidTransition);
        }
        self.pending_permission = Some(id);
        self.attention_reason = Some(AttentionReason::Permission);
        self.requires_attention = true;
        Ok(())
    }

    /// Resolves the matching pending permission request.
    ///
    /// # Errors
    ///
    /// Returns an error if no matching request is pending.
    pub fn respond_to_permission(
        &mut self,
        id: &PermissionRequestId,
        _decision: PermissionDecision,
    ) -> Result<(), DomainError> {
        if self.pending_permission.as_ref() != Some(id) {
            return Err(DomainError::PermissionRequestNotFound);
        }
        self.pending_permission = None;
        self.attention_reason = None;
        self.requires_attention = false;
        Ok(())
    }

    /// Applies provider cancellation outcome semantics.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider refuses cancellation or the agent is archived.
    pub fn cancel(&mut self, outcome: CancellationOutcome) -> Result<bool, DomainError> {
        if self.archived {
            return Err(DomainError::Archived);
        }
        if self.lifecycle != AgentLifecycle::Running {
            return Ok(false);
        }
        match outcome {
            CancellationOutcome::Settled => {
                self.finish_cancellation();
                Ok(true)
            }
            CancellationOutcome::AlreadySettled => {
                self.finish_cancellation();
                Ok(false)
            }
            CancellationOutcome::Refused => Err(DomainError::CancellationRefused {
                agent_id: self.id.0.clone(),
            }),
        }
    }

    /// Closes a live record for daemon restart.
    ///
    /// # Errors
    ///
    /// Returns an error when already closed or archived.
    pub fn close_for_restart(&mut self) -> Result<(), DomainError> {
        if self.archived {
            return Err(DomainError::Archived);
        }
        if self.lifecycle == AgentLifecycle::Closed {
            return Err(DomainError::InvalidTransition);
        }
        self.lifecycle = AgentLifecycle::Closed;
        self.pending_permission = None;
        self.attention_reason = None;
        self.requires_attention = false;
        Ok(())
    }

    /// Starts resumption from a closed stored record.
    ///
    /// # Errors
    ///
    /// Returns an error unless the record is closed and not archived.
    pub fn resume(&mut self) -> Result<(), DomainError> {
        self.require_live_state(AgentLifecycle::Closed)?;
        self.lifecycle = AgentLifecycle::Initializing;
        Ok(())
    }

    /// Archives the agent and normalizes it to closed.
    ///
    /// # Errors
    ///
    /// Returns an error when already archived.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        if self.archived {
            return Err(DomainError::Archived);
        }
        self.archived = true;
        self.lifecycle = AgentLifecycle::Closed;
        self.pending_permission = None;
        self.attention_reason = None;
        self.requires_attention = false;
        Ok(())
    }

    fn require_live_state(&self, state: AgentLifecycle) -> Result<(), DomainError> {
        if self.archived {
            return Err(DomainError::Archived);
        }
        if self.lifecycle != state {
            return Err(DomainError::InvalidTransition);
        }
        Ok(())
    }

    fn finish_cancellation(&mut self) {
        self.lifecycle = AgentLifecycle::Idle;
        self.pending_permission = None;
        self.attention_reason = None;
        self.requires_attention = false;
    }
}
