pub mod git;
pub mod hg;
pub mod svn;

use std::path::Path;

use anyhow::Result;

/// The VCS downloader interface.
///
/// Corresponds to Composer's `VcsDownloader` hierarchy.
pub trait VcsDownloader {
    /// Prepare for installation (e.g., sync mirror cache).
    fn download(&self, url: &str, reference: &str, target: &Path) -> Result<()>;

    /// Install (clone/checkout) the source to the target directory.
    fn install(&self, url: &str, reference: &str, target: &Path) -> Result<()>;

    /// Update the source at target to a new reference.
    fn update(&self, url: &str, old_ref: &str, new_ref: &str, target: &Path) -> Result<()>;

    /// Remove the source from the target directory.
    fn remove(&self, target: &Path) -> Result<()>;

    /// Detect local changes in the working copy.
    /// Returns `None` if clean, `Some(diff)` if modified.
    fn local_changes(&self, target: &Path) -> Result<Option<String>>;

    /// Get commit log between two references.
    fn commit_logs(&self, from: &str, to: &str, target: &Path) -> Result<String>;
}
