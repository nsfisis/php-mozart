//! [`Repository`] backed by the live Packagist HTTP API.
//!
//! Wraps the existing [`crate::packagist::fetch_package_versions`] so the
//! resolver sees the same data either through this trait or via the legacy
//! direct call. Construction takes ownership of the [`Cache`] handle so
//! callers no longer thread it through `ResolveRequest` / `LockFileGenerationRequest`.

use super::super::cache::Cache;
use super::super::packagist;
use super::super::packagist::SearchResult;
use super::{LoadResult, NamedPackagistVersion, PackageQuery, Repository, SearchMode};

pub struct PackagistRepository {
    id: String,
    cache: Cache,
}

impl PackagistRepository {
    pub fn new(cache: Cache) -> Self {
        Self {
            id: "packagist.org".to_string(),
            cache,
        }
    }
}

#[async_trait::async_trait]
impl Repository for PackagistRepository {
    fn id(&self) -> &str {
        &self.id
    }

    async fn load_packages(&self, queries: &[PackageQuery<'_>]) -> anyhow::Result<LoadResult> {
        let mut result = LoadResult::default();
        for query in queries {
            // Errors propagate to the caller. Composer's
            // `ComposerRepository::loadAsyncPackages` distinguishes 404
            // (empty result, no error) from transport failures (exception);
            // Mozart's underlying `fetch_package_versions` doesn't yet make
            // that distinction, so for now both surface as `Err` and the
            // caller decides whether the loop wants to continue (transitive
            // exploration) or abort (seed-time fetch failure).
            let versions = packagist::fetch_package_versions(query.name, &self.cache).await?;
            // A successful fetch counts as "this repo authoritatively knows
            // the name", even if the version list is empty — mirrors
            // Composer's `ArrayRepository::loadPackages` which adds the
            // name to `namesFound` regardless of constraint match.
            result.names_found.push(query.name.to_string());
            for version in versions {
                result.packages.push(NamedPackagistVersion {
                    name: query.name.to_string(),
                    version,
                });
            }
        }
        Ok(result)
    }

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        package_type: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        match mode {
            SearchMode::Fulltext => {
                let (results, _total) = packagist::search_packages(query, package_type).await?;
                Ok(results)
            }
            SearchMode::Name => {
                let pattern = build_name_regex(query)?;
                let names = packagist::fetch_package_names(package_type, &self.cache).await?;
                Ok(names
                    .into_iter()
                    .filter(|name| pattern.is_match(name))
                    .map(empty_search_result)
                    .collect())
            }
            SearchMode::Vendor => {
                let pattern = build_name_regex(query)?;
                let vendors = packagist::fetch_vendor_names(&self.cache).await?;
                Ok(vendors
                    .into_iter()
                    .filter(|name| pattern.is_match(name))
                    .map(empty_search_result)
                    .collect())
            }
        }
    }
}

/// Build the case-insensitive `(?:t1|t2|...)` regex from whitespace-split
/// tokens, mirroring Composer's `'{(?:'.implode('|', $matches).')}i'`.
///
/// Tokens are joined as-is — callers are expected to have already escaped
/// regex metacharacters (`SearchCommand` calls `preg_quote`; Mozart calls
/// `regex::escape` before reaching this point).
fn build_name_regex(query: &str) -> anyhow::Result<regex::Regex> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let body = if tokens.is_empty() {
        String::new()
    } else {
        tokens.join("|")
    };
    Ok(regex::Regex::new(&format!("(?i)(?:{body})"))?)
}

/// Build a [`SearchResult`] with only `name` populated, mirroring the shape
/// Composer returns for `SEARCH_NAME` / `SEARCH_VENDOR` modes
/// (`['name' => $name]`, all other fields `null`).
fn empty_search_result(name: String) -> SearchResult {
    SearchResult {
        name,
        description: String::new(),
        url: String::new(),
        repository: None,
        downloads: 0,
        favers: 0,
        abandoned: None,
    }
}
