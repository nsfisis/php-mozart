use crate::pool::{Literal, PackageId, Pool, PoolLink};
use crate::rule::{ReasonData, Rule, RuleReason, RuleType};
use crate::rule_set::RuleSet;
use std::collections::{HashMap, HashSet, VecDeque};

/// Generates SAT rules from the pool and request.
///
/// Port of Composer's RuleSetGenerator.php.
pub struct RuleSetGenerator<'a> {
    pool: &'a mut Pool,
    rules: RuleSet,
    /// Packages already processed.
    added_map: HashSet<PackageId>,
    /// Package names → list of package IDs with that name (non-alias).
    added_packages_by_name: HashMap<String, Vec<PackageId>>,
    /// Platform packages to ignore.
    ignore_platform_reqs: HashSet<String>,
}

impl<'a> RuleSetGenerator<'a> {
    pub fn new(pool: &'a mut Pool) -> Self {
        RuleSetGenerator {
            pool,
            rules: RuleSet::new(),
            added_map: HashSet::new(),
            added_packages_by_name: HashMap::new(),
            ignore_platform_reqs: HashSet::new(),
        }
    }

    /// Set platform requirements to ignore.
    pub fn set_ignore_platform_reqs(&mut self, names: HashSet<String>) {
        self.ignore_platform_reqs = names;
    }

    /// Generate rules for a set of requirements and fixed packages.
    ///
    /// Port of Composer's RuleSetGenerator::getRulesFor.
    pub fn generate(
        mut self,
        requires: &HashMap<String, Option<String>>,
        fixed_packages: &[PackageId],
    ) -> RuleSet {
        // Process fixed packages
        for &pkg_id in fixed_packages {
            if self.pool.is_unacceptable_fixed_package(pkg_id) {
                continue;
            }

            self.add_rules_for_package(pkg_id);

            // Create assertion rule: this package must be installed
            let rule = Rule::new(
                vec![pkg_id as Literal],
                RuleReason::Fixed,
                ReasonData::Fixed { package_id: pkg_id },
            );
            self.rules.add(rule, RuleType::Request);
        }

        // Process root requirements
        for (name, constraint) in requires {
            if self.ignore_platform_reqs.contains(name.as_str()) {
                continue;
            }

            let providers = self.pool.what_provides(name, constraint.as_deref());

            if !providers.is_empty() {
                for &pkg_id in &providers {
                    self.add_rules_for_package(pkg_id);
                }

                // Create "install one of" rule
                let literals: Vec<Literal> = providers.iter().map(|&id| id as Literal).collect();
                let rule = Rule::new(
                    literals,
                    RuleReason::RootRequire,
                    ReasonData::RootRequire {
                        package_name: name.clone(),
                        constraint: constraint.clone().unwrap_or_default(),
                    },
                );
                self.rules.add(rule, RuleType::Request);
            }
        }

        // Add conflict rules
        self.add_conflict_rules();

        self.rules
    }

    /// Add rules for a package and its transitive dependencies.
    ///
    /// Port of Composer's RuleSetGenerator::addRulesForPackage.
    fn add_rules_for_package(&mut self, pkg_id: PackageId) {
        let mut work_queue: VecDeque<PackageId> = VecDeque::new();
        work_queue.push_back(pkg_id);

        while let Some(current_id) = work_queue.pop_front() {
            if self.added_map.contains(&current_id) {
                continue;
            }
            self.added_map.insert(current_id);

            let pkg = self.pool.package_by_id(current_id);
            let pkg_name = pkg.name.clone();
            let requires = pkg.requires.clone();

            // Index by name (for same-name conflict rules later)
            self.added_packages_by_name
                .entry(pkg_name)
                .or_default()
                .push(current_id);

            // Process each requirement
            for link in requires {
                if self.ignore_platform_reqs.contains(&link.target) {
                    continue;
                }

                let possible_requires = self
                    .pool
                    .what_provides(&link.target, Some(&link.constraint));

                // Create require rule: (-current | provider1 | provider2 | ...)
                let mut literals: Vec<Literal> = vec![-(current_id as Literal)];
                let mut self_fulfilling = false;

                for &provider_id in &possible_requires {
                    if provider_id == current_id {
                        self_fulfilling = true;
                        break;
                    }
                    literals.push(provider_id as Literal);
                }

                if !self_fulfilling {
                    let rule = Rule::new(
                        literals,
                        RuleReason::PackageRequires,
                        ReasonData::Link(PoolLink {
                            target: link.target.clone(),
                            constraint: link.constraint.clone(),
                            source: self.pool.package_by_id(current_id).name.clone(),
                        }),
                    );
                    self.rules.add(rule, RuleType::Package);
                }

                // Enqueue providers for further processing
                for &provider_id in &possible_requires {
                    work_queue.push_back(provider_id);
                }
            }
        }
    }

    /// Add conflict rules: explicit conflicts and same-name rules.
    ///
    /// Port of Composer's RuleSetGenerator::addConflictRules.
    fn add_conflict_rules(&mut self) {
        // Explicit conflicts
        let added_ids: Vec<PackageId> = self.added_map.iter().copied().collect();
        for &pkg_id in &added_ids {
            let pkg = self.pool.package_by_id(pkg_id);
            let conflicts = pkg.conflicts.clone();

            for link in conflicts {
                if self.ignore_platform_reqs.contains(&link.target) {
                    continue;
                }

                if !self.added_packages_by_name.contains_key(&link.target) {
                    continue;
                }

                let conflicting = self
                    .pool
                    .what_provides(&link.target, Some(&link.constraint));

                for &conflict_id in &conflicting {
                    if conflict_id == pkg_id {
                        continue; // ignore self-conflict
                    }
                    let rule = Rule::two_literals(
                        -(pkg_id as Literal),
                        -(conflict_id as Literal),
                        RuleReason::PackageConflict,
                        ReasonData::Link(link.clone()),
                    );
                    self.rules.add(rule, RuleType::Package);
                }
            }
        }

        // Same-name rules: only one version of a package can be installed
        let names_to_process: Vec<(String, Vec<PackageId>)> = self
            .added_packages_by_name
            .iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(name, ids)| (name.clone(), ids.clone()))
            .collect();

        for (name, pkg_ids) in names_to_process {
            let literals: Vec<Literal> = pkg_ids.iter().map(|&id| -(id as Literal)).collect();

            if literals.len() == 2 {
                let rule = Rule::two_literals(
                    literals[0],
                    literals[1],
                    RuleReason::PackageSameName,
                    ReasonData::PackageName(name),
                );
                self.rules.add(rule, RuleType::Package);
            } else if literals.len() >= 3 {
                let rule = Rule::multi_conflict(
                    literals,
                    RuleReason::PackageSameName,
                    ReasonData::PackageName(name),
                );
                self.rules.add(rule, RuleType::Package);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{Pool, PoolLink, PoolPackageInput};

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

    #[test]
    fn test_root_require_generates_rule() {
        let mut pool = Pool::new(
            vec![make_input("a/a", "1.0.0.0"), make_input("a/a", "2.0.0.0")],
            vec![],
        );

        let mut requires = HashMap::new();
        requires.insert("a/a".to_string(), None);

        let generator = RuleSetGenerator::new(&mut pool);
        let rules = generator.generate(&requires, &[]);

        // Should have a request rule: (1 | 2)
        let request_count = rules.iter_type(RuleType::Request).count();
        assert_eq!(request_count, 1);

        // Should have a same-name rule: (-1 | -2)
        let package_count = rules.iter_type(RuleType::Package).count();
        assert!(package_count >= 1);
    }

    #[test]
    fn test_dependency_chain_rules() {
        // a/a 1.0 requires b/b
        let mut pool = Pool::new(
            vec![
                PoolPackageInput {
                    name: "a/a".to_string(),
                    version: "1.0.0.0".to_string(),
                    pretty_version: "1.0.0".to_string(),
                    requires: vec![PoolLink {
                        target: "b/b".to_string(),
                        constraint: "*".to_string(),
                        source: "a/a".to_string(),
                    }],
                    replaces: vec![],
                    provides: vec![],
                    conflicts: vec![],
                    is_fixed: false,
                },
                make_input("b/b", "1.0.0.0"),
            ],
            vec![],
        );

        let mut requires = HashMap::new();
        requires.insert("a/a".to_string(), None);

        let generator = RuleSetGenerator::new(&mut pool);
        let rules = generator.generate(&requires, &[]);

        // Should have:
        // 1. Request rule: (1) — root requires a/a
        // 2. Package rule: (-1 | 2) — a/a requires b/b
        assert!(rules.len() >= 2);
    }

    #[test]
    fn test_fixed_package_rule() {
        let mut pool = Pool::new(vec![make_input("php", "8.2.0.0")], vec![]);

        let generator = RuleSetGenerator::new(&mut pool);
        let rules = generator.generate(&HashMap::new(), &[1]);

        // Should have an assertion rule: (1)
        let request_rules: Vec<_> = rules.iter_type(RuleType::Request).collect();
        assert_eq!(request_rules.len(), 1);
        assert!(request_rules[0].1.is_assertion());
    }
}
