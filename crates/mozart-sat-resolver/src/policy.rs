use crate::pool::{Literal, Pool};
use std::collections::HashMap;

/// Version selection policy: decides which version to prefer when multiple
/// candidates satisfy a requirement.
///
/// Port of Composer's DefaultPolicy.php.
pub struct DefaultPolicy {
    /// Whether to prefer stable versions.
    pub prefer_stable: bool,
    /// Whether to prefer lowest versions.
    pub prefer_lowest: bool,
}

impl DefaultPolicy {
    pub fn new(prefer_stable: bool, prefer_lowest: bool) -> Self {
        DefaultPolicy {
            prefer_stable,
            prefer_lowest,
        }
    }

    /// Select preferred packages from a list of candidate literals.
    /// Returns the literals sorted by preference (most preferred first).
    ///
    /// Port of Composer's DefaultPolicy::selectPreferredPackages.
    pub fn select_preferred_packages(
        &self,
        pool: &Pool,
        literals: &[Literal],
        _required_package: Option<&str>,
    ) -> Vec<Literal> {
        if literals.is_empty() {
            return vec![];
        }

        // Group literals by package name
        let mut groups: HashMap<&str, Vec<Literal>> = HashMap::new();
        for &lit in literals {
            let pkg = pool.literal_to_package(lit);
            groups.entry(pkg.name.as_str()).or_default().push(lit);
        }

        // Sort each group by version preference
        for lits in groups.values_mut() {
            lits.sort_by(|&a, &b| self.compare_by_priority(pool, a, b));
        }

        // Prune to best version within each group
        for lits in groups.values_mut() {
            *lits = self.prune_to_best_version(pool, lits);
        }

        // Merge and sort across all packages
        let mut selected: Vec<Literal> = groups.into_values().flatten().collect();
        selected.sort_by(|&a, &b| self.compare_by_priority(pool, a, b));

        selected
    }

    /// Compare two package literals by priority.
    /// Returns Ordering: negative means a is preferred.
    fn compare_by_priority(&self, pool: &Pool, a: Literal, b: Literal) -> std::cmp::Ordering {
        let pkg_a = pool.literal_to_package(a);
        let pkg_b = pool.literal_to_package(b);

        // If same name, prefer higher version (or lower if prefer_lowest)
        if pkg_a.name == pkg_b.name {
            let cmp = self.compare_versions(&pkg_a.version, &pkg_b.version);
            return if self.prefer_lowest {
                cmp
            } else {
                cmp.reverse()
            };
        }

        // Different names: sort by package ID for reproducibility
        pkg_a.id.cmp(&pkg_b.id)
    }

    /// Compare two normalized version strings.
    fn compare_versions(&self, a: &str, b: &str) -> std::cmp::Ordering {
        match (
            mozart_semver::Version::parse(a),
            mozart_semver::Version::parse(b),
        ) {
            (Ok(va), Ok(vb)) => va.cmp(&vb),
            _ => a.cmp(b),
        }
    }

    /// Prune to the best version among a sorted list of literals for the same package.
    fn prune_to_best_version(&self, pool: &Pool, literals: &[Literal]) -> Vec<Literal> {
        if literals.is_empty() {
            return vec![];
        }

        // The first literal is the best after sorting
        let best_version = &pool.literal_to_package(literals[0]).version;
        literals
            .iter()
            .filter(|&&lit| pool.literal_to_package(lit).version == *best_version)
            .copied()
            .collect()
    }
}

impl Default for DefaultPolicy {
    fn default() -> Self {
        DefaultPolicy::new(false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::PoolPackageInput;

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
    fn test_prefer_highest() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0"),
                make_input("a/a", "2.0.0.0"),
                make_input("a/a", "3.0.0.0"),
            ],
            vec![],
        );
        let policy = DefaultPolicy::new(false, false);
        let result = policy.select_preferred_packages(&pool, &[1, 2, 3], None);
        // Should prefer highest version (3.0.0.0 = id 3)
        assert_eq!(result[0], 3);
    }

    #[test]
    fn test_prefer_lowest() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0"),
                make_input("a/a", "2.0.0.0"),
                make_input("a/a", "3.0.0.0"),
            ],
            vec![],
        );
        let policy = DefaultPolicy::new(false, true);
        let result = policy.select_preferred_packages(&pool, &[1, 2, 3], None);
        // Should prefer lowest version (1.0.0.0 = id 1)
        assert_eq!(result[0], 1);
    }
}
