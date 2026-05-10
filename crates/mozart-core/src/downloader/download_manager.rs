use crate::composer::{InstallationSource, LocalPackage};
use crate::console::IoInterface;
use crate::downloader::{DownloaderInterface, VcsDownloader};
use crate::repository::cache::Cache;
use crate::repository::downloader::{DownloadProgress, download_dist};
use crate::util::Filesystem;

/// ref: \Composer\Downloader\DownloadManager
pub struct DownloadManager {
    #[allow(unused)]
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    prefer_dist: bool,
    prefer_source: bool,
    #[allow(unused)]
    package_preferences: Vec<(String, InstallationSource)>,
    #[allow(unused)]
    filesystem: Filesystem,
    downloaders: indexmap::IndexMap<String, Box<dyn DownloaderInterface>>,
    files_cache: Cache, // TODO: remove
}

impl DownloadManager {
    pub fn new(
        io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        prefer_source: bool,
        filesystem: Filesystem,
        files_cache: Cache,
    ) -> Self {
        Self {
            io,
            prefer_dist: false,
            prefer_source,
            package_preferences: Vec::new(),
            filesystem,
            downloaders: indexmap::IndexMap::new(),
            files_cache,
        }
    }

    pub fn set_downloader(&mut self, r#type: String, downloader: Box<dyn DownloaderInterface>) {
        assert!(r#type.chars().all(|c| c.is_ascii_lowercase()));

        self.downloaders.insert(r#type, downloader);
    }

    pub fn get_downloader_for_package(&self, package: &LocalPackage) -> Option<&dyn VcsDownloader> {
        if package.package_type() == Some("metapackage") {
            return None;
        }
        match package.installation_source()? {
            InstallationSource::Dist => None,
            InstallationSource::Source => {
                let kind = package.source()?.kind.as_str();
                self.downloaders
                    .get(kind)
                    .and_then(|d| d.as_vcs_downloader())
            }
        }
    }

    /// Makes downloader prefer source installation over the dist.
    pub fn set_prefer_source(&mut self, prefer_source: bool) {
        self.prefer_source = prefer_source;
    }

    /// Makes downloader prefer dist installation over the source.
    pub fn set_prefer_dist(&mut self, prefer_dist: bool) {
        self.prefer_dist = prefer_dist;
    }

    pub async fn download_legacy(
        &self,
        url: &str,
        expected_shasum: Option<&str>,
        progress: Option<&mut DownloadProgress>,
    ) -> anyhow::Result<Vec<u8>> {
        download_dist(url, expected_shasum, progress, &self.files_cache).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::PackageReference;
    use crate::console::Console;
    use crate::downloader::GitDownloader;
    use crate::vcs::process::ProcessExecutor;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn make_dm() -> DownloadManager {
        let io: Arc<Mutex<Box<dyn IoInterface>>> = Arc::new(Mutex::new(Box::new(Console::new(
            0, true, false, true, true,
        ))
            as Box<dyn IoInterface>));
        let cache_dir = PathBuf::from("/tmp/mz-test-cache");
        let cache = Cache::new(cache_dir.clone(), false);
        let mut dm = DownloadManager::new(io, false, Filesystem::new(), cache);
        dm.set_downloader(
            "git".to_owned(),
            Box::new(GitDownloader::new(ProcessExecutor::new(), cache_dir)),
        );
        dm
    }

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
        let dm = make_dm();
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
        let dm = make_dm();
        let p = pkg(Some(InstallationSource::Dist), Some("git"));
        assert!(dm.get_downloader_for_package(&p).is_none());
    }

    #[test]
    fn source_install_with_git_returns_some() {
        let dm = make_dm();
        let p = pkg(Some(InstallationSource::Source), Some("git"));
        assert!(dm.get_downloader_for_package(&p).is_some());
    }

    #[test]
    fn unknown_source_kind_returns_none() {
        let dm = make_dm();
        let p = pkg(Some(InstallationSource::Source), Some("perforce"));
        assert!(dm.get_downloader_for_package(&p).is_none());
    }

    #[test]
    fn missing_installation_source_returns_none() {
        let dm = make_dm();
        let p = pkg(None, Some("git"));
        assert!(dm.get_downloader_for_package(&p).is_none());
    }
}
