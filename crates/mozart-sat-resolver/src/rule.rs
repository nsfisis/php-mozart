use crate::pool::{Literal, PoolLink};
use std::fmt;

/// Why a rule was created.
/// Port of Composer Rule::RULE_* constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleReason {
    /// Root composer.json requirement.
    RootRequire,
    /// Fixed/locked package.
    Fixed,
    /// Two packages conflict.
    PackageConflict,
    /// Package dependency (requires).
    PackageRequires,
    /// Only one version of a package can be installed.
    PackageSameName,
    /// Learned from conflict analysis.
    Learned,
    /// Alias requires its target.
    PackageAlias,
    /// Target requires its alias.
    PackageInverseAlias,
}

/// Data explaining why a rule was created.
#[derive(Debug, Clone)]
pub enum ReasonData {
    /// For RootRequire: package name + constraint string.
    RootRequire {
        package_name: String,
        constraint: String,
    },
    /// For Fixed: the fixed package ID.
    Fixed { package_id: u32 },
    /// For PackageConflict, PackageRequires: a link.
    Link(PoolLink),
    /// For PackageSameName: the package name.
    PackageName(String),
    /// For Learned: index into the learned pool.
    Learned(usize),
    /// For PackageAlias/InverseAlias: the alias package ID.
    AliasPackage(u32),
    /// No data.
    None,
}

/// The type assigned by RuleSet (which collection this rule belongs to).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleType {
    Package = 0,
    Request = 1,
    Learned = 4,
}

/// A SAT rule (clause). A disjunction of literals: (L1 | L2 | ... | Ln).
///
/// Port of Composer's Rule hierarchy (GenericRule, Rule2Literals, MultiConflictRule).
/// In Rust we use a single enum instead of class inheritance.
#[derive(Debug, Clone)]
pub struct Rule {
    /// The literals in this rule (sorted for deduplication).
    literals: Vec<Literal>,
    /// Whether this is a multi-conflict rule.
    pub is_multi_conflict: bool,
    /// Why this rule was created.
    pub reason: RuleReason,
    /// Additional data about why this rule was created.
    pub reason_data: ReasonData,
    /// Which RuleSet type this rule belongs to.
    pub rule_type: RuleType,
    /// Whether this rule is disabled.
    pub disabled: bool,
}

impl Rule {
    /// Create a generic rule (arbitrary number of literals).
    /// Equivalent to Composer's GenericRule.
    pub fn new(mut literals: Vec<Literal>, reason: RuleReason, reason_data: ReasonData) -> Self {
        literals.sort();
        Rule {
            literals,
            is_multi_conflict: false,
            reason,
            reason_data,
            rule_type: RuleType::Package, // default, set by RuleSet
            disabled: false,
        }
    }

    /// Create a 2-literal rule (optimized common case).
    /// Equivalent to Composer's Rule2Literals.
    pub fn two_literals(
        lit1: Literal,
        lit2: Literal,
        reason: RuleReason,
        reason_data: ReasonData,
    ) -> Self {
        let (a, b) = if lit1 <= lit2 {
            (lit1, lit2)
        } else {
            (lit2, lit1)
        };
        Rule {
            literals: vec![a, b],
            is_multi_conflict: false,
            reason,
            reason_data,
            rule_type: RuleType::Package,
            disabled: false,
        }
    }

    /// Create a multi-conflict rule (3+ literals, all negative).
    /// Equivalent to Composer's MultiConflictRule.
    /// Acts as if it were multiple binary conflict rules.
    pub fn multi_conflict(
        mut literals: Vec<Literal>,
        reason: RuleReason,
        reason_data: ReasonData,
    ) -> Self {
        assert!(
            literals.len() >= 3,
            "MultiConflictRule requires at least 3 literals"
        );
        literals.sort();
        Rule {
            literals,
            is_multi_conflict: true,
            reason,
            reason_data,
            rule_type: RuleType::Package,
            disabled: false,
        }
    }

    /// Get the sorted literals.
    pub fn literals(&self) -> &[Literal] {
        &self.literals
    }

    /// Whether this rule has exactly one literal (unit clause / assertion).
    pub fn is_assertion(&self) -> bool {
        self.literals.len() == 1
    }

    /// Compute a hash for deduplication.
    pub fn hash_key(&self) -> String {
        if self.is_multi_conflict {
            let parts: Vec<String> = self.literals.iter().map(|l| l.to_string()).collect();
            format!("c:{}", parts.join(","))
        } else {
            let parts: Vec<String> = self.literals.iter().map(|l| l.to_string()).collect();
            parts.join(",")
        }
    }

    /// Structural equality check (same literals).
    pub fn equals(&self, other: &Rule) -> bool {
        self.is_multi_conflict == other.is_multi_conflict && self.literals == other.literals
    }

    /// Get the required package name, if applicable.
    pub fn required_package(&self) -> Option<&str> {
        match &self.reason_data {
            ReasonData::RootRequire { package_name, .. } => Some(package_name),
            ReasonData::Link(link) => Some(&link.target),
            ReasonData::Fixed { .. } => None, // would need pool access
            _ => None,
        }
    }

    /// Disable this rule.
    pub fn disable(&mut self) {
        if self.is_multi_conflict {
            panic!("Cannot disable a MultiConflictRule");
        }
        self.disabled = true;
    }

    /// Enable this rule.
    pub fn enable(&mut self) {
        self.disabled = false;
    }

    /// Whether this rule is disabled.
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Whether this rule is enabled.
    pub fn is_enabled(&self) -> bool {
        !self.disabled
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.disabled {
            write!(f, "disabled(")?;
        }
        if self.is_multi_conflict {
            write!(f, "(multi(")?;
            for (i, lit) in self.literals.iter().enumerate() {
                if i > 0 {
                    write!(f, "|")?;
                }
                write!(f, "{lit}")?;
            }
            write!(f, "))")?;
        } else {
            write!(f, "(")?;
            for (i, lit) in self.literals.iter().enumerate() {
                if i > 0 {
                    write!(f, "|")?;
                }
                write!(f, "{lit}")?;
            }
            write!(f, ")")?;
        }
        if self.disabled {
            write!(f, ")")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generic_rule() {
        let rule = Rule::new(vec![3, 1, 2], RuleReason::PackageRequires, ReasonData::None);
        assert_eq!(rule.literals(), &[1, 2, 3]);
        assert!(!rule.is_assertion());
        assert_eq!(rule.to_string(), "(1|2|3)");
    }

    #[test]
    fn test_two_literal_rule() {
        let rule = Rule::two_literals(-2, -1, RuleReason::PackageConflict, ReasonData::None);
        assert_eq!(rule.literals(), &[-2, -1]);
        assert!(!rule.is_assertion());
    }

    #[test]
    fn test_assertion_rule() {
        let rule = Rule::new(vec![1], RuleReason::Fixed, ReasonData::None);
        assert!(rule.is_assertion());
    }

    #[test]
    fn test_multi_conflict_rule() {
        let rule = Rule::multi_conflict(
            vec![-3, -1, -2],
            RuleReason::PackageSameName,
            ReasonData::None,
        );
        assert!(rule.is_multi_conflict);
        assert_eq!(rule.literals(), &[-3, -2, -1]);
    }

    #[test]
    fn test_hash_key() {
        let r1 = Rule::new(vec![2, 1], RuleReason::PackageRequires, ReasonData::None);
        let r2 = Rule::new(vec![1, 2], RuleReason::PackageConflict, ReasonData::None);
        assert_eq!(r1.hash_key(), r2.hash_key());
    }

    #[test]
    fn test_disable_enable() {
        let mut rule = Rule::new(vec![1, 2], RuleReason::PackageRequires, ReasonData::None);
        assert!(rule.is_enabled());
        rule.disable();
        assert!(rule.is_disabled());
        rule.enable();
        assert!(rule.is_enabled());
    }
}
