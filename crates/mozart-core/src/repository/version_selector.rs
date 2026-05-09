use super::super::package::Stability;
use super::cache::Cache;
use super::packagist::{self, PackagistVersion};
use super::version;

/// Mirrors `Composer\Package\Version\VersionSelector`.
pub struct VersionSelector {
    preferred_stability: Stability,
    repo_cache: Cache,
}

impl VersionSelector {
    pub fn new(preferred_stability: Stability, repo_cache: Cache) -> Self {
        Self {
            preferred_stability,
            repo_cache,
        }
    }

    /// Fetch versions from Packagist and pick the best candidate.
    /// Mirrors `VersionSelector::findBestCandidate()`.
    pub async fn find_best_candidate(
        &self,
        package_name: &str,
    ) -> anyhow::Result<Option<PackagistVersion>> {
        let versions = packagist::fetch_package_versions(package_name, &self.repo_cache).await?;
        Ok(version::find_best_candidate(&versions, self.preferred_stability).cloned())
    }

    /// Generate a recommended constraint string from a concrete version.
    /// Mirrors `VersionSelector::findRecommendedRequireVersion()`.
    pub fn find_recommended_require_version_string(
        &self,
        pkg: &PackagistVersion,
        fixed: bool,
    ) -> String {
        if fixed {
            pkg.version.clone()
        } else {
            let stability = version::stability_of(&pkg.version_normalized);
            version::find_recommended_require_version(
                &pkg.version,
                &pkg.version_normalized,
                stability,
            )
        }
    }
}
