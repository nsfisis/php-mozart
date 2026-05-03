use crate::pool::{Literal, Pool};
use indexmap::IndexMap;

/// Version selection policy: decides which version to prefer when multiple
/// candidates satisfy a requirement.
///
/// Port of Composer's DefaultPolicy.php.
pub struct DefaultPolicy {
    /// Whether to prefer stable versions.
    pub prefer_stable: bool,
    /// Whether to prefer lowest versions.
    pub prefer_lowest: bool,
    /// `name → normalized version` overrides used when more than one
    /// candidate could satisfy a requirement: a literal pinned at the
    /// preferred version wins outright over the usual highest/lowest pick.
    /// Mirrors Composer's `DefaultPolicy::pruneToBestVersion` behavior under
    /// `--minimal-changes`, where the lock's previously-installed versions
    /// are passed in so the solver only moves a package when a constraint
    /// actually forces a different version.
    pub preferred_versions: Option<IndexMap<String, String>>,
}

impl DefaultPolicy {
    pub fn new(prefer_stable: bool, prefer_lowest: bool) -> Self {
        DefaultPolicy {
            prefer_stable,
            prefer_lowest,
            preferred_versions: None,
        }
    }

    pub fn with_preferred(
        prefer_stable: bool,
        prefer_lowest: bool,
        preferred_versions: IndexMap<String, String>,
    ) -> Self {
        DefaultPolicy {
            prefer_stable,
            prefer_lowest,
            preferred_versions: Some(preferred_versions),
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
        let mut groups: IndexMap<&str, Vec<Literal>> = IndexMap::new();
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

        // If same name, apply Composer's policy ordering. Mirrors
        // `DefaultPolicy::versionCompare`: when `prefer_stable` is on and
        // the two candidates have different stabilities, the more-stable
        // one wins outright — `prefer_lowest` only kicks in within the same
        // stability tier. Otherwise sort by version (asc for prefer_lowest,
        // desc otherwise).
        if pkg_a.name == pkg_b.name {
            if self.prefer_stable {
                let stab_a = stability_priority(&pkg_a.version);
                let stab_b = stability_priority(&pkg_b.version);
                if stab_a != stab_b {
                    return stab_a.cmp(&stab_b);
                }
            }
            let cmp = self.compare_versions(&pkg_a.version, &pkg_b.version);
            return if self.prefer_lowest {
                cmp
            } else {
                cmp.reverse()
            };
        }

        // Different names: when one package replaces the other, prefer the
        // *replaced* original. Mirrors the `replaces()` shortcut in
        // Composer's `DefaultPolicy::compareByPriority` (the cross-package
        // `ignoreReplace=false` pass). Without this, a request like
        // `update a/installed` where the pool also contains an
        // `a/replacer` declaring `replace: { "a/installed": "dev-master" }`
        // could fall through to package-id tie-break and land on the
        // replacer instead of the package the user actually asked for.
        if pkg_a.replaces.iter().any(|link| link.target == pkg_b.name) {
            return std::cmp::Ordering::Greater;
        }
        if pkg_b.replaces.iter().any(|link| link.target == pkg_a.name) {
            return std::cmp::Ordering::Less;
        }

        // Different names, no replace relationship: sort by package ID
        // for reproducibility.
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

        // Mirror Composer's `DefaultPolicy::pruneToBestVersion` short-circuit:
        // when a preferred version is set for this package and one of the
        // candidates matches it exactly, that wins over the regular
        // highest/lowest pick. Falls through otherwise (e.g. the locked
        // version no longer satisfies the constraint and was filtered out
        // before reaching this method).
        if let Some(ref preferred) = self.preferred_versions {
            let name = pool.literal_to_package(literals[0]).name.clone();
            if let Some(preferred_ver) = preferred.get(&name) {
                let preferred_lits: Vec<Literal> = literals
                    .iter()
                    .filter(|&&lit| pool.literal_to_package(lit).version == *preferred_ver)
                    .copied()
                    .collect();
                if !preferred_lits.is_empty() {
                    return preferred_lits;
                }
            }
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

/// Map a normalized version string to Composer's stability priority
/// (`BasePackage::STABILITIES`). Lower = more stable. Stable=0, RC=5, beta=10,
/// alpha=15, dev=20. Mirrors `DefaultPolicy::versionCompare`'s comparison
/// when `prefer_stable` is set.
fn stability_priority(version: &str) -> u8 {
    let Ok(v) = mozart_semver::Version::parse(version) else {
        return 0;
    };
    if v.is_dev_branch {
        return 20;
    }
    match v.pre_release.as_deref() {
        None => 0,
        Some(pre) => {
            let lower = pre.to_lowercase();
            if lower.starts_with("dev") {
                20
            } else if lower.starts_with("alpha") || lower == "a" {
                15
            } else if lower.starts_with("beta") || lower == "b" {
                10
            } else if lower.starts_with("rc") {
                5
            } else {
                // patch/pl/p / unknown → stable
                0
            }
        }
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
            is_alias_of: None,
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
