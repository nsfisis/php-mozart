use crate::pool::{Pool, PoolLink, PoolPackageInput};
use std::collections::{HashSet, VecDeque};

/// Builder for constructing a Pool from package metadata.
///
/// The builder accepts package inputs and recursively discovers
/// transitive dependencies. This is done by the registry layer
/// before solving.
pub struct PoolBuilder {
    /// Packages to add to the pool.
    inputs: Vec<PoolPackageInput>,
    /// Names already added (to avoid duplicates).
    added: HashSet<String>,
    /// Queue of package names that need to be explored.
    pending_names: VecDeque<String>,
    /// Package names that have already been explored (returned by next_pending).
    explored_names: HashSet<String>,
    /// Platform packages to ignore.
    ignore_platform_reqs: HashSet<String>,
}

impl PoolBuilder {
    pub fn new() -> Self {
        PoolBuilder {
            inputs: Vec::new(),
            added: HashSet::new(),
            pending_names: VecDeque::new(),
            explored_names: HashSet::new(),
            ignore_platform_reqs: HashSet::new(),
        }
    }

    /// Set platform requirements to ignore during exploration.
    pub fn set_ignore_platform_reqs(&mut self, names: HashSet<String>) {
        self.ignore_platform_reqs = names;
    }

    /// Add a package version to the builder. Returns true if it's new.
    pub fn add_package(&mut self, input: PoolPackageInput) -> bool {
        let key = format!("{}@{}", input.name, input.version);
        if self.added.contains(&key) {
            return false;
        }
        self.added.insert(key);

        // Queue dependency names for exploration
        for link in &input.requires {
            if !self.ignore_platform_reqs.contains(&link.target) {
                self.pending_names.push_back(link.target.clone());
            }
        }

        self.inputs.push(input);
        true
    }

    /// Get the next package name that needs to be explored.
    /// The caller should fetch available versions for this package
    /// and add them via `add_package`.
    pub fn next_pending(&mut self) -> Option<String> {
        while let Some(name) = self.pending_names.pop_front() {
            // Skip if already explored or already has versions in inputs
            if self.explored_names.contains(&name) {
                continue;
            }
            if self.inputs.iter().any(|p| p.name == name) {
                continue;
            }
            self.explored_names.insert(name.clone());
            return Some(name);
        }
        None
    }

    /// Check if there are more names to explore.
    pub fn has_pending(&self) -> bool {
        !self.pending_names.is_empty()
    }

    /// Build the final Pool.
    pub fn build(self) -> Pool {
        Pool::new(self.inputs, vec![])
    }

    /// Get the number of packages added so far.
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether the builder has no packages.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}

impl Default for PoolBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to convert (name, constraint) pairs from Packagist into PoolLinks.
pub fn make_pool_links(source: &str, deps: &[(String, String)]) -> Vec<PoolLink> {
    deps.iter()
        .map(|(target, constraint)| PoolLink {
            target: target.clone(),
            constraint: constraint.clone(),
            source: source.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_builder_basic() {
        let mut builder = PoolBuilder::new();

        builder.add_package(PoolPackageInput {
            name: "a/a".to_string(),
            version: "1.0.0.0".to_string(),
            pretty_version: "1.0.0".to_string(),
            requires: vec![PoolLink {
                target: "b/b".to_string(),
                constraint: "^1.0".to_string(),
                source: "a/a".to_string(),
            }],
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        });

        // Should have b/b pending
        let pending = builder.next_pending();
        assert_eq!(pending, Some("b/b".to_string()));

        builder.add_package(PoolPackageInput {
            name: "b/b".to_string(),
            version: "1.0.0.0".to_string(),
            pretty_version: "1.0.0".to_string(),
            requires: vec![],
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        });

        // No more pending
        assert!(builder.next_pending().is_none());

        let pool = builder.build();
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn test_deduplication() {
        let mut builder = PoolBuilder::new();

        let input = PoolPackageInput {
            name: "a/a".to_string(),
            version: "1.0.0.0".to_string(),
            pretty_version: "1.0.0".to_string(),
            requires: vec![],
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        };

        assert!(builder.add_package(input.clone()));
        assert!(!builder.add_package(input));
        assert_eq!(builder.len(), 1);
    }
}
