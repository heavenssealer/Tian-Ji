//! The ApprovalGate — how an async agent turn parks waiting for a human click without blocking
//! the runtime (DESIGN.md §9.3).
//!
//! On `NeedsApproval`: mint a token, create a `oneshot`, store `token → (Sender, ProposedCall)`,
//! emit the request to the UI, and `.await` the receiver. `policy_resolve` looks the token up
//! and sends the outcome, waking the parked turn. The stored call lets "always allow" build a
//! rule from the exact command.

use std::collections::HashMap;
use std::sync::Mutex;

use tianji_policy::AllowRule;
use tianji_types::uuid::Uuid; // funnelled through tianji-types' re-export
use tianji_types::{Classification, Target};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalToken(pub Uuid);

impl ApprovalToken {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ApprovalToken {
    fn default() -> Self {
        Self::new()
    }
}

/// What the UI is being asked to approve. Mirrors the approval card.
#[derive(Debug, Clone)]
pub struct ProposedCall {
    pub tool: String,
    pub argv: Vec<String>,
    pub targets: Vec<Target>,
    pub classification: Classification,
}

/// The human's decision, sent back through the gate.
#[derive(Debug, Clone)]
pub enum ApprovalOutcome {
    ApproveOnce,
    ApproveEdited(Vec<String>),
    Deny(String),
    AlwaysAllow(AllowRule),
}

#[derive(Default)]
pub struct ApprovalGate {
    pending: Mutex<HashMap<ApprovalToken, (oneshot::Sender<ApprovalOutcome>, ProposedCall)>>,
}

impl ApprovalGate {
    /// Register a pending approval and return the token + the receiver the turn awaits.
    pub fn open(&self, call: ProposedCall) -> (ApprovalToken, oneshot::Receiver<ApprovalOutcome>) {
        let token = ApprovalToken::new();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(token, (tx, call));
        (token, rx)
    }

    /// Peek the proposed call for a token (used to build an "always allow" rule).
    pub fn call_for(&self, token: ApprovalToken) -> Option<ProposedCall> {
        self.pending.lock().unwrap().get(&token).map(|(_, c)| c.clone())
    }

    /// Called by the Tauri `policy_resolve` command when the user acts on the card.
    pub fn resolve(&self, token: ApprovalToken, outcome: ApprovalOutcome) {
        if let Some((tx, _)) = self.pending.lock().unwrap().remove(&token) {
            let _ = tx.send(outcome); // receiver gone = turn already aborted; ignore.
        }
    }
}
