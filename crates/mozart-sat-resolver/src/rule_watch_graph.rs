use crate::decisions::Decisions;
use crate::pool::Literal;
use crate::rule::Rule;
use crate::rule_set::RuleId;
use std::collections::HashMap;

/// A watch node: tracks which 2 literals a rule watches.
///
/// Port of Composer's RuleWatchNode.php.
#[derive(Debug, Clone)]
struct WatchNode {
    /// First watched literal.
    watch1: Literal,
    /// Second watched literal.
    watch2: Literal,
    /// The rule ID this node refers to.
    rule_id: RuleId,
    /// Whether the rule is a multi-conflict rule.
    is_multi_conflict: bool,
}

/// Efficient unit propagation using 2-watched literals optimization.
///
/// Port of Composer's RuleWatchGraph.php.
pub struct RuleWatchGraph {
    /// Literal → list of watch node indices watching that literal.
    watch_chains: HashMap<Literal, Vec<usize>>,
    /// All watch nodes.
    nodes: Vec<WatchNode>,
}

impl RuleWatchGraph {
    pub fn new() -> Self {
        RuleWatchGraph {
            watch_chains: HashMap::new(),
            nodes: Vec::new(),
        }
    }

    /// Insert a rule into the watch graph.
    /// Assertions (single literal) are skipped.
    pub fn insert(&mut self, rule_id: RuleId, rule: &Rule) {
        if rule.is_assertion() {
            return;
        }

        let literals = rule.literals();
        let node_idx = self.nodes.len();

        let watch1 = literals[0];
        let watch2 = if literals.len() > 1 { literals[1] } else { 0 };

        self.nodes.push(WatchNode {
            watch1,
            watch2,
            rule_id,
            is_multi_conflict: rule.is_multi_conflict,
        });

        if rule.is_multi_conflict {
            // Multi-conflict rules watch ALL their literals
            for &lit in literals {
                self.watch_chains.entry(lit).or_default().push(node_idx);
            }
        } else {
            // Normal rules watch first 2 literals
            self.watch_chains.entry(watch1).or_default().push(node_idx);
            self.watch_chains.entry(watch2).or_default().push(node_idx);
        }
    }

    /// Adjust watch2 to the literal decided at the highest level.
    /// Used for learned rules.
    pub fn watch2_on_highest(&mut self, node_idx: usize, rule: &Rule, decisions: &Decisions) {
        let literals = rule.literals();
        if literals.len() < 3 || rule.is_multi_conflict {
            return;
        }

        let mut watch_level = 0i32;
        let mut best_literal = self.nodes[node_idx].watch2;

        for &lit in literals {
            let level = decisions.decision_level(lit);
            if level > watch_level {
                best_literal = lit;
                watch_level = level;
            }
        }

        let old_watch2 = self.nodes[node_idx].watch2;
        if old_watch2 != best_literal {
            // Remove from old chain, add to new chain
            self.remove_from_chain(old_watch2, node_idx);
            self.nodes[node_idx].watch2 = best_literal;
            self.watch_chains
                .entry(best_literal)
                .or_default()
                .push(node_idx);
        }
    }

    /// Propagate a decision literal through the watch graph.
    /// Returns the rule ID of a conflicting rule, if found.
    ///
    /// Port of Composer's RuleWatchGraph::propagateLiteral.
    pub fn propagate_literal(
        &mut self,
        decided_literal: Literal,
        level: i32,
        decisions: &mut Decisions,
        rules: &crate::rule_set::RuleSet,
    ) -> Result<Option<RuleId>, crate::error::SolverBugError> {
        // We look for rules watching the negation of the decided literal
        let literal = -decided_literal;

        let Some(chain) = self.watch_chains.get(&literal).cloned() else {
            return Ok(None);
        };

        // We need to process nodes; some may be moved to different chains
        let mut i = 0;
        while i < chain.len() {
            let node_idx = chain[i];
            let node = &self.nodes[node_idx];
            let rule_id = node.rule_id;
            let is_multi_conflict = node.is_multi_conflict;
            let rule = rules.rule_by_id(rule_id);

            if !is_multi_conflict {
                let other_watch = if node.watch1 == literal {
                    node.watch2
                } else {
                    node.watch1
                };

                if !rule.is_disabled() && !decisions.satisfy(other_watch) {
                    let rule_literals = rule.literals();

                    // Find an alternative literal to watch
                    let alternative = rule_literals
                        .iter()
                        .find(|&&rl| rl != literal && rl != other_watch && !decisions.conflict(rl));

                    if let Some(&alt_literal) = alternative {
                        // Move watch from `literal` to `alt_literal`
                        self.move_watch(literal, alt_literal, node_idx);
                        // Don't increment i since the node was removed from this chain
                        // We need to re-fetch the chain since it was modified
                        let chain_ref = self.watch_chains.get(&literal);
                        if chain_ref.is_none() || i >= chain_ref.unwrap().len() {
                            break;
                        }
                        continue;
                    }

                    if decisions.conflict(other_watch) {
                        return Ok(Some(rule_id));
                    }

                    decisions.decide(other_watch, level, rule_id)?;
                }
            } else {
                // Multi-conflict rule: all literals are watched
                let rule_literals = rule.literals().to_vec();
                for &other_literal in &rule_literals {
                    if other_literal != literal && !decisions.satisfy(other_literal) {
                        if decisions.conflict(other_literal) {
                            return Ok(Some(rule_id));
                        }
                        decisions.decide(other_literal, level, rule_id)?;
                    }
                }
            }

            i += 1;
            // Re-fetch chain in case it was modified
            let chain_ref = self.watch_chains.get(&literal);
            if chain_ref.is_none() || i >= chain_ref.unwrap().len() {
                break;
            }
        }

        Ok(None)
    }

    /// Move a watch node from one literal's chain to another's.
    fn move_watch(&mut self, from_literal: Literal, to_literal: Literal, node_idx: usize) {
        // Update the node's watch
        let node = &mut self.nodes[node_idx];
        if node.watch1 == from_literal {
            node.watch1 = to_literal;
        } else {
            node.watch2 = to_literal;
        }

        // Remove from old chain
        self.remove_from_chain(from_literal, node_idx);

        // Add to new chain
        self.watch_chains
            .entry(to_literal)
            .or_default()
            .push(node_idx);
    }

    /// Remove a node from a literal's watch chain.
    fn remove_from_chain(&mut self, literal: Literal, node_idx: usize) {
        if let Some(chain) = self.watch_chains.get_mut(&literal) {
            chain.retain(|&idx| idx != node_idx);
        }
    }

    /// Get the last inserted node index (for watch2_on_highest after insert).
    pub fn last_node_idx(&self) -> usize {
        self.nodes.len() - 1
    }
}

impl Default for RuleWatchGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{ReasonData, Rule, RuleReason};
    use crate::rule_set::RuleSet;

    #[test]
    fn test_insert_assertion_skipped() {
        let mut graph = RuleWatchGraph::new();
        let rule = Rule::new(vec![1], RuleReason::Fixed, ReasonData::None);
        graph.insert(0, &rule);
        assert_eq!(graph.nodes.len(), 0);
    }

    #[test]
    fn test_insert_normal_rule() {
        let mut graph = RuleWatchGraph::new();
        let rule = Rule::new(vec![1, 2, 3], RuleReason::PackageRequires, ReasonData::None);
        graph.insert(0, &rule);
        assert_eq!(graph.nodes.len(), 1);
        // Watches literals 1 and 2
        assert!(graph.watch_chains.contains_key(&1));
        assert!(graph.watch_chains.contains_key(&2));
    }

    #[test]
    fn test_propagate_unit_clause() {
        // Rule: (1 | 2). Decide -1, should force 2.
        let mut rs = RuleSet::new();
        rs.add(
            Rule::new(vec![1, 2], RuleReason::PackageRequires, ReasonData::None),
            crate::rule::RuleType::Package,
        );

        let mut graph = RuleWatchGraph::new();
        graph.insert(0, rs.rule_by_id(0));

        let mut decisions = Decisions::new();
        decisions.decide(-1, 1, 99).unwrap(); // don't install package 1

        let conflict = graph.propagate_literal(-1, 1, &mut decisions, &rs).unwrap();
        assert!(conflict.is_none());
        // Package 2 should now be decided install
        assert!(decisions.decided_install(2));
    }

    #[test]
    fn test_propagate_conflict() {
        // Rule: (1 | 2). Decide -1, then -2 should conflict.
        let mut rs = RuleSet::new();
        rs.add(
            Rule::new(vec![1, 2], RuleReason::PackageRequires, ReasonData::None),
            crate::rule::RuleType::Package,
        );

        let mut graph = RuleWatchGraph::new();
        graph.insert(0, rs.rule_by_id(0));

        let mut decisions = Decisions::new();
        decisions.decide(-1, 1, 99).unwrap();
        decisions.decide(-2, 1, 99).unwrap();

        let conflict = graph.propagate_literal(-1, 1, &mut decisions, &rs).unwrap();
        assert!(conflict.is_some());
    }
}
