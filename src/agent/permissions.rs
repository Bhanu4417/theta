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

/// The answer to a pending `ask` question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAnswer {
    Answered(Vec<Vec<String>>),
    Rejected,
}

/// Tracks `ask`-tool questions the loop is waiting on. The UI answers through
/// the manager, mirroring the permission broker.
#[derive(Default)]
pub struct QuestionBroker {
    pending: Mutex<HashMap<String, oneshot::Sender<QuestionAnswer>>>,
}

impl QuestionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, id: String) -> oneshot::Receiver<QuestionAnswer> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        rx
    }

    pub fn reply(&self, id: &str, answers: Vec<Vec<String>>) -> bool {
        match self.pending.lock().unwrap().remove(id) {
            Some(tx) => tx.send(QuestionAnswer::Answered(answers)).is_ok(),
            None => false,
        }
    }

    pub fn reject(&self, id: &str) -> bool {
        match self.pending.lock().unwrap().remove(id) {
            Some(tx) => tx.send(QuestionAnswer::Rejected).is_ok(),
            None => false,
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
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

    #[tokio::test]
    async fn question_broker_round_trips_answers() {
        let b = QuestionBroker::new();
        let rx = b.register("q1".into());
        assert!(b.reply("q1", vec![vec!["Yes".into()]]));
        assert_eq!(rx.await.unwrap(), QuestionAnswer::Answered(vec![vec!["Yes".into()]]));
        // Rejection and unknown ids.
        let rx = b.register("q2".into());
        assert!(b.reject("q2"));
        assert_eq!(rx.await.unwrap(), QuestionAnswer::Rejected);
        assert!(!b.reply("nope", vec![]));
    }

    #[test]
    fn responses_map_to_decisions() {
        assert_eq!(decision_for("reject"), PermissionDecision::Deny);
        assert_eq!(decision_for("once"), PermissionDecision::Allow);
        assert_eq!(decision_for("always"), PermissionDecision::Allow);
    }
}
