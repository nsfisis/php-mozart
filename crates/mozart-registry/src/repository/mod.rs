//! Repository abstraction over package metadata sources.
//!
//! Mirrors Composer's `Composer\Repository\RepositoryInterface::loadPackages`
//! and `Composer\Repository\RepositoryManager`. The resolver and lockfile
//! generator query a [`RepositorySet`] instead of calling Packagist directly,
//! so test code can substitute a set without `PackagistRepository` (mirroring
//! Composer's `FactoryMock` injecting `repositories: ['packagist' => false]`).
//!
//! Concrete implementations live in sibling modules: [`packagist_repo`] for
//! the live Packagist HTTP repo, [`inline_package_repo`] for `type: package`
//! entries embedded in `composer.json`, and [`vcs_repo`] for VCS repositories.

use crate::packagist::PackagistVersion;

pub mod inline_package_repo;
pub mod packagist_repo;
pub mod vcs_repo;

/// One name-keyed lookup against a repository.
///
/// Matches the `$packageNameMap` argument of Composer's `loadPackages`. The
/// constraint is informational — repositories may use it to skip versions
/// that obviously can't match (an optimization), but the resolver still
/// re-checks every returned version when generating rules.
#[derive(Debug, Clone)]
pub struct PackageQuery<'a> {
    pub name: &'a str,
    /// Raw constraint string from `composer.json`, e.g. `"^1.2"`. `None`
    /// when the caller wants every version (transitive exploration).
    pub constraint: Option<&'a str>,
}

/// Result of a single [`Repository::load_packages`] call.
///
/// Mirrors Composer's `['packages' => ..., 'namesFound' => ...]` tuple.
/// `names_found` lets [`RepositorySet`] short-circuit lower-priority repos
/// once an upstream repo has authoritatively answered for a name (Composer's
/// "first repo wins" semantics).
#[derive(Debug, Default)]
pub struct LoadResult {
    pub packages: Vec<NamedPackagistVersion>,
    pub names_found: Vec<String>,
}

/// A `PackagistVersion` paired with the canonical package name it answers
/// for. Inline `type: package` repos can return packages whose own `name`
/// field differs from the queried name when they declare `replace`/`provide`,
/// so callers need both.
#[derive(Debug, Clone)]
pub struct NamedPackagistVersion {
    pub name: String,
    pub version: PackagistVersion,
}

/// A source of package metadata. Mirrors Composer's `RepositoryInterface`.
///
/// Implementations should return an empty [`LoadResult`] (not an error) when
/// they simply don't know a queried name — [`RepositorySet`] uses that to
/// fall through to the next repo. Reserve `Err` for genuine I/O failures
/// the caller cannot route around.
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    /// Identifier for diagnostics (`"packagist.org"`, `"package"`, `"vcs:<url>"`).
    fn id(&self) -> &str;

    /// Look up every version of every queried name this repo knows about.
    async fn load_packages(&self, queries: &[PackageQuery<'_>]) -> anyhow::Result<LoadResult>;
}

/// Ordered list of repositories. Mirrors `Composer\Repository\RepositoryManager`.
///
/// `load_packages` queries each repo in order. Once a repo authoritatively
/// answers for a name (i.e. lists it in `names_found`), later repos are not
/// asked about that name — matching Composer's first-repo-wins priority.
pub struct RepositorySet {
    repos: Vec<Box<dyn Repository>>,
}

impl RepositorySet {
    pub fn new(repos: Vec<Box<dyn Repository>>) -> Self {
        Self { repos }
    }

    /// Production default: a single [`packagist_repo::PackagistRepository`]
    /// backed by the given on-disk cache. Mirrors what Composer does when
    /// no `'packagist' => false` entry appears in the merged config.
    pub fn with_packagist(repo_cache: crate::cache::Cache) -> Self {
        Self::new(vec![Box::new(packagist_repo::PackagistRepository::new(
            repo_cache,
        ))])
    }

    /// An empty set. Mirrors Composer's `'packagist' => false` test config:
    /// resolution proceeds entirely from packages already in the pool
    /// (eager VCS scan, inline `type: package` repos, the locked repository).
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    pub fn len(&self) -> usize {
        self.repos.len()
    }

    /// Iterate over repositories in priority order.
    pub fn repos(&self) -> impl Iterator<Item = &dyn Repository> {
        self.repos.iter().map(|b| b.as_ref())
    }

    /// Query every repo, accumulating packages and tracking which names have
    /// been authoritatively answered. Names already covered by an earlier
    /// repo are dropped from the query passed to later repos.
    pub async fn load_packages(
        &self,
        queries: &[PackageQuery<'_>],
    ) -> anyhow::Result<Vec<NamedPackagistVersion>> {
        use indexmap::IndexSet;

        let mut packages: Vec<NamedPackagistVersion> = Vec::new();
        let mut answered: IndexSet<String> = IndexSet::new();

        for repo in &self.repos {
            let pending: Vec<PackageQuery<'_>> = queries
                .iter()
                .filter(|q| !answered.contains(q.name))
                .cloned()
                .collect();
            if pending.is_empty() {
                break;
            }
            let result = repo.load_packages(&pending).await?;
            for name in result.names_found {
                answered.insert(name);
            }
            packages.extend(result.packages);
        }

        Ok(packages)
    }
}
