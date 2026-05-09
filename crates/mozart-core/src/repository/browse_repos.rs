//! Composite of repositories consulted by the `browse` command.
//!
//! Mirrors `Composer\Command\HomeCommand::initializeRepos()`:
//! root package + local installed repository + remote(s). Each repo
//! exposes a uniform [`BrowseRepo::find_packages`] that yields
//! [`CompletePackageView`]s — the trio of fields
//! `Composer\Command\HomeCommand::handlePackage` reads off
//! `CompletePackageInterface` (`getSupport()['source']`,
//! `getSourceUrl()`, `getHomepage()`).

use super::super::package::RawPackageData;
use super::cache::Cache;
use super::installed::{InstalledPackageEntry, InstalledPackages};
use super::lockfile::LockedPackage;
use super::packagist::{self, PackagistVersion};

/// Subset of `Composer\Package\CompletePackageInterface` consumed by
/// `HomeCommand::handlePackage`. Every backing repo flattens its
/// package shape into this so URL selection lives in one place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletePackageView {
    /// `$package->getSupport()['source']`.
    pub support_source: Option<String>,
    /// `$package->getSourceUrl()`.
    pub source_url: Option<String>,
    /// `$package->getHomepage()`.
    pub homepage: Option<String>,
}

impl From<&LockedPackage> for CompletePackageView {
    fn from(pkg: &LockedPackage) -> Self {
        Self {
            support_source: pkg
                .support
                .as_ref()
                .and_then(|s| s.get("source"))
                .and_then(|s| s.as_str())
                .map(str::to_string),
            source_url: pkg.source.as_ref().map(|s| s.url.clone()),
            homepage: pkg.homepage.clone(),
        }
    }
}

impl From<&InstalledPackageEntry> for CompletePackageView {
    fn from(pkg: &InstalledPackageEntry) -> Self {
        Self {
            support_source: pkg
                .support
                .as_ref()
                .and_then(|s| s.get("source"))
                .and_then(|s| s.as_str())
                .map(str::to_string),
            source_url: pkg
                .source
                .as_ref()
                .and_then(|s| s.get("url"))
                .and_then(|s| s.as_str())
                .map(str::to_string),
            homepage: pkg.homepage.clone(),
        }
    }
}

impl From<&PackagistVersion> for CompletePackageView {
    fn from(pkg: &PackagistVersion) -> Self {
        Self {
            support_source: pkg
                .support
                .as_ref()
                .and_then(|s| s.get("source"))
                .and_then(|s| s.as_str())
                .map(str::to_string),
            source_url: pkg.source.as_ref().map(|s| s.url.clone()),
            homepage: pkg.homepage.clone(),
        }
    }
}

/// `RawPackageData` lacks a typed `support` field — the root package's
/// `support` block lives inside `extra_fields` because the schema is not
/// yet ported. Read it manually here.
pub fn view_from_raw(pkg: &RawPackageData) -> CompletePackageView {
    CompletePackageView {
        support_source: pkg
            .extra_fields
            .get("support")
            .and_then(|s| s.get("source"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        source_url: None,
        homepage: pkg.homepage.clone(),
    }
}

/// One repository in the composite. Mirrors the three repo kinds
/// `HomeCommand::initializeRepos()` returns:
/// `RootPackageRepository` + local installed + remotes.
pub enum BrowseRepo {
    /// Stand-in for `Composer\Repository\RootPackageRepository` —
    /// a one-package array containing the root composer.json.
    /// Boxed because `RawPackageData` is much larger than the other
    /// variants (clippy::large_enum_variant).
    Root(Box<RawPackageData>),
    /// Stand-in for `RepositoryManager::getLocalRepository()` —
    /// the installed.json view of `vendor/`.
    Installed(InstalledPackages),
    /// Stand-in for the configured remote. For now Mozart only knows
    /// the default Packagist remote (`RepositoryFactory::defaultRepos`).
    Packagist { cache: Cache },
}

impl BrowseRepo {
    /// Mirrors `RepositoryInterface::findPackages($name)` — case-insensitive
    /// match by package name, returning every match the repo holds.
    pub async fn find_packages(&self, name: &str) -> anyhow::Result<Vec<CompletePackageView>> {
        match self {
            BrowseRepo::Root(pkg) => {
                if pkg.name.eq_ignore_ascii_case(name) {
                    Ok(vec![view_from_raw(pkg)])
                } else {
                    Ok(Vec::new())
                }
            }
            BrowseRepo::Installed(installed) => Ok(installed
                .packages
                .iter()
                .filter(|p| p.name.eq_ignore_ascii_case(name))
                .map(CompletePackageView::from)
                .collect()),
            BrowseRepo::Packagist { cache } => {
                let versions = packagist::fetch_package_versions(name, cache).await?;
                Ok(versions.iter().map(CompletePackageView::from).collect())
            }
        }
    }
}

/// Ordered composite consulted by `HomeCommand::execute()`'s outer
/// `foreach ($repos as $repo)` loop.
pub struct BrowseRepos {
    repos: Vec<BrowseRepo>,
}

impl BrowseRepos {
    /// Build the composite. `root` and `installed` are passed in
    /// rather than read here so callers can decide whether to load
    /// them from `Composer` (when composer.json is present) or skip
    /// them entirely (the `defaultReposWithDefaultManager` fallback).
    pub fn new(
        root: Option<RawPackageData>,
        installed: Option<InstalledPackages>,
        packagist_cache: Cache,
    ) -> Self {
        let mut repos: Vec<BrowseRepo> = Vec::with_capacity(3);
        if let Some(root) = root {
            repos.push(BrowseRepo::Root(Box::new(root)));
        }
        if let Some(installed) = installed {
            repos.push(BrowseRepo::Installed(installed));
        }
        repos.push(BrowseRepo::Packagist {
            cache: packagist_cache,
        });
        Self { repos }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, BrowseRepo> {
        self.repos.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn locked(
        name: &str,
        source_url: Option<&str>,
        homepage: Option<&str>,
        support_source: Option<&str>,
    ) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: source_url.map(|url| super::super::lockfile::LockedSource {
                source_type: "git".to_string(),
                url: url.to_string(),
                reference: None,
            }),
            dist: None,
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            conflict: BTreeMap::new(),
            provide: BTreeMap::new(),
            replace: BTreeMap::new(),
            suggest: None,
            package_type: None,
            autoload: None,
            autoload_dev: None,
            license: None,
            description: None,
            homepage: homepage.map(str::to_string),
            keywords: None,
            authors: None,
            support: support_source.map(|s| serde_json::json!({"source": s})),
            funding: None,
            time: None,
            extra_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn view_from_locked_package_carries_three_urls() {
        let pkg = locked(
            "vendor/pkg",
            Some("https://github.com/vendor/pkg.git"),
            Some("https://vendor.example.com"),
            Some("https://github.com/vendor/pkg"),
        );
        let view = CompletePackageView::from(&pkg);
        assert_eq!(
            view.support_source.as_deref(),
            Some("https://github.com/vendor/pkg")
        );
        assert_eq!(
            view.source_url.as_deref(),
            Some("https://github.com/vendor/pkg.git")
        );
        assert_eq!(view.homepage.as_deref(), Some("https://vendor.example.com"));
    }

    #[test]
    fn view_from_installed_entry_extracts_source_url() {
        let mut entry = InstalledPackageEntry {
            name: "vendor/pkg".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: Some(serde_json::json!({"url": "https://github.com/vendor/pkg.git"})),
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: Some("https://vendor.example.com".to_string()),
            support: Some(serde_json::json!({"source": "https://github.com/vendor/pkg"})),
            extra_fields: BTreeMap::new(),
        };
        let view = CompletePackageView::from(&entry);
        assert_eq!(
            view.source_url.as_deref(),
            Some("https://github.com/vendor/pkg.git")
        );
        assert_eq!(
            view.support_source.as_deref(),
            Some("https://github.com/vendor/pkg")
        );
        assert_eq!(view.homepage.as_deref(), Some("https://vendor.example.com"));

        entry.support = None;
        entry.source = None;
        entry.homepage = None;
        let empty = CompletePackageView::from(&entry);
        assert_eq!(empty, CompletePackageView::default());
    }

    #[test]
    fn view_from_raw_reads_support_via_extra_fields() {
        let mut raw = RawPackageData::new("vendor/root".to_string());
        raw.homepage = Some("https://vendor.example.com".to_string());
        raw.extra_fields.insert(
            "support".to_string(),
            serde_json::json!({"source": "https://github.com/vendor/root"}),
        );
        let view = view_from_raw(&raw);
        assert_eq!(
            view.support_source.as_deref(),
            Some("https://github.com/vendor/root")
        );
        assert!(view.source_url.is_none());
        assert_eq!(view.homepage.as_deref(), Some("https://vendor.example.com"));
    }

    #[tokio::test]
    async fn root_repo_matches_case_insensitively() {
        let raw = RawPackageData::new("Vendor/Root".to_string());
        let repo = BrowseRepo::Root(Box::new(raw));
        assert_eq!(repo.find_packages("vendor/root").await.unwrap().len(), 1);
        assert_eq!(repo.find_packages("other/pkg").await.unwrap().len(), 0);
    }
}
