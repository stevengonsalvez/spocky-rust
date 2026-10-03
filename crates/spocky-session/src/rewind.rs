//! Pinned Paseo `agent/rewind/rewind.ts`: which rewind a provider session
//! can do, and the call that does it.

use spocky_contracts::js::truthy;

use crate::agent_sdk::{AgentError, AgentSession};

/// `RewindMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindMode {
    Conversation,
    Files,
    Both,
}

impl RewindMode {
    /// The mode as the original spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Files => "files",
            Self::Both => "both",
        }
    }
}

/// `RewindCapabilityError`.
fn capability_error(mode: RewindMode) -> AgentError {
    AgentError::named(
        "RewindCapabilityError".to_owned(),
        format!("Provider does not support rewinding {}", mode.as_str()),
    )
}

/// `invokeRewindCapability(session, { messageId, mode })`.
///
/// # Errors
///
/// `RewindCapabilityError` when the session's capabilities or methods do not
/// cover `mode`, or the session's own error.
pub async fn invoke_rewind_capability(
    session: &dyn AgentSession,
    message_id: &str,
    mode: RewindMode,
) -> Result<(), AgentError> {
    let flag = match mode {
        RewindMode::Conversation => "supportsRewindConversation",
        RewindMode::Files => "supportsRewindFiles",
        RewindMode::Both => "supportsRewindBoth",
    };
    // The original reads the flag before it looks for the method, and calls
    // the method only when both hold.
    if !truthy(session.capabilities().get(flag)) {
        return Err(capability_error(mode));
    }
    let revert = match mode {
        RewindMode::Conversation => session.revert_conversation(message_id),
        RewindMode::Files => session.revert_files(message_id),
        RewindMode::Both => session.revert_both(message_id),
    };
    match revert {
        Some(revert) => revert.await,
        None => Err(capability_error(mode)),
    }
}
