//! [`Repository`] for VCS-type repositories.
//!
//! Wraps [`crate::vcs_bridge::scan_vcs_repositories`] + [`crate::vcs_bridge::vcs_to_packagist_version`].
//! Scanning is expensive (clones / fetches), so we do it once at construction
//! and serve subsequent queries from the in-memory cache. Mirrors
//! `Composer\Repository\Vcs\VcsRepository`'s lazy-then-memoized behavior.

use super::super::packagist::PackagistVersion;
use super::super::vcs_bridge::{scan_vcs_repositories, vcs_to_packagist_version};
use super::{LoadResult, NamedPackagistVersion, PackageQuery, Repository};
use crate::package::RawRepository;

pub struct VcsRepository {
    id: String,
    versions: Vec<(String, PackagistVersion)>,
}

impl VcsRepository {
    /// Scan every VCS-type entry in `repositories` and cache the resulting
    /// versions. Non-VCS entries are ignored. This performs network I/O.
    pub async fn from_repositories(repositories: &[RawRepository]) -> Self {
        let scanned = scan_vcs_repositories(repositories).await;
        let versions = scanned
            .iter()
            .map(|v| (v.name.clone(), vcs_to_packagist_version(v)))
            .collect();
        Self {
            id: "vcs".to_string(),
            versions,
        }
    }

    pub fn version_count(&self) -> usize {
        self.versions.len()
    }
}

#[async_trait::async_trait]
impl Repository for VcsRepository {
    fn id(&self) -> &str {
        &self.id
    }

    async fn load_packages(&self, queries: &[PackageQuery<'_>]) -> anyhow::Result<LoadResult> {
        let mut result = LoadResult::default();
        for query in queries {
            let mut found_any = false;
            for (name, version) in &self.versions {
                if name == query.name {
                    found_any = true;
                    result.packages.push(NamedPackagistVersion {
                        name: name.clone(),
                        version: version.clone(),
                    });
                }
            }
            if found_any {
                result.names_found.push(query.name.to_string());
            }
        }
        Ok(result)
    }
}
