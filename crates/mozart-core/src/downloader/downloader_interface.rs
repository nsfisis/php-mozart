use crate::downloader::VcsDownloader;

pub trait DownloaderInterface: Send + Sync {
    fn as_vcs_downloader(&self) -> Option<&dyn VcsDownloader>;
}
