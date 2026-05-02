//! [`Repository`] for inline `type: package` repositories.
//!
//! Wraps [`crate::inline_package::collect_inline_packages`]. The data is
//! embedded in `composer.json` so there's no I/O — the repo just filters
//! its in-memory list by queried name.
//!
//! Mirrors `Composer\Repository\PackageRepository` (which extends
//! `ArrayRepository`). Only the package's own `name` is matched against
//! queries — `replace`/`provide` targets are NOT advertised here, exactly
//! like Composer's `ArrayRepository::loadPackages` checks `getName()` only.
//! Replacement satisfaction happens later in the solver once the replacing
//! package is loaded transitively.

use super::{LoadResult, NamedPackagistVersion, PackageQuery, Repository};
use crate::inline_package::{InlinePackage, collect_inline_packages};
use mozart_core::package::RawRepository;

pub struct InlinePackageRepository {
    id: String,
    packages: Vec<InlinePackage>,
}

impl InlinePackageRepository {
    /// Build from the raw `repositories` array of a `composer.json`. Non-
    /// `package` entries are ignored.
    pub fn from_repositories(repositories: &[RawRepository]) -> Self {
        Self {
            id: "package".to_string(),
            packages: collect_inline_packages(repositories),
        }
    }

    pub fn package_count(&self) -> usize {
        self.packages.len()
    }
}

#[async_trait::async_trait]
impl Repository for InlinePackageRepository {
    fn id(&self) -> &str {
        &self.id
    }

    async fn load_packages(&self, queries: &[PackageQuery<'_>]) -> anyhow::Result<LoadResult> {
        let mut result = LoadResult::default();
        for query in queries {
            let mut found_any = false;
            for ipkg in &self.packages {
                if ipkg.name == query.name {
                    found_any = true;
                    result.packages.push(NamedPackagistVersion {
                        name: ipkg.name.clone(),
                        version: ipkg.version.clone(),
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
