use std::path::Path;

use anyhow::Result;

use crate::util::svn::SvnUtil;

use super::VcsDownloader;

/// SVN downloader using checkout/switch.
pub struct SvnDownloader {
    svn_util: SvnUtil,
}

impl SvnDownloader {
    pub fn new(svn_util: SvnUtil) -> Self {
        Self { svn_util }
    }
}

impl VcsDownloader for SvnDownloader {
    fn download(&self, _url: &str, _reference: &str, _target: &Path) -> Result<()> {
        // SVN doesn't need a pre-download step
        Ok(())
    }

    fn install(&self, url: &str, reference: &str, target: &Path) -> Result<()> {
        let target_str = target.to_string_lossy().to_string();
        let svn_url = format!("{url}@{reference}");
        self.svn_util
            .execute(&["checkout", &svn_url, &target_str], None)?;
        Ok(())
    }

    fn update(&self, url: &str, _old_ref: &str, new_ref: &str, target: &Path) -> Result<()> {
        let svn_url = format!("{url}@{new_ref}");
        self.svn_util
            .execute(&["switch", "--ignore-ancestry", &svn_url], Some(target))?;
        Ok(())
    }

    fn remove(&self, target: &Path) -> Result<()> {
        if target.exists() {
            std::fs::remove_dir_all(target)?;
        }
        Ok(())
    }

    fn local_changes(&self, target: &Path) -> Result<Option<String>> {
        let output = self.svn_util.execute(&["status"], Some(target))?;
        if output.stdout.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(output.stdout))
        }
    }

    fn commit_logs(&self, from: &str, to: &str, target: &Path) -> Result<String> {
        let range = format!("{from}:{to}");
        let output = self
            .svn_util
            .execute(&["log", "-r", &range], Some(target))?;
        Ok(output.stdout)
    }
}
