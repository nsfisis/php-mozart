use crate::pool::{Literal, Pool, literal_to_package_id};
use crate::rule::{ReasonData, Rule, RuleReason};
use crate::rule_set::{RuleId, RuleSet};

/// Represents a conflict found during resolution.
/// Collects the rules involved in the problem.
///
/// Port of Composer's Problem.php.
#[derive(Debug, Clone)]
pub struct Problem {
    /// Sections of rules that form this problem.
    /// Each section is a group of related rules.
    sections: Vec<Vec<RuleId>>,
}

impl Problem {
    pub fn new() -> Self {
        Problem {
            sections: vec![vec![]],
        }
    }

    /// Add a rule to the current section.
    pub fn add_rule(&mut self, rule_id: RuleId) {
        if let Some(section) = self.sections.last_mut()
            && !section.contains(&rule_id)
        {
            section.push(rule_id);
        }
    }

    /// Start a new section.
    pub fn next_section(&mut self) {
        if self.sections.last().is_some_and(|s| !s.is_empty()) {
            self.sections.push(vec![]);
        }
    }

    /// Get all rule IDs in this problem.
    pub fn rule_ids(&self) -> Vec<RuleId> {
        self.sections.iter().flatten().copied().collect()
    }

    /// Format the problem as a human-readable string using Pool data.
    ///
    /// Port of Composer's Problem::getPrettyString().
    pub fn pretty_string(&self, pool: &Pool, rules: &RuleSet) -> String {
        // Flatten all sections (reversed) like Composer does
        let mut all_rules: Vec<RuleId> = self.sections.iter().rev().flatten().copied().collect();

        if all_rules.is_empty() {
            return "Unknown problem".to_string();
        }

        // Sort by priority, then by sortable string
        all_rules.sort_by(|&a, &b| {
            let rule_a = rules.rule_by_id(a);
            let rule_b = rules.rule_by_id(b);
            let prio_a = rule_priority(rule_a);
            let prio_b = rule_priority(rule_b);
            if prio_a != prio_b {
                return prio_b.cmp(&prio_a);
            }
            sortable_string(pool, rule_a).cmp(&sortable_string(pool, rule_b))
        });

        // Format each rule
        let mut messages: Vec<String> = Vec::new();
        for &rule_id in &all_rules {
            let rule = rules.rule_by_id(rule_id);
            let msg = rule_pretty_string(pool, rule);
            if !msg.is_empty() {
                messages.push(msg);
            }
        }

        // Deduplicate
        let mut seen = std::collections::HashSet::new();
        let mut unique = Vec::new();
        for msg in messages {
            if seen.insert(msg.clone()) {
                unique.push(msg);
            }
        }

        if unique.is_empty() {
            return "Unknown problem".to_string();
        }

        unique
            .iter()
            .map(|m| format!("    - {m}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Basic format for backward compatibility (uses rule Display).
    pub fn format(&self, rules: &RuleSet) -> String {
        let mut parts = Vec::new();
        for section in &self.sections {
            for &rule_id in section {
                let rule = rules.rule_by_id(rule_id);
                parts.push(format!("  - {rule}"));
            }
        }
        if parts.is_empty() {
            "Unknown problem".to_string()
        } else {
            parts.join("\n")
        }
    }
}

impl Default for Problem {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the sort priority for a rule (higher = more important).
/// Port of Problem::getRulePriority().
fn rule_priority(rule: &Rule) -> u8 {
    match rule.reason {
        RuleReason::Fixed => 3,
        RuleReason::RootRequire => 2,
        RuleReason::PackageConflict | RuleReason::PackageRequires => 1,
        RuleReason::PackageSameName
        | RuleReason::Learned
        | RuleReason::PackageAlias
        | RuleReason::PackageInverseAlias => 0,
    }
}

/// Get a sortable string for a rule.
/// Port of Problem::getSortableString().
fn sortable_string(pool: &Pool, rule: &Rule) -> String {
    match (&rule.reason, &rule.reason_data) {
        (RuleReason::RootRequire, ReasonData::RootRequire { package_name, .. }) => {
            package_name.clone()
        }
        (RuleReason::Fixed, ReasonData::Fixed { package_id }) => {
            pool.package_by_id(*package_id).to_string()
        }
        (RuleReason::PackageConflict | RuleReason::PackageRequires, ReasonData::Link(link)) => {
            if let Some(source_lit) = rule.literals().first() {
                let source_pkg = pool.literal_to_package(*source_lit);
                format!("{}//{}", source_pkg, link)
            } else {
                link.to_string()
            }
        }
        (RuleReason::PackageSameName, ReasonData::PackageName(name)) => name.clone(),
        (RuleReason::Learned, _) => rule
            .literals()
            .iter()
            .map(|l: &Literal| l.to_string())
            .collect::<Vec<_>>()
            .join("-"),
        _ => String::new(),
    }
}

/// Format a rule as a human-readable string.
/// Port of Composer's Rule::getPrettyString().
fn rule_pretty_string(pool: &Pool, rule: &Rule) -> String {
    match (&rule.reason, &rule.reason_data) {
        (
            RuleReason::RootRequire,
            ReasonData::RootRequire {
                package_name,
                constraint,
            },
        ) => {
            let providers = format_providers(pool, rule.literals());
            if providers.is_empty() {
                format!(
                    "No package found to satisfy root composer.json require {package_name} {constraint}"
                )
            } else {
                format!(
                    "Root composer.json requires {package_name} {constraint} -> satisfiable by {providers}."
                )
            }
        }

        (RuleReason::Fixed, ReasonData::Fixed { package_id }) => {
            let pkg = pool.package_by_id(*package_id);
            if pkg.is_fixed {
                format!(
                    "{} {} is locked to version {} and an update of this package was not requested.",
                    pkg.name, pkg.pretty_version, pkg.pretty_version
                )
            } else {
                format!(
                    "{} {} is present at version {} and cannot be modified by Mozart",
                    pkg.name, pkg.pretty_version, pkg.pretty_version
                )
            }
        }

        (RuleReason::PackageConflict, ReasonData::Link(link)) => {
            let literals = rule.literals();
            if literals.len() >= 2 {
                let pkg1 = pool.literal_to_package(literals[0]);
                let pkg2 = pool.literal_to_package(literals[1]);
                // Determine which is the source of the conflict
                if link.source == pkg1.name {
                    format!("{pkg2} conflicts with {pkg1}.")
                } else {
                    format!("{pkg1} conflicts with {pkg2}.")
                }
            } else {
                format!("Conflict: {link}")
            }
        }

        (RuleReason::PackageRequires, ReasonData::Link(link)) => {
            let literals = rule.literals();
            if literals.is_empty() {
                return format!("Requirement: {link}");
            }

            let source_pkg = pool.literal_to_package(literals[0]);
            let base_text = format!(
                "{} {} requires {} {}",
                source_pkg.name, source_pkg.pretty_version, link.target, link.constraint
            );

            // Remaining literals are the satisfying packages
            let provider_lits: Vec<Literal> = literals[1..].to_vec();
            if provider_lits.is_empty() {
                format!("{base_text} -> no matching package found.")
            } else {
                let providers = format_providers(pool, &provider_lits);
                format!("{base_text} -> satisfiable by {providers}.")
            }
        }

        (RuleReason::PackageSameName, ReasonData::PackageName(name)) => {
            let literals = rule.literals();
            // Collect unique package names in this rule
            let mut pkg_names: Vec<String> = Vec::new();
            for &lit in literals {
                let pkg = pool.literal_to_package(lit);
                if !pkg_names.contains(&pkg.name) {
                    pkg_names.push(pkg.name.clone());
                }
            }

            if pkg_names.len() > 1 {
                // Different packages that replace/provide the same name
                let replacers: Vec<&str> = pkg_names
                    .iter()
                    .filter(|n| n.as_str() != name)
                    .map(|n| n.as_str())
                    .collect();

                let reason = if replacers.is_empty() {
                    format!("They all replace {name} and thus cannot coexist.")
                } else if !pkg_names.contains(name) {
                    format!(
                        "They {} replace {name} and thus cannot coexist.",
                        if literals.len() == 2 { "both" } else { "all" }
                    )
                } else if replacers.len() == 1 {
                    format!(
                        "{} replaces {name} and thus cannot coexist with it.",
                        replacers[0]
                    )
                } else {
                    format!(
                        "[{}] replace {name} and thus cannot coexist with it.",
                        replacers.join(", ")
                    )
                };

                let pkgs_str = format_providers(pool, literals);
                format!("Only one of these can be installed: {pkgs_str}. {reason}")
            } else {
                // Same package, different versions
                let pkgs_str = format_providers(pool, literals);
                format!(
                    "You can only install one version of a package, so only one of these can be installed: {pkgs_str}."
                )
            }
        }

        (RuleReason::Learned, _) => {
            let literals = rule.literals();
            if literals.len() == 1 {
                let pretty = pool.literal_to_pretty_string(literals[0]);
                format!("Conclusion: {pretty} (conflict analysis result)")
            } else {
                // Group literals by install/don't install
                let mut install = Vec::new();
                let mut dont_install = Vec::new();
                for &lit in literals {
                    if lit > 0 {
                        install.push(lit);
                    } else {
                        dont_install.push(lit);
                    }
                }

                let mut parts = Vec::new();
                if !install.is_empty() {
                    let pkgs = format_providers(pool, &install);
                    if install.len() > 1 {
                        parts.push(format!("install one of {pkgs}"));
                    } else {
                        parts.push(format!("install {pkgs}"));
                    }
                }
                if !dont_install.is_empty() {
                    let pkgs = format_providers_abs(pool, &dont_install);
                    if dont_install.len() > 1 {
                        parts.push(format!("don't install one of {pkgs}"));
                    } else {
                        parts.push(format!("don't install {pkgs}"));
                    }
                }

                format!(
                    "Conclusion: {} (conflict analysis result)",
                    parts.join(" | ")
                )
            }
        }

        (RuleReason::PackageAlias, _) => {
            let literals = rule.literals();
            if literals.len() >= 2 {
                let alias_pkg = pool.literal_to_package(literals[0]);
                let target_pkg = pool.literal_to_package(literals[1]);
                format!(
                    "{alias_pkg} is an alias of {target_pkg} and thus requires it to be installed too."
                )
            } else {
                String::new()
            }
        }

        (RuleReason::PackageInverseAlias, _) => {
            let literals = rule.literals();
            if literals.len() >= 2 {
                let target_pkg = pool.literal_to_package(literals[0]);
                let alias_pkg = pool.literal_to_package(literals[1]);
                format!("{alias_pkg} is an alias of {target_pkg} and must be installed with it.")
            } else {
                String::new()
            }
        }

        _ => {
            // Fallback: display raw literals
            let literal_strs: Vec<String> = rule
                .literals()
                .iter()
                .map(|&l| pool.literal_to_pretty_string(l))
                .collect();
            literal_strs.join(" | ")
        }
    }
}

/// Format a list of literals as a list of package names grouped by name.
/// Similar to Composer's formatPackagesUnique.
fn format_providers(pool: &Pool, literals: &[Literal]) -> String {
    // Group by package name
    let mut groups: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for &lit in literals {
        let pkg = pool.literal_to_package(lit);
        groups
            .entry(&pkg.name)
            .or_default()
            .push(&pkg.pretty_version);
    }

    let mut parts: Vec<String> = Vec::new();
    for (name, versions) in &groups {
        if versions.len() == 1 {
            parts.push(format!("{name} {}", versions[0]));
        } else {
            let v_str = versions.join(", ");
            parts.push(format!("{name}[{v_str}]"));
        }
    }

    parts.sort();
    parts.join(", ")
}

/// Same as format_providers but uses absolute value of literals.
fn format_providers_abs(pool: &Pool, literals: &[Literal]) -> String {
    let abs_lits: Vec<Literal> = literals
        .iter()
        .map(|&l| literal_to_package_id(l) as Literal)
        .collect();
    format_providers(pool, &abs_lits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::PoolPackageInput;
    use crate::rule::{ReasonData, Rule, RuleReason, RuleType};

    fn make_input(name: &str, version: &str, pretty: &str) -> PoolPackageInput {
        PoolPackageInput {
            name: name.to_string(),
            version: version.to_string(),
            pretty_version: pretty.to_string(),
            requires: vec![],
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        }
    }

    #[test]
    fn test_root_require_pretty_string() {
        let pool = Pool::new(vec![make_input("foo/bar", "1.0.0.0", "1.0.0")], vec![]);

        let mut rule_set = RuleSet::new();
        let rule = Rule::new(
            vec![1],
            RuleReason::RootRequire,
            ReasonData::RootRequire {
                package_name: "foo/bar".to_string(),
                constraint: "^1.0".to_string(),
            },
        );
        rule_set.add(rule, RuleType::Request);

        let mut problem = Problem::new();
        problem.add_rule(0);

        let output = problem.pretty_string(&pool, &rule_set);
        assert!(output.contains("Root composer.json requires foo/bar ^1.0"));
        assert!(output.contains("satisfiable by foo/bar 1.0.0"));
    }

    #[test]
    fn test_same_name_pretty_string() {
        let pool = Pool::new(
            vec![
                make_input("foo/bar", "1.0.0.0", "1.0.0"),
                make_input("foo/bar", "2.0.0.0", "2.0.0"),
            ],
            vec![],
        );

        let mut rule_set = RuleSet::new();
        let rule = Rule::new(
            vec![-1, -2],
            RuleReason::PackageSameName,
            ReasonData::PackageName("foo/bar".to_string()),
        );
        rule_set.add(rule, RuleType::Package);

        let mut problem = Problem::new();
        problem.add_rule(0);

        let output = problem.pretty_string(&pool, &rule_set);
        assert!(output.contains("You can only install one version"));
    }

    #[test]
    fn test_package_requires_pretty_string() {
        let pool = Pool::new(
            vec![
                make_input("foo/bar", "1.0.0.0", "1.0.0"),
                make_input("baz/qux", "2.0.0.0", "2.0.0"),
            ],
            vec![],
        );

        let mut rule_set = RuleSet::new();
        let rule = Rule::new(
            vec![-1, 2],
            RuleReason::PackageRequires,
            ReasonData::Link(crate::pool::PoolLink {
                source: "foo/bar".to_string(),
                target: "baz/qux".to_string(),
                constraint: "^2.0".to_string(),
            }),
        );
        rule_set.add(rule, RuleType::Package);

        let mut problem = Problem::new();
        problem.add_rule(0);

        let output = problem.pretty_string(&pool, &rule_set);
        assert!(output.contains("foo/bar 1.0.0 requires baz/qux ^2.0"));
        assert!(output.contains("satisfiable by baz/qux 2.0.0"));
    }
}
