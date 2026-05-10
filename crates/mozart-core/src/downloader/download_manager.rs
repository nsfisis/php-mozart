//! `DownloadManager` — pick the right [`VcsDownloader`] for a given
//! [`LocalPackage`]. Mirrors `Composer\Downloader\DownloadManager`.

use crate::composer::{InstallationSource, LocalPackage};
use crate::downloader::{GitDownloader, HgDownloader, SvnDownloader, VcsDownloader};
use crate::vcs::process::ProcessExecutor;
use crate::vcs::util::git::GitUtil;
use crate::vcs::util::hg::HgUtil;
use crate::vcs::util::svn::SvnUtil;
use std::path::PathBuf;

/// Selects a `VcsDownloader` for a package based on its installation source
/// and source type. Mirrors `DownloadManager::getDownloaderForPackage`:
///
/// - `metapackage` → `None`.
/// - `installation-source: dist` → `None` (Composer would return a
///   `FileDownloader`-family object that does not implement
///   `ChangeReportInterface` / `DvcsDownloaderInterface`, so the status
///   command's `instanceof` checks all become no-ops; returning `None`
///   directly is the equivalent in our trait-object world).
/// - `installation-source: source` → the matching VCS downloader by
///   `source.type` (`git` / `hg` / `svn`).
pub struct DownloadManager {
    git_cache_dir: PathBuf,
}

impl DownloadManager {
    /// `git_cache_dir`: where `GitUtil` should keep mirror clones (e.g.
    /// `<vendor>/.cache/git`).
    pub fn new(git_cache_dir: PathBuf) -> Self {
        Self { git_cache_dir }
    }

    pub fn get_downloader_for_package(
        &self,
        package: &LocalPackage,
    ) -> Option<Box<dyn VcsDownloader>> {
        if package.package_type() == Some("metapackage") {
            return None;
        }
        match package.installation_source()? {
            InstallationSource::Dist => None,
            InstallationSource::Source => {
                let kind = package.source()?.kind.as_str();
                match kind {
                    "git" => {
                        let git_util =
                            GitUtil::new(ProcessExecutor::new(), self.git_cache_dir.clone());
                        Some(Box::new(GitDownloader::new(git_util)))
                    }
                    "hg" => {
                        let hg_util = HgUtil::new(ProcessExecutor::new());
                        Some(Box::new(HgDownloader::new(hg_util)))
                    }
                    "svn" => {
                        let svn_util = SvnUtil::new(ProcessExecutor::new());
                        Some(Box::new(SvnDownloader::new(svn_util)))
                    }
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::PackageReference;
    use serde_json::Value;

    fn pkg(
        installation_source: Option<InstallationSource>,
        source_kind: Option<&str>,
    ) -> LocalPackage {
        let source = source_kind.map(|kind| PackageReference {
            kind: kind.to_string(),
            url: "https://example/repo".into(),
            reference: Some("abc123".into()),
            shasum: None,
        });
        LocalPackage::new(
            "vendor/pkg".into(),
            "1.0.0".into(),
            None,
            Some("library".into()),
            installation_source,
            source,
            None,
            Value::Null,
        )
    }

    #[test]
    fn metapackage_returns_none() {
        let dm = DownloadManager::new(PathBuf::from("/tmp/mz-test-cache"));
        let mut p = pkg(Some(InstallationSource::Source), Some("git"));
        // override type
        p = LocalPackage::new(
            "vendor/pkg".into(),
            "1.0.0".into(),
            None,
            Some("metapackage".into()),
            p.installation_source(),
            p.source().cloned(),
            None,
            Value::Null,
        );
        assert!(dm.get_downloader_for_package(&p).is_none());
    }

    #[test]
    fn dist_install_returns_none() {
        let dm = DownloadManager::new(PathBuf::from("/tmp/mz-test-cache"));
        let p = pkg(Some(InstallationSource::Dist), Some("git"));
        assert!(dm.get_downloader_for_package(&p).is_none());
    }

    #[test]
    fn source_install_with_git_returns_some() {
        let dm = DownloadManager::new(PathBuf::from("/tmp/mz-test-cache"));
        let p = pkg(Some(InstallationSource::Source), Some("git"));
        assert!(dm.get_downloader_for_package(&p).is_some());
    }

    #[test]
    fn unknown_source_kind_returns_none() {
        let dm = DownloadManager::new(PathBuf::from("/tmp/mz-test-cache"));
        let p = pkg(Some(InstallationSource::Source), Some("perforce"));
        assert!(dm.get_downloader_for_package(&p).is_none());
    }

    #[test]
    fn missing_installation_source_returns_none() {
        let dm = DownloadManager::new(PathBuf::from("/tmp/mz-test-cache"));
        let p = pkg(None, Some("git"));
        assert!(dm.get_downloader_for_package(&p).is_none());
    }
}
