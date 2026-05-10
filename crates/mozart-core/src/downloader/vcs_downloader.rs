/// The VCS downloader interface.
///
/// Corresponds to Composer's `VcsDownloader` hierarchy.
pub trait VcsDownloader {
    /// Prepare for installation (e.g., sync mirror cache).
    fn download(&self, url: &str, reference: &str, target: &std::path::Path) -> anyhow::Result<()>;

    /// Install (clone/checkout) the source to the target directory.
    fn install(&self, url: &str, reference: &str, target: &std::path::Path) -> anyhow::Result<()>;

    /// Update the source at target to a new reference.
    fn update(
        &self,
        url: &str,
        old_ref: &str,
        new_ref: &str,
        target: &std::path::Path,
    ) -> anyhow::Result<()>;

    /// Remove the source from the target directory.
    fn remove(&self, target: &std::path::Path) -> anyhow::Result<()>;

    /// Detect local changes in the working copy.
    /// Returns `None` if clean, `Some(diff)` if modified.
    /// Mirrors `Composer\Downloader\ChangeReportInterface::getLocalChanges`.
    fn get_local_changes(&self, target: &std::path::Path) -> anyhow::Result<Option<String>>;

    /// Detect commits present locally but not on the tracking remote.
    /// Returns `None` if there are no unpushed commits or the concept does
    /// not apply (only `GitDownloader` implements this in Composer's
    /// `DvcsDownloaderInterface`).
    fn unpushed_changes(&self, _target: &std::path::Path) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Resolve the working copy's current VCS reference (e.g. commit hash).
    /// Returns `None` if no reference can be determined. Mirrors
    /// `Composer\Downloader\VcsCapableDownloaderInterface::getVcsReference`.
    fn vcs_reference(&self, _target: &std::path::Path) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Get commit log between two references.
    fn commit_logs(&self, from: &str, to: &str, target: &std::path::Path)
    -> anyhow::Result<String>;

    /// instanceof ChangeReportInterface
    fn is_change_report(&self) -> bool;

    /// instanceof VcsCapableDownloaderInterface
    fn is_vcs_capable_downloader(&self) -> bool;

    /// instanceof DvcsDownloaderInterface
    fn is_dvcs_downloader(&self) -> bool;
}
