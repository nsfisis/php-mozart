use crate::rule::{Rule, RuleType};
use std::collections::HashMap;

/// A unique identifier for a rule within the RuleSet.
pub type RuleId = usize;

/// Container for all rules, organized by type.
///
/// Port of Composer's RuleSet.php.
pub struct RuleSet {
    /// Lookup: rule ID → index into the appropriate type vector.
    /// This is the primary read-only access path used by the solver.
    rules_by_id: Vec<usize>,
    /// Rules grouped by type.
    package_rules: Vec<Rule>,
    request_rules: Vec<Rule>,
    learned_rules: Vec<Rule>,
    /// Total rule count.
    next_rule_id: usize,
    /// Deduplication index.
    rules_by_hash: HashMap<String, Vec<usize>>,
    /// Maps rule ID → (type, index within type's vec).
    rule_type_index: Vec<(RuleType, usize)>,
}

impl RuleSet {
    pub fn new() -> Self {
        RuleSet {
            rules_by_id: Vec::new(),
            package_rules: Vec::new(),
            request_rules: Vec::new(),
            learned_rules: Vec::new(),
            next_rule_id: 0,
            rules_by_hash: HashMap::new(),
            rule_type_index: Vec::new(),
        }
    }

    /// Add a rule to the set. Duplicates (by hash + equals) are skipped.
    pub fn add(&mut self, mut rule: Rule, rule_type: RuleType) {
        let hash = rule.hash_key();

        // Check for duplicates
        if let Some(existing_ids) = self.rules_by_hash.get(&hash) {
            for &existing_id in existing_ids {
                if rule.equals(self.rule_by_id(existing_id)) {
                    return;
                }
            }
        }

        rule.rule_type = rule_type;

        let rules_vec = match rule_type {
            RuleType::Package => &mut self.package_rules,
            RuleType::Request => &mut self.request_rules,
            RuleType::Learned => &mut self.learned_rules,
        };
        let idx = rules_vec.len();
        rules_vec.push(rule);

        let rule_id = self.next_rule_id;
        self.rules_by_id.push(idx);
        self.rule_type_index.push((rule_type, idx));
        self.next_rule_id += 1;

        self.rules_by_hash.entry(hash).or_default().push(rule_id);
    }

    /// Total number of rules.
    pub fn len(&self) -> usize {
        self.next_rule_id
    }

    /// Whether the rule set is empty.
    pub fn is_empty(&self) -> bool {
        self.next_rule_id == 0
    }

    /// Look up a rule by its global ID.
    pub fn rule_by_id(&self, id: RuleId) -> &Rule {
        let (rule_type, idx) = self.rule_type_index[id];
        match rule_type {
            RuleType::Package => &self.package_rules[idx],
            RuleType::Request => &self.request_rules[idx],
            RuleType::Learned => &self.learned_rules[idx],
        }
    }

    /// Get a mutable reference to a rule by its global ID.
    pub fn rule_by_id_mut(&mut self, id: RuleId) -> &mut Rule {
        let (rule_type, idx) = self.rule_type_index[id];
        match rule_type {
            RuleType::Package => &mut self.package_rules[idx],
            RuleType::Request => &mut self.request_rules[idx],
            RuleType::Learned => &mut self.learned_rules[idx],
        }
    }

    /// Iterate over all rules in order (Package, then Request, then Learned).
    pub fn iter(&self) -> impl Iterator<Item = (RuleId, &Rule)> {
        (0..self.next_rule_id).map(move |id| (id, self.rule_by_id(id)))
    }

    /// Iterate over rules of a specific type, returning (global_rule_id, &Rule).
    pub fn iter_type(&self, rule_type: RuleType) -> RuleTypeIterator<'_> {
        RuleTypeIterator {
            rule_set: self,
            rule_type,
            current: 0,
            total: self.next_rule_id,
        }
    }

    /// Get the request rules slice.
    pub fn request_rules(&self) -> &[Rule] {
        &self.request_rules
    }
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over rules of a specific type.
pub struct RuleTypeIterator<'a> {
    rule_set: &'a RuleSet,
    rule_type: RuleType,
    current: RuleId,
    total: usize,
}

impl<'a> Iterator for RuleTypeIterator<'a> {
    type Item = (RuleId, &'a Rule);

    fn next(&mut self) -> Option<Self::Item> {
        while self.current < self.total {
            let id = self.current;
            self.current += 1;
            let rule = self.rule_set.rule_by_id(id);
            if rule.rule_type == self.rule_type {
                return Some((id, rule));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{ReasonData, RuleReason};

    #[test]
    fn test_add_and_lookup() {
        let mut rs = RuleSet::new();
        rs.add(
            Rule::new(vec![1, 2], RuleReason::PackageRequires, ReasonData::None),
            RuleType::Package,
        );
        rs.add(
            Rule::new(vec![3], RuleReason::RootRequire, ReasonData::None),
            RuleType::Request,
        );

        assert_eq!(rs.len(), 2);
        assert_eq!(rs.rule_by_id(0).literals(), &[1, 2]);
        assert_eq!(rs.rule_by_id(1).literals(), &[3]);
    }

    #[test]
    fn test_deduplication() {
        let mut rs = RuleSet::new();
        rs.add(
            Rule::new(vec![1, 2], RuleReason::PackageRequires, ReasonData::None),
            RuleType::Package,
        );
        rs.add(
            Rule::new(vec![2, 1], RuleReason::PackageConflict, ReasonData::None),
            RuleType::Package,
        );
        // Duplicate should be skipped
        assert_eq!(rs.len(), 1);
    }

    #[test]
    fn test_iter_type() {
        let mut rs = RuleSet::new();
        rs.add(
            Rule::new(vec![1, 2], RuleReason::PackageRequires, ReasonData::None),
            RuleType::Package,
        );
        rs.add(
            Rule::new(vec![3], RuleReason::RootRequire, ReasonData::None),
            RuleType::Request,
        );
        rs.add(
            Rule::new(vec![4, 5], RuleReason::PackageConflict, ReasonData::None),
            RuleType::Package,
        );

        let request_rules: Vec<_> = rs.iter_type(RuleType::Request).collect();
        assert_eq!(request_rules.len(), 1);
        assert_eq!(request_rules[0].1.literals(), &[3]);

        let package_rules: Vec<_> = rs.iter_type(RuleType::Package).collect();
        assert_eq!(package_rules.len(), 2);
    }
}
