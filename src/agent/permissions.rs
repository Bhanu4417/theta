//! Interactive permission handling for the local agent loop.
//!
//! When a tool is gated as `Ask`, the loop emits `PermissionAsked` and awaits a
//! decision here; the UI answers through the manager. This is the local
//! equivalent of the OpenCode permission flow.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use super::tools::PermissionDecision;

#[derive(Default)]
pub struct Broker {
    pending: Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
}

impl Broker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request, returning the receiver the loop awaits on.
    pub fn register(&self, id: String) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        rx
    }

    /// Answer a pending request. Returns false if it is unknown/expired.
    pub fn reply(&self, id: &str, decision: PermissionDecision) -> bool {
        match self.pending.lock().unwrap().remove(id) {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

/// Map a UI permission response (`once`/`always`/`reject`) to a decision.
pub fn decision_for(response: &str) -> PermissionDecision {
    match response {
        "reject" => PermissionDecision::Deny,
        _ => PermissionDecision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broker_round_trips_a_decision() {
        let b = Broker::new();
        let rx = b.register("p1".into());
        assert_eq!(b.pending_count(), 1);
        assert!(b.reply("p1", PermissionDecision::Allow));
        assert_eq!(rx.await.unwrap(), PermissionDecision::Allow);
        assert_eq!(b.pending_count(), 0);
        // Unknown ids are rejected.
        assert!(!b.reply("missing", PermissionDecision::Deny));
    }

    #[test]
    fn responses_map_to_decisions() {
        assert_eq!(decision_for("reject"), PermissionDecision::Deny);
        assert_eq!(decision_for("once"), PermissionDecision::Allow);
        assert_eq!(decision_for("always"), PermissionDecision::Allow);
    }
}
