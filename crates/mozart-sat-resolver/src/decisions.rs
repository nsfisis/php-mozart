use crate::error::SolverBugError;
use crate::pool::{Literal, PackageId, literal_to_package_id};
use crate::rule_set::RuleId;
use std::collections::HashMap;

/// A decision entry: which literal was decided and which rule caused it.
#[derive(Debug, Clone)]
pub struct Decision {
    pub literal: Literal,
    pub rule_id: RuleId,
}

/// Tracks all decisions (variable assignments) made during solving.
///
/// Port of Composer's Decisions.php.
pub struct Decisions {
    /// Package ID → signed level. Positive = install, negative = uninstall.
    /// The absolute value is the decision level.
    decision_map: HashMap<PackageId, i32>,
    /// Queue of decisions in order.
    decision_queue: Vec<Decision>,
}

impl Decisions {
    pub fn new() -> Self {
        Decisions {
            decision_map: HashMap::new(),
            decision_queue: Vec::new(),
        }
    }

    /// Record a decision.
    pub fn decide(
        &mut self,
        literal: Literal,
        level: i32,
        rule_id: RuleId,
    ) -> Result<(), SolverBugError> {
        let package_id = literal_to_package_id(literal);
        let previous = self.decision_map.get(&package_id).copied().unwrap_or(0);
        if previous != 0 {
            return Err(SolverBugError {
                message: format!(
                    "Trying to decide literal {literal} on level {level}, \
                     even though package {package_id} was previously decided as {previous}."
                ),
            });
        }

        if literal > 0 {
            self.decision_map.insert(package_id, level);
        } else {
            self.decision_map.insert(package_id, -level);
        }

        self.decision_queue.push(Decision { literal, rule_id });
        Ok(())
    }

    /// Check if literal is satisfied (true in current assignment).
    pub fn satisfy(&self, literal: Literal) -> bool {
        let package_id = literal_to_package_id(literal);
        match self.decision_map.get(&package_id) {
            Some(&val) => (literal > 0 && val > 0) || (literal < 0 && val < 0),
            None => false,
        }
    }

    /// Check if literal conflicts with current assignment.
    pub fn conflict(&self, literal: Literal) -> bool {
        let package_id = literal_to_package_id(literal);
        match self.decision_map.get(&package_id) {
            Some(&val) => (val > 0 && literal < 0) || (val < 0 && literal > 0),
            None => false,
        }
    }

    /// Check if package has been decided.
    pub fn decided(&self, literal_or_id: i32) -> bool {
        let package_id = literal_or_id.unsigned_abs();
        self.decision_map.get(&package_id).copied().unwrap_or(0) != 0
    }

    /// Check if package is undecided.
    pub fn undecided(&self, literal_or_id: i32) -> bool {
        !self.decided(literal_or_id)
    }

    /// Check if package is decided for installation.
    pub fn decided_install(&self, literal_or_id: i32) -> bool {
        let package_id = literal_or_id.unsigned_abs();
        self.decision_map.get(&package_id).copied().unwrap_or(0) > 0
    }

    /// Get the decision level for a package (0 if undecided).
    pub fn decision_level(&self, literal_or_id: i32) -> i32 {
        let package_id = literal_or_id.unsigned_abs();
        self.decision_map
            .get(&package_id)
            .copied()
            .unwrap_or(0)
            .abs()
    }

    /// Get the rule ID that caused a decision for a package.
    pub fn decision_rule(&self, literal_or_id: i32) -> Result<RuleId, SolverBugError> {
        let package_id = literal_or_id.unsigned_abs();
        for decision in &self.decision_queue {
            if literal_to_package_id(decision.literal) == package_id {
                return Ok(decision.rule_id);
            }
        }
        Err(SolverBugError {
            message: format!("Did not find a decision rule for {literal_or_id}"),
        })
    }

    /// Get decision at a specific offset in the queue.
    pub fn at_offset(&self, offset: usize) -> &Decision {
        &self.decision_queue[offset]
    }

    /// Check if an offset is valid.
    pub fn valid_offset(&self, offset: usize) -> bool {
        offset < self.decision_queue.len()
    }

    /// Get the rule ID of the last decision.
    pub fn last_reason(&self) -> RuleId {
        self.decision_queue.last().unwrap().rule_id
    }

    /// Get the literal of the last decision.
    pub fn last_literal(&self) -> Literal {
        self.decision_queue.last().unwrap().literal
    }

    /// Clear all decisions.
    pub fn reset(&mut self) {
        while let Some(decision) = self.decision_queue.pop() {
            let pkg_id = literal_to_package_id(decision.literal);
            self.decision_map.insert(pkg_id, 0);
        }
    }

    /// Remove decisions after the given offset (keep offset+1 items).
    pub fn reset_to_offset(&mut self, offset: usize) {
        while self.decision_queue.len() > offset + 1 {
            let decision = self.decision_queue.pop().unwrap();
            let pkg_id = literal_to_package_id(decision.literal);
            self.decision_map.insert(pkg_id, 0);
        }
    }

    /// Remove the last decision.
    pub fn revert_last(&mut self) {
        let decision = self.decision_queue.pop().unwrap();
        let pkg_id = literal_to_package_id(decision.literal);
        self.decision_map.insert(pkg_id, 0);
    }

    /// Number of decisions.
    pub fn len(&self) -> usize {
        self.decision_queue.len()
    }

    /// Whether there are no decisions.
    pub fn is_empty(&self) -> bool {
        self.decision_queue.is_empty()
    }

    /// Iterate decisions in reverse order (newest first).
    /// Used by analyzeUnsolvable in Composer.
    pub fn iter_reverse(&self) -> impl Iterator<Item = &Decision> {
        self.decision_queue.iter().rev()
    }
}

impl Default for Decisions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decide_and_satisfy() {
        let mut d = Decisions::new();
        d.decide(1, 1, 0).unwrap(); // install package 1 at level 1

        assert!(d.satisfy(1));
        assert!(!d.satisfy(-1));
        assert!(d.conflict(-1));
        assert!(!d.conflict(1));
        assert!(d.decided(1));
        assert!(d.decided_install(1));
    }

    #[test]
    fn test_decide_negative() {
        let mut d = Decisions::new();
        d.decide(-1, 1, 0).unwrap(); // don't install package 1

        assert!(d.satisfy(-1));
        assert!(!d.satisfy(1));
        assert!(d.conflict(1));
        assert!(d.decided(1));
        assert!(!d.decided_install(1));
    }

    #[test]
    fn test_undecided() {
        let d = Decisions::new();
        assert!(d.undecided(1));
        assert!(!d.decided(1));
        assert!(!d.satisfy(1));
        assert!(!d.conflict(1));
    }

    #[test]
    fn test_revert_last() {
        let mut d = Decisions::new();
        d.decide(1, 1, 0).unwrap();
        d.decide(2, 2, 1).unwrap();

        assert!(d.decided(2));
        d.revert_last();
        assert!(d.undecided(2));
        assert!(d.decided(1));
    }

    #[test]
    fn test_reset_to_offset() {
        let mut d = Decisions::new();
        d.decide(1, 1, 0).unwrap();
        d.decide(2, 2, 1).unwrap();
        d.decide(3, 3, 2).unwrap();

        d.reset_to_offset(0); // keep only first decision
        assert_eq!(d.len(), 1);
        assert!(d.decided(1));
        assert!(d.undecided(2));
        assert!(d.undecided(3));
    }

    #[test]
    fn test_double_decide_error() {
        let mut d = Decisions::new();
        d.decide(1, 1, 0).unwrap();
        assert!(d.decide(1, 2, 1).is_err());
    }

    #[test]
    fn test_decision_level() {
        let mut d = Decisions::new();
        d.decide(1, 3, 0).unwrap();
        assert_eq!(d.decision_level(1), 3);
        assert_eq!(d.decision_level(2), 0); // undecided
    }
}
