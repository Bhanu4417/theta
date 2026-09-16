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

    pub fn register(&self, id: String) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        rx
    }

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

pub fn decision_for(response: &str) -> PermissionDecision {
    match response {
        "reject" => PermissionDecision::Deny,
        _ => PermissionDecision::Allow,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAnswer {
    Answered(Vec<Vec<String>>),
    Rejected,
}

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
        assert!(!b.reply("missing", PermissionDecision::Deny));
    }

    #[tokio::test]
    async fn question_broker_round_trips_answers() {
        let b = QuestionBroker::new();
        let rx = b.register("q1".into());
        assert!(b.reply("q1", vec![vec!["Yes".into()]]));
        assert_eq!(rx.await.unwrap(), QuestionAnswer::Answered(vec![vec!["Yes".into()]]));
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
