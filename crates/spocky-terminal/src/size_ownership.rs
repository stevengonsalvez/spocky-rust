//! One daemon-owned size claimant per terminal, following pinned
//! `packages/server/src/terminal/terminal-size-ownership.ts`.
//!
//! A `claim` transfers ownership to its connection even when the size is
//! unchanged; an `update` applies only from the current owner. The baseline
//! keeps the owner in a `WeakRef`, so a dropped connection stops owning the
//! terminal; [`SizeOwnership`] keeps a [`Weak`] for the same reason.

use std::sync::{Arc, Weak};

/// The `intent` of a resize request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeIntent {
    Claim,
    Update,
}

/// A resize request from one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeRequest {
    pub rows: u16,
    pub cols: u16,
    /// `None` is a client older than v0.2.6, treated as a claim.
    pub intent: Option<SizeIntent>,
}

/// The terminal whose size the owner controls.
pub trait SizeTarget {
    /// Current `(rows, cols)`.
    fn size(&self) -> (u16, u16);
    /// Sends `{ type: "resize", rows, cols }` to the terminal.
    fn resize(&mut self, rows: u16, cols: u16);
}

/// The current size claimant of one terminal.
#[derive(Debug)]
pub struct SizeOwnership<O> {
    owner: Option<Weak<O>>,
}

impl<O> Default for SizeOwnership<O> {
    fn default() -> Self {
        Self { owner: None }
    }
}

impl<O> SizeOwnership<O> {
    /// `applyTerminalSize`: returns whether the request was accepted.
    pub fn apply(
        &mut self,
        terminal: &mut impl SizeTarget,
        owner: &Arc<O>,
        request: SizeRequest,
    ) -> bool {
        // COMPAT(terminalSizeOwnership): a missing intent is a claim until the
        // client floor sends resize intent (baseline removal after 2027-02-02).
        let intent = request.intent.unwrap_or(SizeIntent::Claim);
        if intent == SizeIntent::Update && !self.is_owner(owner) {
            return false;
        }
        if intent == SizeIntent::Claim {
            self.owner = Some(Arc::downgrade(owner));
        }
        if terminal.size() != (request.rows, request.cols) {
            terminal.resize(request.rows, request.cols);
        }
        true
    }

    fn is_owner(&self, owner: &Arc<O>) -> bool {
        self.owner
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|current| Arc::ptr_eq(&current, owner))
    }
}
