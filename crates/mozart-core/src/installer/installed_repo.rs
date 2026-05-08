//! Lightweight stand-in for `Composer\Repository\InstalledRepository`.
//!
//! Composer's `InstalledRepository` is a composite over `LockArrayRepository`,
//! `InstalledRepositoryInterface`, `RootPackageRepository`, and
//! `PlatformRepository`. Mozart does not (yet) expose a unified repository
//! abstraction, so this struct is the smallest layer we need to support the
//! handful of commands that drive their behavior off
//! `findPackagesWithReplacersAndProviders` (currently `check-platform-reqs`
//! and `suggests`; later candidates: `depends`/`prohibits`, `audit`).
//!
//! The struct serves two roles:
//!
//! - As a lower-cased name set: callers `insert(name)` whatever they want
//!   visible to `contains` / suggestion-filter logic.
//! - As a candidate index: callers `add_candidate(InstalledCandidate)` and
//!   then resolve a require name to the candidate(s) that satisfy it directly
//!   or through a `provide` / `replace` link.

use indexmap::IndexSet;
use std::collections::BTreeMap;

/// One installed package, in the shape `findPackagesWithReplacersAndProviders`
/// needs. Mirrors the fields of `Composer\Package\PackageInterface` that the
/// PHP implementation reads — name, version, provides, replaces.
#[derive(Debug, Clone)]
pub struct InstalledCandidate {
    /// Lower-cased package name, used for matching.
    pub name: String,
    /// Original-case package name, used in user-facing output.
    pub pretty_name: String,
    /// Normalized version (what the constraint matcher consumes).
    pub version: String,
    /// Original-case version, used in user-facing output.
    pub pretty_version: String,
    /// `provide` map: target package name → constraint string.
    pub provides: BTreeMap<String, String>,
    /// `replace` map: target package name → constraint string.
    pub replaces: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct InstalledRepoLite {
    /// Lower-cased names of every package, plus every `provide`/`replace`
    /// target that any candidate exposes. `contains` queries this set.
    pub names: IndexSet<String>,
    /// Full candidate records, in insertion order.
    pub candidates: Vec<InstalledCandidate>,
}

impl InstalledRepoLite {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: &str) {
        self.names.insert(name.to_lowercase());
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(&name.to_lowercase())
    }

    /// Add a full candidate record. Also inserts the candidate's own name and
    /// every `provide` / `replace` target into the names set so `contains`
    /// keeps reflecting all installed virtuals.
    pub fn add_candidate(&mut self, candidate: InstalledCandidate) {
        self.names.insert(candidate.name.clone());
        for target in candidate.provides.keys().chain(candidate.replaces.keys()) {
            self.names.insert(target.to_lowercase());
        }
        self.candidates.push(candidate);
    }

    /// Mirrors `Composer\Repository\InstalledRepository::findPackagesWithReplacersAndProviders`
    /// without the optional constraint filter — callers in
    /// `check-platform-reqs` apply their own per-link constraint check after
    /// they have the candidate list. Returns each candidate at most once.
    pub fn find_with_replacers_and_providers(&self, require: &str) -> Vec<&InstalledCandidate> {
        let needle = require.to_lowercase();
        let mut matches: Vec<&InstalledCandidate> = Vec::new();
        for candidate in &self.candidates {
            if candidate.name == needle {
                matches.push(candidate);
                continue;
            }
            let provides_or_replaces = candidate
                .provides
                .keys()
                .chain(candidate.replaces.keys())
                .any(|target| target.to_lowercase() == needle);
            if provides_or_replaces {
                matches.push(candidate);
            }
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(name: &str, version: &str) -> InstalledCandidate {
        InstalledCandidate {
            name: name.to_lowercase(),
            pretty_name: name.to_string(),
            version: version.to_string(),
            pretty_version: version.to_string(),
            provides: BTreeMap::new(),
            replaces: BTreeMap::new(),
        }
    }

    #[test]
    fn insert_and_contains_lowercase() {
        let mut repo = InstalledRepoLite::new();
        repo.insert("Vendor/Pkg");
        assert!(repo.contains("vendor/pkg"));
        assert!(repo.contains("VENDOR/PKG"));
    }

    #[test]
    fn add_candidate_registers_name_and_virtuals() {
        let mut c = make_candidate("vendor/poly", "1.0.0");
        c.provides.insert("ext-mbstring".into(), "1.0".into());
        c.replaces.insert("ext-iconv".into(), "*".into());

        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(c);

        assert!(repo.contains("vendor/poly"));
        assert!(repo.contains("ext-mbstring"));
        assert!(repo.contains("ext-iconv"));
    }

    #[test]
    fn find_returns_direct_match() {
        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(make_candidate("php", "8.2.1"));
        let hits = repo.find_with_replacers_and_providers("php");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "php");
    }

    #[test]
    fn find_returns_provider() {
        let mut c = make_candidate("symfony/polyfill-mbstring", "1.30.0");
        c.provides.insert("ext-mbstring".into(), "*".into());

        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(c);

        let hits = repo.find_with_replacers_and_providers("ext-mbstring");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "symfony/polyfill-mbstring");
    }

    #[test]
    fn find_returns_replacer() {
        let mut c = make_candidate("vendor/forklift", "2.0.0");
        c.replaces.insert("vendor/legacy".into(), "1.*".into());

        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(c);

        let hits = repo.find_with_replacers_and_providers("vendor/legacy");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "vendor/forklift");
    }

    #[test]
    fn find_returns_empty_when_unknown() {
        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(make_candidate("php", "8.2.1"));
        assert!(
            repo.find_with_replacers_and_providers("ext-foobar")
                .is_empty()
        );
    }

    #[test]
    fn find_includes_each_candidate_at_most_once() {
        let mut c = make_candidate("vendor/poly", "1.0.0");
        // Same target listed in both maps — should still only return one hit.
        c.provides.insert("ext-x".into(), "*".into());
        c.replaces.insert("ext-x".into(), "*".into());

        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(c);

        let hits = repo.find_with_replacers_and_providers("ext-x");
        assert_eq!(hits.len(), 1);
    }
}
