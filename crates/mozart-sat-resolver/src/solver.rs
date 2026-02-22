use crate::decisions::Decisions;
use crate::error::{SolverBugError, SolverError};
use crate::policy::DefaultPolicy;
use crate::pool::{Literal, PackageId, Pool, literal_to_package_id};
use crate::problem::Problem;
use crate::rule::{ReasonData, Rule, RuleReason, RuleType};
use crate::rule_set::{RuleId, RuleSet};
use crate::rule_watch_graph::RuleWatchGraph;
use std::collections::{HashMap, HashSet};

/// Result of solving: the list of package IDs to install.
#[derive(Debug)]
pub struct SolverResult {
    /// Package IDs decided for installation.
    pub installed: Vec<PackageId>,
}

/// Main SAT solver implementing CDCL (Conflict-Driven Clause Learning).
///
/// Port of Composer's Solver.php.
pub struct Solver<'a> {
    pool: &'a Pool,
    policy: DefaultPolicy,
    rules: RuleSet,
    watch_graph: RuleWatchGraph,
    decisions: Decisions,
    /// Fixed packages by ID.
    fixed_map: HashSet<PackageId>,
    /// Current propagation index in decision queue.
    propagate_index: usize,
    /// Branch points: (alternative literals, decision level).
    branches: Vec<(Vec<Literal>, i32)>,
    /// Problems found during solving.
    problems: Vec<Problem>,
    /// Learned rule pool: for each learned rule, the chain of rules that led to it.
    learned_pool: Vec<Vec<RuleId>>,
    /// Map from rule ID → learned pool index.
    learned_why: HashMap<RuleId, usize>,
}

impl<'a> Solver<'a> {
    /// Create a new solver with the given rules, pool, policy, and fixed package set.
    pub fn new(
        rules: RuleSet,
        pool: &'a Pool,
        policy: DefaultPolicy,
        fixed_packages: HashSet<PackageId>,
    ) -> Self {
        Solver {
            pool,
            policy,
            rules,
            watch_graph: RuleWatchGraph::new(),
            decisions: Decisions::new(),
            fixed_map: fixed_packages,
            propagate_index: 0,
            branches: Vec::new(),
            problems: Vec::new(),
            learned_pool: Vec::new(),
            learned_why: HashMap::new(),
        }
    }

    /// Solve the dependency resolution problem.
    /// Returns the set of packages to install, or an error.
    pub fn solve(mut self) -> Result<SolverResult, SolverError> {
        // Insert all rules into watch graph
        let rule_count = self.rules.len();
        for id in 0..rule_count {
            let rule = self.rules.rule_by_id(id);
            self.watch_graph.insert(id, rule);
        }

        // Make decisions based on assertion rules (unit clauses)
        self.make_assertion_rule_decisions()?;

        // Run the main SAT loop
        self.run_sat()?;

        if !self.problems.is_empty() {
            let messages: Vec<String> = self
                .problems
                .iter()
                .map(|p| p.pretty_string(self.pool, &self.rules))
                .collect();
            return Err(SolverError::Unsolvable(messages));
        }

        // Collect installed packages
        let mut installed = Vec::new();
        for i in 0..self.decisions.len() {
            let decision = self.decisions.at_offset(i);
            if decision.literal > 0 {
                installed.push(literal_to_package_id(decision.literal));
            }
        }

        Ok(SolverResult { installed })
    }

    /// Process assertion rules (unit clauses) — make immediate decisions.
    ///
    /// Port of Composer's Solver::makeAssertionRuleDecisions.
    fn make_assertion_rule_decisions(&mut self) -> Result<(), SolverError> {
        let decision_start = if self.decisions.is_empty() {
            0
        } else {
            self.decisions.len() - 1
        };

        let mut rule_index: usize = 0;
        while rule_index < self.rules.len() {
            let rule = self.rules.rule_by_id(rule_index);

            if !rule.is_assertion() || rule.is_disabled() {
                rule_index += 1;
                continue;
            }

            let literal = rule.literals()[0];

            if !self.decisions.decided(literal) {
                self.decisions.decide(literal, 1, rule_index)?;
                rule_index += 1;
                continue;
            }

            if self.decisions.satisfy(literal) {
                rule_index += 1;
                continue;
            }

            // Found a conflict
            let rule_type = self.rules.rule_by_id(rule_index).rule_type;

            if rule_type == RuleType::Learned {
                self.rules.rule_by_id_mut(rule_index).disable();
                rule_index += 1;
                continue;
            }

            let conflict_rule_id = self.decisions.decision_rule(literal)?;
            let conflict_type = self.rules.rule_by_id(conflict_rule_id).rule_type;

            if conflict_type == RuleType::Package {
                let mut problem = Problem::new();
                problem.add_rule(rule_index);
                problem.add_rule(conflict_rule_id);
                self.rules.rule_by_id_mut(rule_index).disable();
                self.problems.push(problem);
                rule_index += 1;
                continue;
            }

            // Conflict with another root require/fixed package
            let mut problem = Problem::new();
            problem.add_rule(rule_index);
            problem.add_rule(conflict_rule_id);

            // Push all request assertion rules asserting this literal
            let pkg_id = literal_to_package_id(literal);
            let request_rule_ids: Vec<RuleId> = self
                .rules
                .iter_type(RuleType::Request)
                .filter(|(_, r)| {
                    !r.is_disabled()
                        && r.is_assertion()
                        && literal_to_package_id(r.literals()[0]) == pkg_id
                })
                .map(|(id, _)| id)
                .collect();

            for rid in &request_rule_ids {
                problem.add_rule(*rid);
            }
            self.problems.push(problem);

            for rid in request_rule_ids {
                self.rules.rule_by_id_mut(rid).disable();
            }

            self.decisions.reset_to_offset(decision_start);
            rule_index = 0; // restart
        }

        Ok(())
    }

    /// Unit propagation: propagate decisions through the watch graph.
    ///
    /// Port of Composer's Solver::propagate.
    fn propagate(&mut self, level: i32) -> Result<Option<RuleId>, SolverBugError> {
        while self.decisions.valid_offset(self.propagate_index) {
            let decision = self.decisions.at_offset(self.propagate_index).clone();
            self.propagate_index += 1;

            let conflict = self.watch_graph.propagate_literal(
                decision.literal,
                level,
                &mut self.decisions,
                &self.rules,
            )?;

            if conflict.is_some() {
                return Ok(conflict);
            }
        }

        Ok(None)
    }

    /// Revert decisions to a given level.
    ///
    /// Port of Composer's Solver::revert.
    fn revert(&mut self, level: i32) {
        while !self.decisions.is_empty() {
            let literal = self.decisions.last_literal();
            if self.decisions.undecided(literal) {
                break;
            }
            let decision_level = self.decisions.decision_level(literal);
            if decision_level <= level {
                break;
            }
            self.decisions.revert_last();
            self.propagate_index = self.decisions.len();
        }

        while !self.branches.is_empty() && self.branches.last().unwrap().1 >= level {
            self.branches.pop();
        }
    }

    /// Make a decision, propagate, and learn from conflicts.
    ///
    /// Port of Composer's Solver::setPropagateLearn.
    fn set_propagate_learn(
        &mut self,
        mut level: i32,
        literal: Literal,
        rule_id: RuleId,
    ) -> Result<i32, SolverError> {
        level += 1;
        self.decisions.decide(literal, level, rule_id)?;

        loop {
            let conflict = self.propagate(level)?;

            let Some(conflict_rule_id) = conflict else {
                break;
            };

            if level == 1 {
                self.analyze_unsolvable(conflict_rule_id);
                return Ok(0);
            }

            // Conflict analysis
            let (learn_literal, new_level, new_rule, why) =
                self.analyze(level, conflict_rule_id)?;

            if new_level <= 0 || new_level >= level {
                return Err(SolverBugError {
                    message: format!(
                        "Trying to revert to invalid level {new_level} from level {level}."
                    ),
                }
                .into());
            }

            level = new_level;
            self.revert(level);

            // Add learned rule
            self.rules.add(new_rule, RuleType::Learned);
            let new_rule_id = self.rules.len() - 1;

            self.learned_why.insert(new_rule_id, why);

            let rule_ref = self.rules.rule_by_id(new_rule_id);
            self.watch_graph.insert(new_rule_id, rule_ref);

            // Adjust watch2 to highest level literal
            let last_node = self.watch_graph.last_node_idx();
            let rule_for_watch = self.rules.rule_by_id(new_rule_id);
            self.watch_graph
                .watch2_on_highest(last_node, rule_for_watch, &self.decisions);

            self.decisions.decide(learn_literal, level, new_rule_id)?;
        }

        Ok(level)
    }

    /// Choose best package from candidates and install.
    ///
    /// Port of Composer's Solver::selectAndInstall.
    fn select_and_install(
        &mut self,
        level: i32,
        decision_queue: Vec<Literal>,
        rule_id: RuleId,
    ) -> Result<i32, SolverError> {
        let required_package = self
            .rules
            .rule_by_id(rule_id)
            .required_package()
            .map(|s| s.to_string());
        let mut literals = self.policy.select_preferred_packages(
            self.pool,
            &decision_queue,
            required_package.as_deref(),
        );

        let selected = literals.remove(0);

        // If there are remaining alternatives, save as branch point
        if !literals.is_empty() {
            self.branches.push((literals, level));
        }

        self.set_propagate_learn(level, selected, rule_id)
    }

    /// First UIP conflict analysis.
    ///
    /// Port of Composer's Solver::analyze.
    fn analyze(
        &mut self,
        level: i32,
        conflict_rule_id: RuleId,
    ) -> Result<(Literal, i32, Rule, usize), SolverError> {
        let mut rule_level: i32 = 1;
        let mut num: i32 = 0;
        let mut l1num: i32 = 0;
        let mut seen: HashSet<PackageId> = HashSet::new();
        let mut learned_literal: Option<Literal> = None;
        let mut other_learned_literals: Vec<Literal> = Vec::new();

        let mut decision_id = self.decisions.len();

        self.learned_pool.push(Vec::new());
        let pool_idx = self.learned_pool.len() - 1;

        let mut current_rule_id = conflict_rule_id;

        loop {
            self.learned_pool[pool_idx].push(current_rule_id);

            let rule = self.rules.rule_by_id(current_rule_id);
            let rule_literals = rule.literals().to_vec();
            let is_multi_conflict = rule.is_multi_conflict;

            for &literal in &rule_literals {
                // MultiConflictRule: skip undecided literals
                if is_multi_conflict && !self.decisions.decided(literal) {
                    continue;
                }

                // Skip the one true literal
                if self.decisions.satisfy(literal) {
                    continue;
                }

                let pkg_id = literal_to_package_id(literal);
                if seen.contains(&pkg_id) {
                    continue;
                }
                seen.insert(pkg_id);

                let l = self.decisions.decision_level(literal);

                if l == 1 {
                    l1num += 1;
                } else if l == level {
                    num += 1;
                } else {
                    other_learned_literals.push(literal);
                    if l > rule_level {
                        rule_level = l;
                    }
                }
            }

            // l1 retry loop
            let mut l1retry = true;
            while l1retry {
                l1retry = false;

                if num == 0 {
                    l1num -= 1;
                    if l1num == 0 {
                        // All level 1 literals done
                        let why = pool_idx;
                        let ll = learned_literal.ok_or_else(|| SolverBugError {
                            message: format!(
                                "Did not find a learnable literal in analyzed rule {conflict_rule_id}."
                            ),
                        })?;

                        let mut all_literals = vec![ll];
                        all_literals.extend_from_slice(&other_learned_literals);

                        let new_rule =
                            Rule::new(all_literals, RuleReason::Learned, ReasonData::Learned(why));

                        return Ok((ll, rule_level, new_rule, why));
                    }
                }

                loop {
                    if decision_id == 0 {
                        return Err(SolverBugError {
                            message: format!(
                                "Reached invalid decision id 0 while analyzing rule {conflict_rule_id}."
                            ),
                        }
                        .into());
                    }

                    decision_id -= 1;
                    let decision = self.decisions.at_offset(decision_id);
                    let literal = decision.literal;

                    if seen.contains(&literal_to_package_id(literal)) {
                        break;
                    }
                }

                let decision = self.decisions.at_offset(decision_id);
                let literal = decision.literal;

                seen.remove(&literal_to_package_id(literal));

                if num != 0 {
                    num -= 1;
                    if num == 0 {
                        learned_literal = Some(-literal);

                        if l1num == 0 {
                            // Done
                            let why = pool_idx;
                            let ll = learned_literal.unwrap();

                            let mut all_literals = vec![ll];
                            all_literals.extend_from_slice(&other_learned_literals);

                            let new_rule = Rule::new(
                                all_literals,
                                RuleReason::Learned,
                                ReasonData::Learned(why),
                            );

                            return Ok((ll, rule_level, new_rule, why));
                        }

                        // Only level 1 marks left
                        for other in &other_learned_literals {
                            seen.remove(&literal_to_package_id(*other));
                        }
                        l1num += 1;
                        l1retry = true;
                    } else {
                        let decision = self.decisions.at_offset(decision_id);
                        let next_rule_id = decision.rule_id;
                        let next_rule = self.rules.rule_by_id(next_rule_id);

                        if next_rule.is_multi_conflict {
                            // Handle multi-conflict rule
                            let mcr_literals = next_rule.literals().to_vec();
                            for &rule_literal in &mcr_literals {
                                let pkg_id = literal_to_package_id(rule_literal);
                                if !seen.contains(&pkg_id) && self.decisions.satisfy(-rule_literal)
                                {
                                    self.learned_pool[pool_idx].push(next_rule_id);
                                    let l = self.decisions.decision_level(rule_literal);
                                    if l == 1 {
                                        l1num += 1;
                                    } else if l == level {
                                        num += 1;
                                    } else {
                                        other_learned_literals.push(rule_literal);
                                        if l > rule_level {
                                            rule_level = l;
                                        }
                                    }
                                    seen.insert(pkg_id);
                                    break;
                                }
                            }
                            l1retry = true;
                        }
                    }
                }
            }

            let decision = self.decisions.at_offset(decision_id);
            current_rule_id = decision.rule_id;
        }
    }

    /// Recursively collect rules involved in an unsolvable conflict.
    fn analyze_unsolvable_rule(
        &self,
        problem: &mut Problem,
        conflict_rule_id: RuleId,
        rule_seen: &mut HashSet<RuleId>,
    ) {
        if rule_seen.contains(&conflict_rule_id) {
            return;
        }
        rule_seen.insert(conflict_rule_id);

        let rule = self.rules.rule_by_id(conflict_rule_id);

        if rule.rule_type == RuleType::Learned {
            if let Some(&why) = self.learned_why.get(&conflict_rule_id)
                && let Some(problem_rules) = self.learned_pool.get(why)
            {
                for &pr_id in problem_rules {
                    if !rule_seen.contains(&pr_id) {
                        self.analyze_unsolvable_rule(problem, pr_id, rule_seen);
                    }
                }
            }
            return;
        }

        if rule.rule_type == RuleType::Package {
            // Package rules cannot be part of a problem
            return;
        }

        problem.next_section();
        problem.add_rule(conflict_rule_id);
    }

    /// Analyze an unsolvable conflict at level 1.
    ///
    /// Port of Composer's Solver::analyzeUnsolvable.
    fn analyze_unsolvable(&mut self, conflict_rule_id: RuleId) {
        let mut problem = Problem::new();
        problem.add_rule(conflict_rule_id);

        let mut rule_seen = HashSet::new();
        self.analyze_unsolvable_rule(&mut problem, conflict_rule_id, &mut rule_seen);

        // Collect related decisions
        let mut seen: HashSet<PackageId> = HashSet::new();
        let conflict_literals = self.rules.rule_by_id(conflict_rule_id).literals().to_vec();
        for &lit in &conflict_literals {
            if self.decisions.satisfy(lit) {
                continue;
            }
            seen.insert(literal_to_package_id(lit));
        }

        // Walk decisions in reverse
        for i in (0..self.decisions.len()).rev() {
            let decision = self.decisions.at_offset(i);
            let dec_literal = decision.literal;
            let pkg_id = literal_to_package_id(dec_literal);

            if !seen.contains(&pkg_id) {
                continue;
            }

            let why = decision.rule_id;
            problem.add_rule(why);
            self.analyze_unsolvable_rule(&mut problem, why, &mut rule_seen);

            let why_literals = self.rules.rule_by_id(why).literals().to_vec();
            for &lit in &why_literals {
                if self.decisions.satisfy(lit) {
                    continue;
                }
                seen.insert(literal_to_package_id(lit));
            }
        }

        self.problems.push(problem);
    }

    /// Main SAT loop.
    ///
    /// Port of Composer's Solver::runSat.
    fn run_sat(&mut self) -> Result<(), SolverError> {
        self.propagate_index = 0;

        let mut level: i32 = 1;
        let mut system_level: i32 = level + 1;

        loop {
            // Step 1: propagate at level 1
            if level == 1 {
                let conflict = self.propagate(level)?;
                if let Some(conflict_rule_id) = conflict {
                    self.analyze_unsolvable(conflict_rule_id);
                    return Ok(());
                }
            }

            // Step 2: handle root require/fixed package rules
            if level < system_level {
                let mut made_decision = false;

                // Collect request rule IDs first to avoid borrow issues
                let request_rule_ids: Vec<RuleId> = self
                    .rules
                    .iter_type(RuleType::Request)
                    .map(|(id, _)| id)
                    .collect();

                let mut all_satisfied = true;

                for &rule_id in &request_rule_ids {
                    let rule = self.rules.rule_by_id(rule_id);
                    if !rule.is_enabled() {
                        continue;
                    }

                    let mut decision_queue: Vec<Literal> = Vec::new();
                    let mut none_satisfied = true;

                    for &lit in rule.literals() {
                        if self.decisions.satisfy(lit) {
                            none_satisfied = false;
                            break;
                        }
                        if lit > 0 && self.decisions.undecided(lit) {
                            decision_queue.push(lit);
                        }
                    }

                    if none_satisfied && !decision_queue.is_empty() {
                        // Prune: prefer fixed packages
                        let pruned: Vec<Literal> = decision_queue
                            .iter()
                            .filter(|&&lit| self.fixed_map.contains(&literal_to_package_id(lit)))
                            .copied()
                            .collect();

                        if !pruned.is_empty() {
                            decision_queue = pruned;
                        }
                    }

                    if none_satisfied && !decision_queue.is_empty() {
                        let old_level = level;
                        level = self.select_and_install(level, decision_queue, rule_id)?;

                        if level == 0 {
                            return Ok(());
                        }
                        if level <= old_level {
                            made_decision = true;
                            break;
                        }
                    }

                    // Check if there are more rules to process
                    all_satisfied = false;
                }

                system_level = level + 1;

                if made_decision || !all_satisfied {
                    // Check if we still have unsatisfied request rules
                    let has_unsatisfied = request_rule_ids.iter().any(|&rule_id| {
                        let rule = self.rules.rule_by_id(rule_id);
                        if !rule.is_enabled() {
                            return false;
                        }
                        let mut none_satisfied = true;
                        for &lit in rule.literals() {
                            if self.decisions.satisfy(lit) {
                                none_satisfied = false;
                                break;
                            }
                        }
                        if !none_satisfied {
                            return false;
                        }
                        rule.literals()
                            .iter()
                            .any(|&lit| lit > 0 && self.decisions.undecided(lit))
                    });

                    if has_unsatisfied {
                        continue;
                    }
                }
            }

            if level < system_level {
                system_level = level;
            }

            // Step 3: fulfill all unresolved rules
            let mut rules_count = self.rules.len();
            let mut i: usize = 0;
            let mut n: usize = 0;
            let mut made_decision = false;

            while n < rules_count {
                if i == rules_count {
                    i = 0;
                }

                let rule = self.rules.rule_by_id(i);
                let literals = rule.literals().to_vec();

                i += 1;
                n += 1;

                if rule.is_disabled() {
                    continue;
                }

                let mut decision_queue: Vec<Literal> = Vec::new();
                let mut skip = false;

                for &lit in &literals {
                    if lit <= 0 {
                        if !self.decisions.decided_install(lit) {
                            skip = true;
                            break;
                        }
                    } else {
                        if self.decisions.decided_install(lit) {
                            skip = true;
                            break;
                        }
                        if self.decisions.undecided(lit) {
                            decision_queue.push(lit);
                        }
                    }
                }

                if skip {
                    continue;
                }

                // Need at least 2 undecided positive literals
                if decision_queue.len() < 2 {
                    continue;
                }

                let rule_id = i - 1;
                level = self.select_and_install(level, decision_queue, rule_id)?;

                if level == 0 {
                    return Ok(());
                }

                // Something changed, restart scan
                rules_count = self.rules.len();
                n = 0;
                i = 0;
                made_decision = true;
            }

            if level < system_level && made_decision {
                continue;
            }

            // Step 4: minimization (backjumping)
            if !self.branches.is_empty() {
                let mut last_literal: Option<Literal> = None;
                let mut last_level: Option<i32> = None;
                let mut last_branch_index: usize = 0;
                let mut last_branch_offset: usize = 0;

                for i in (0..self.branches.len()).rev() {
                    let (ref literals, l) = self.branches[i];
                    for (offset, &literal) in literals.iter().enumerate() {
                        if literal > 0 && self.decisions.decision_level(literal) > l + 1 {
                            last_literal = Some(literal);
                            last_branch_index = i;
                            last_branch_offset = offset;
                            last_level = Some(l);
                        }
                    }
                }

                if let Some(literal) = last_literal {
                    let last_l = last_level.unwrap();

                    self.branches[last_branch_index]
                        .0
                        .remove(last_branch_offset);

                    level = last_l;
                    self.revert(level);

                    let why = self.decisions.last_reason();

                    level = self.set_propagate_learn(level, literal, why)?;

                    if level == 0 {
                        return Ok(());
                    }

                    continue;
                }
            }

            break;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::PoolPackageInput;
    use crate::rule::{ReasonData, Rule, RuleReason, RuleType};

    fn make_input(name: &str, version: &str) -> PoolPackageInput {
        PoolPackageInput {
            name: name.to_string(),
            version: version.to_string(),
            pretty_version: version.to_string(),
            requires: vec![],
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        }
    }

    /// Helper: create a simple problem and solve it.
    /// Creates a pool with N dummy packages (1..=max_id).
    fn make_rules_and_solve(
        rules: Vec<(Rule, RuleType)>,
        fixed: HashSet<PackageId>,
        max_id: u32,
    ) -> Result<SolverResult, SolverError> {
        let mut rs = RuleSet::new();
        for (rule, rt) in rules {
            rs.add(rule, rt);
        }
        let inputs: Vec<_> = (1..=max_id)
            .map(|i| make_input(&format!("pkg/{i}"), &format!("{i}.0.0.0")))
            .collect();
        let pool = Pool::new(inputs, vec![]);
        let policy = DefaultPolicy::default();
        let solver = Solver::new(rs, &pool, policy, fixed);
        solver.solve()
    }

    #[test]
    fn test_single_package_required() {
        // Root requires package 1
        let result = make_rules_and_solve(
            vec![(
                Rule::new(vec![1], RuleReason::RootRequire, ReasonData::None),
                RuleType::Request,
            )],
            HashSet::new(),
            3,
        )
        .unwrap();

        assert_eq!(result.installed, vec![1]);
    }

    #[test]
    fn test_two_packages_required() {
        // Root requires either package 1 or 2, and also requires 3
        let result = make_rules_and_solve(
            vec![
                (
                    Rule::new(vec![1, 2], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
                (
                    Rule::new(vec![3], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
            ],
            HashSet::new(),
            3,
        )
        .unwrap();

        assert!(result.installed.contains(&3));
        // Should install one of 1 or 2
        assert!(result.installed.contains(&1) || result.installed.contains(&2));
    }

    #[test]
    fn test_dependency_chain() {
        // Root requires 1. Package 1 requires 2.
        // Rule for root: (1)
        // Rule for dep: (-1 | 2)
        let result = make_rules_and_solve(
            vec![
                (
                    Rule::new(vec![1], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
                (
                    Rule::new(vec![-1, 2], RuleReason::PackageRequires, ReasonData::None),
                    RuleType::Package,
                ),
            ],
            HashSet::new(),
            3,
        )
        .unwrap();

        assert!(result.installed.contains(&1));
        assert!(result.installed.contains(&2));
    }

    #[test]
    fn test_conflict_resolution() {
        // Root requires 1 or 2. Package 1 conflicts with 3.
        // Package 2 requires 3.
        // Rules:
        //   Request: (1 | 2)
        //   Package: (-1 | -3)  -- conflict
        //   Package: (-2 | 3)   -- dep
        //   Request: (3)        -- root also requires 3
        let result = make_rules_and_solve(
            vec![
                (
                    Rule::new(vec![1, 2], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
                (
                    Rule::two_literals(-1, -3, RuleReason::PackageConflict, ReasonData::None),
                    RuleType::Package,
                ),
                (
                    Rule::new(vec![-2, 3], RuleReason::PackageRequires, ReasonData::None),
                    RuleType::Package,
                ),
                (
                    Rule::new(vec![3], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
            ],
            HashSet::new(),
            3,
        )
        .unwrap();

        // Package 3 is required, so 1 conflicts, must choose 2
        assert!(result.installed.contains(&2));
        assert!(result.installed.contains(&3));
        assert!(!result.installed.contains(&1));
    }

    #[test]
    fn test_same_name_conflict() {
        // Two versions of same package: 1 and 2. Root requires either.
        // Same-name rule: (-1 | -2)
        let result = make_rules_and_solve(
            vec![
                (
                    Rule::new(vec![1, 2], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
                (
                    Rule::two_literals(-1, -2, RuleReason::PackageSameName, ReasonData::None),
                    RuleType::Package,
                ),
            ],
            HashSet::new(),
            3,
        )
        .unwrap();

        // Should install exactly one
        let has_1 = result.installed.contains(&1);
        let has_2 = result.installed.contains(&2);
        assert!(has_1 ^ has_2, "Should install exactly one of 1 or 2");
    }

    #[test]
    fn test_unsolvable() {
        // Root requires 1. Root requires 2. But 1 and 2 conflict.
        let result = make_rules_and_solve(
            vec![
                (
                    Rule::new(vec![1], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
                (
                    Rule::new(vec![2], RuleReason::RootRequire, ReasonData::None),
                    RuleType::Request,
                ),
                (
                    Rule::two_literals(-1, -2, RuleReason::PackageConflict, ReasonData::None),
                    RuleType::Package,
                ),
            ],
            HashSet::new(),
            3,
        );

        assert!(result.is_err());
    }
}
