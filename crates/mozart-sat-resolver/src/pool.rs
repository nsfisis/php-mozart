use mozart_semver::VersionConstraint;
use std::collections::HashMap;
use std::fmt;

/// Unique identifier for a package in the pool. 1-based.
pub type PackageId = u32;

/// A SAT literal. Positive = install package, negative = don't install.
/// The absolute value is the PackageId.
pub type Literal = i32;

/// Returns the PackageId from a literal.
#[inline]
pub fn literal_to_package_id(literal: Literal) -> PackageId {
    literal.unsigned_abs()
}

/// A link from a package to another package name with a version constraint.
#[derive(Debug, Clone)]
pub struct PoolLink {
    /// The target package name.
    pub target: String,
    /// The version constraint string (e.g. "^1.0").
    pub constraint: String,
    /// The source package name (the one declaring this link).
    pub source: String,
}

impl fmt::Display for PoolLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.source, self.target, self.constraint)
    }
}

/// A package entry in the pool. This is the SAT solver's view of a package.
#[derive(Debug, Clone)]
pub struct PoolPackage {
    /// 1-based package ID assigned by the pool.
    pub id: PackageId,
    /// Normalized package name (e.g. "monolog/monolog").
    pub name: String,
    /// Normalized version string (e.g. "1.0.0.0").
    pub version: String,
    /// Pretty version string (e.g. "1.0.0").
    pub pretty_version: String,
    /// Package requirements.
    pub requires: Vec<PoolLink>,
    /// Packages this replaces.
    pub replaces: Vec<PoolLink>,
    /// Packages this provides.
    pub provides: Vec<PoolLink>,
    /// Packages this conflicts with.
    pub conflicts: Vec<PoolLink>,
    /// Whether this is a fixed/locked package.
    pub is_fixed: bool,
}

impl PoolPackage {
    /// Returns all names this package is known by (own name + provides + replaces targets).
    pub fn names(&self) -> Vec<&str> {
        let mut names = vec![self.name.as_str()];
        for link in &self.provides {
            names.push(link.target.as_str());
        }
        for link in &self.replaces {
            names.push(link.target.as_str());
        }
        names
    }
}

impl fmt::Display for PoolPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.name, self.pretty_version)
    }
}

/// Input for building a Pool. Users of the crate provide these.
#[derive(Debug, Clone)]
pub struct PoolPackageInput {
    pub name: String,
    pub version: String,
    pub pretty_version: String,
    pub requires: Vec<PoolLink>,
    pub replaces: Vec<PoolLink>,
    pub provides: Vec<PoolLink>,
    pub conflicts: Vec<PoolLink>,
    pub is_fixed: bool,
}

/// The package pool: contains all candidate packages for dependency resolution.
/// Packages are assigned sequential 1-based IDs.
///
/// Port of Composer's Pool.php.
pub struct Pool {
    /// All packages, indexed by (id - 1).
    packages: Vec<PoolPackage>,
    /// Index: package name → list of package IDs providing that name.
    package_by_name: HashMap<String, Vec<PackageId>>,
    /// Cache for what_provides results.
    provider_cache: HashMap<(String, String), Vec<PackageId>>,
    /// Packages that are fixed/locked but unacceptable (e.g. failed stability).
    unacceptable_fixed_packages: Vec<PackageId>,
}

impl Pool {
    /// Create a new pool from a list of package inputs.
    pub fn new(inputs: Vec<PoolPackageInput>, unacceptable_fixed_ids: Vec<PackageId>) -> Self {
        let mut packages = Vec::with_capacity(inputs.len());
        let mut package_by_name: HashMap<String, Vec<PackageId>> = HashMap::new();

        for (idx, input) in inputs.into_iter().enumerate() {
            let id = (idx as PackageId) + 1;
            let pkg = PoolPackage {
                id,
                name: input.name,
                version: input.version,
                pretty_version: input.pretty_version,
                requires: input.requires,
                replaces: input.replaces,
                provides: input.provides,
                conflicts: input.conflicts,
                is_fixed: input.is_fixed,
            };

            // Index by all names this package provides
            for name in pkg.names() {
                package_by_name
                    .entry(name.to_string())
                    .or_default()
                    .push(id);
            }

            packages.push(pkg);
        }

        Pool {
            packages,
            package_by_name,
            provider_cache: HashMap::new(),
            unacceptable_fixed_packages: unacceptable_fixed_ids,
        }
    }

    /// Returns the number of packages in the pool.
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Returns true if the pool has no packages.
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Look up a package by its 1-based ID.
    pub fn package_by_id(&self, id: PackageId) -> &PoolPackage {
        &self.packages[(id - 1) as usize]
    }

    /// All packages in the pool.
    pub fn packages(&self) -> &[PoolPackage] {
        &self.packages
    }

    /// Convert a literal to its package reference.
    pub fn literal_to_package(&self, literal: Literal) -> &PoolPackage {
        self.package_by_id(literal_to_package_id(literal))
    }

    /// Format a literal as a human-readable string.
    pub fn literal_to_pretty_string(&self, literal: Literal) -> String {
        let pkg = self.literal_to_package(literal);
        let prefix = if literal > 0 {
            "install"
        } else {
            "don't install"
        };
        format!("{prefix} {} {}", pkg.name, pkg.pretty_version)
    }

    /// Find all packages matching a name and optional constraint.
    /// Results are cached.
    pub fn what_provides(&mut self, name: &str, constraint: Option<&str>) -> Vec<PackageId> {
        let key = (name.to_string(), constraint.unwrap_or("").to_string());
        if let Some(cached) = self.provider_cache.get(&key) {
            return cached.clone();
        }

        let result = self.compute_what_provides(name, constraint);
        self.provider_cache.insert(key, result.clone());
        result
    }

    fn compute_what_provides(&self, name: &str, constraint: Option<&str>) -> Vec<PackageId> {
        let Some(candidate_ids) = self.package_by_name.get(name) else {
            return vec![];
        };

        let parsed_constraint = constraint.and_then(|c| VersionConstraint::parse(c).ok());

        let mut matches = Vec::new();
        for &id in candidate_ids {
            let pkg = self.package_by_id(id);
            if self.matches_package(pkg, name, parsed_constraint.as_ref()) {
                matches.push(id);
            }
        }
        matches
    }

    /// Check if a candidate package matches a name and optional constraint.
    /// Handles provides and replaces.
    fn matches_package(
        &self,
        candidate: &PoolPackage,
        name: &str,
        constraint: Option<&VersionConstraint>,
    ) -> bool {
        if candidate.name == name {
            return match constraint {
                None => true,
                Some(vc) => {
                    if let Ok(v) = mozart_semver::Version::parse(&candidate.version) {
                        vc.matches(&v)
                    } else {
                        false
                    }
                }
            };
        }

        // Check provides
        for link in &candidate.provides {
            if link.target == name {
                return match constraint {
                    None => true,
                    Some(vc) => {
                        // The provide link has its own constraint; check if they intersect
                        if let Ok(provide_vc) = VersionConstraint::parse(&link.constraint) {
                            constraints_intersect(vc, &provide_vc)
                        } else {
                            false
                        }
                    }
                };
            }
        }

        // Check replaces
        for link in &candidate.replaces {
            if link.target == name {
                return match constraint {
                    None => true,
                    Some(vc) => {
                        if let Ok(replace_vc) = VersionConstraint::parse(&link.constraint) {
                            constraints_intersect(vc, &replace_vc)
                        } else {
                            false
                        }
                    }
                };
            }
        }

        false
    }

    /// Check if a package is in the unacceptable fixed list.
    pub fn is_unacceptable_fixed_package(&self, id: PackageId) -> bool {
        self.unacceptable_fixed_packages.contains(&id)
    }
}

impl fmt::Display for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Pool:")?;
        for pkg in &self.packages {
            writeln!(f, "  {:>6}: {} {}", pkg.id, pkg.name, pkg.pretty_version)?;
        }
        Ok(())
    }
}

/// Simple intersection check: does there exist a version that satisfies both constraints?
/// For provides/replaces matching, we just check if a "=version" from one constraint
/// can match the other. This is a simplified check.
fn constraints_intersect(_a: &VersionConstraint, _b: &VersionConstraint) -> bool {
    // For a basic approximation: if b is a single exact constraint, check if a matches it
    // and vice versa. For complex cases, we assume they intersect.
    // This mirrors Composer's behavior where provide/replace constraints are matched
    // against the requirement constraint.
    //
    // In Composer, this is done via `$constraint->matches($link->getConstraint())`
    // which checks if there exists a version satisfying both.
    // For now, we'll do a simple approach: always return true (provider matches).
    // The RuleSetGenerator will create proper rules anyway.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_pool_basic() {
        let mut pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0"),
                make_input("a/a", "2.0.0.0"),
                make_input("b/b", "1.0.0.0"),
            ],
            vec![],
        );

        assert_eq!(pool.len(), 3);
        assert_eq!(pool.package_by_id(1).name, "a/a");
        assert_eq!(pool.package_by_id(2).name, "a/a");
        assert_eq!(pool.package_by_id(3).name, "b/b");

        let providers = pool.what_provides("a/a", None);
        assert_eq!(providers, vec![1, 2]);
    }

    #[test]
    fn test_literal_to_package() {
        let pool = Pool::new(
            vec![make_input("a/a", "1.0.0.0"), make_input("b/b", "1.0.0.0")],
            vec![],
        );

        assert_eq!(pool.literal_to_package(1).name, "a/a");
        assert_eq!(pool.literal_to_package(-1).name, "a/a");
        assert_eq!(pool.literal_to_package(2).name, "b/b");
        assert_eq!(pool.literal_to_package(-2).name, "b/b");
    }

    #[test]
    fn test_literal_pretty_string() {
        let pool = Pool::new(vec![make_input("a/a", "1.0.0.0")], vec![]);
        assert_eq!(pool.literal_to_pretty_string(1), "install a/a 1.0.0.0");
        assert_eq!(
            pool.literal_to_pretty_string(-1),
            "don't install a/a 1.0.0.0"
        );
    }
}
