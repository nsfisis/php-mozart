use std::path::Path;

use anyhow::Result;

use crate::process::ProcessExecutor;
use crate::util::git::GitUtil;

use super::VcsDownloader;

/// Git downloader using clone/checkout with optional mirror cache.
///
/// Corresponds to Composer's `Downloader\GitDownloader`.
pub struct GitDownloader {
    git_util: GitUtil,
}

impl GitDownloader {
    pub fn new(git_util: GitUtil) -> Self {
        Self { git_util }
    }
}

impl VcsDownloader for GitDownloader {
    fn download(&self, url: &str, _reference: &str, _target: &Path) -> Result<()> {
        // Pre-sync the mirror so install can use --reference
        self.git_util.sync_mirror(url)?;
        Ok(())
    }

    fn install(&self, url: &str, reference: &str, target: &Path) -> Result<()> {
        let target_str = target.to_string_lossy();
        let mirror_path = self.git_util.mirror_path(url);

        if mirror_path.join("HEAD").exists() {
            // Clone with mirror reference for efficiency
            let mirror_str = mirror_path.to_string_lossy().to_string();
            self.git_util.run_command(
                &[
                    "git",
                    "clone",
                    "--no-checkout",
                    "--dissociate",
                    "--reference",
                    &mirror_str,
                    "--",
                    url,
                    &target_str,
                ],
                url,
                None,
            )?;
        } else {
            self.git_util.run_command(
                &["git", "clone", "--no-checkout", "--", url, &target_str],
                url,
                None,
            )?;
        }

        // Checkout the specific reference
        let process = ProcessExecutor::new();
        process.execute_checked(&["git", "checkout", reference, "--force"], Some(target))?;

        Ok(())
    }

    fn update(&self, url: &str, _old_ref: &str, new_ref: &str, target: &Path) -> Result<()> {
        let process = ProcessExecutor::new();

        // Update remote URL
        process.execute_checked(
            &["git", "remote", "set-url", "origin", "--", url],
            Some(target),
        )?;

        // Fetch latest
        self.git_util
            .run_command(&["git", "fetch", "origin"], url, Some(target))?;

        // Checkout new reference
        process.execute_checked(&["git", "checkout", new_ref, "--force"], Some(target))?;

        Ok(())
    }

    fn remove(&self, target: &Path) -> Result<()> {
        if target.exists() {
            std::fs::remove_dir_all(target)?;
        }
        Ok(())
    }

    fn local_changes(&self, target: &Path) -> Result<Option<String>> {
        let process = ProcessExecutor::new();
        let output = process.execute(&["git", "status", "--porcelain"], Some(target))?;
        if output.stdout.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(output.stdout))
        }
    }

    fn commit_logs(&self, from: &str, to: &str, target: &Path) -> Result<String> {
        let process = ProcessExecutor::new();
        let range = format!("{from}..{to}");
        let output = process.execute(
            &["git", "log", &range, "--oneline", "--no-decorate"],
            Some(target),
        )?;
        Ok(output.stdout)
    }
}
