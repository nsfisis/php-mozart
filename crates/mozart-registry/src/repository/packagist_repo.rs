//! [`Repository`] backed by the live Packagist HTTP API.
//!
//! Wraps the existing [`crate::packagist::fetch_package_versions`] so the
//! resolver sees the same data either through this trait or via the legacy
//! direct call. Construction takes ownership of the [`Cache`] handle so
//! callers no longer thread it through `ResolveRequest` / `LockFileGenerationRequest`.

use super::{LoadResult, NamedPackagistVersion, PackageQuery, Repository};
use crate::cache::Cache;
use crate::packagist;

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
}
