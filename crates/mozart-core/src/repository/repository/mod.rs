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

use std::collections::BTreeMap;

use super::advisory::{MatchedAdvisory, PackageInfo};
use super::packagist::{PackagistVersion, SearchResult};

pub mod inline_package_repo;
pub mod packagist_repo;
pub mod vcs_repo;

/// Search modes for [`Repository::search`].
///
/// Mirrors Composer's `RepositoryInterface::SEARCH_FULLTEXT|SEARCH_NAME|SEARCH_VENDOR`
/// constants (`composer/src/Composer/Repository/RepositoryInterface.php`).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum SearchMode {
    /// Full-text search over name, description, and keywords (Packagist's
    /// `search.json` API).
    Fulltext,
    /// Match the regex against package names. Tokens are split on whitespace
    /// and joined as `(?:t1|t2|...)`; callers must pre-quote regex metachars.
    Name,
    /// Match the regex against vendor names. Result rows have only `name`
    /// populated (the vendor part).
    Vendor,
}

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

    /// Search this repository.
    ///
    /// The default returns an empty result so repositories that don't
    /// participate in search (e.g. inline / VCS repos that only resolve
    /// known names) can opt out. Mirrors Composer's
    /// `RepositoryInterface::search` whose default behavior on
    /// `ArrayRepository` walks the in-memory list.
    async fn search(
        &self,
        _query: &str,
        _mode: SearchMode,
        _package_type: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        Ok(Vec::new())
    }
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
    pub fn with_packagist(repo_cache: super::cache::Cache) -> Self {
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

    /// Fan-out search across every repository, concatenating results in
    /// priority order. Mirrors Composer's
    /// `CompositeRepository::search` which `array_merge`s per-repo results
    /// without de-duplication.
    pub async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        package_type: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut all = Vec::new();
        for repo in &self.repos {
            let mut hits = repo.search(query, mode, package_type).await?;
            all.append(&mut hits);
        }
        Ok(all)
    }

    /// Fetch security advisories matching the installed packages, with version filtering.
    ///
    /// Mirrors `Composer\Repository\RepositorySet::getMatchingSecurityAdvisories()`.
    /// Returns the matched advisories (already filtered by installed version) and a list
    /// of unreachable repository URLs. When `ignore_unreachable` is false and a repository
    /// is unreachable, the error is propagated instead.
    pub async fn get_matching_security_advisories(
        &self,
        packages: &[PackageInfo],
        _allow_partial: bool,
        ignore_unreachable: bool,
    ) -> anyhow::Result<(BTreeMap<String, Vec<MatchedAdvisory>>, Vec<String>)> {
        let names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();

        let (raw_advisories, unreachable_repos) =
            match super::packagist::fetch_security_advisories(&names).await {
                Ok(a) => (a, vec![]),
                Err(e) if ignore_unreachable => {
                    tracing::warn!("Packagist advisory fetch failed (ignored): {e}");
                    let unreachable = vec!["https://packagist.org".to_string()];
                    (BTreeMap::new(), unreachable)
                }
                Err(e) => return Err(e),
            };

        let matched = version_filter_advisories(&raw_advisories, packages);

        Ok((matched, unreachable_repos))
    }
}

/// Normalize single-pipe OR separators (`|`) in a version constraint string to
/// double-pipe (`||`) so the constraint parser can handle both forms.
///
/// The Packagist security advisories API may return constraints with single `|`
/// as the OR separator (e.g. `>=1.0,<1.5|>=2.0,<2.3`), but Mozart's
/// `VersionConstraint::parse` expects `||`.
///
/// TODO: fix `mozart_semver::VersionConstraint::parse` to accept single `|` and remove this.
fn normalize_or_separator(constraint: &str) -> String {
    let bytes = constraint.as_bytes();
    let mut result = String::with_capacity(constraint.len() + 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                result.push_str("||");
                i += 2;
            } else {
                result.push_str("||");
                i += 1;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// Filter raw advisories by installed package versions.
///
/// Mirrors the version-matching step inside Composer's repository advisory fetch.
fn version_filter_advisories(
    all_advisories: &BTreeMap<String, Vec<super::packagist::SecurityAdvisory>>,
    packages: &[PackageInfo],
) -> BTreeMap<String, Vec<MatchedAdvisory>> {
    let mut result: BTreeMap<String, Vec<MatchedAdvisory>> = BTreeMap::new();

    for pkg in packages {
        let Some(advisories) = all_advisories.get(&pkg.name) else {
            continue;
        };

        let version_str = pkg
            .version_normalized
            .as_deref()
            .unwrap_or(pkg.version.as_str());

        let installed_ver = match mozart_semver::Version::parse(version_str) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    "Could not parse version {:?} for package {:?}, skipping advisory matching",
                    version_str,
                    pkg.name
                );
                continue;
            }
        };

        let mut matched: Vec<MatchedAdvisory> = Vec::new();

        for advisory in advisories {
            let normalized = normalize_or_separator(&advisory.affected_versions);
            let constraint = match mozart_semver::VersionConstraint::parse(&normalized) {
                Ok(c) => c,
                Err(_) => {
                    tracing::warn!(
                        "Could not parse affected versions {:?} for advisory {:?}, skipping",
                        advisory.affected_versions,
                        advisory.advisory_id
                    );
                    continue;
                }
            };

            if constraint.matches(&installed_ver) {
                matched.push(MatchedAdvisory {
                    advisory: advisory.clone(),
                    installed_version: pkg.version.clone(),
                });
            }
        }

        if !matched.is_empty() {
            result.insert(pkg.name.clone(), matched);
        }
    }

    result
}
