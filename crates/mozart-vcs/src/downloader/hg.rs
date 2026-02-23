use std::path::Path;

use anyhow::Result;

use crate::util::hg::HgUtil;

use super::VcsDownloader;

/// Mercurial downloader using clone/pull/update.
pub struct HgDownloader {
    hg_util: HgUtil,
}

impl HgDownloader {
    pub fn new(hg_util: HgUtil) -> Self {
        Self { hg_util }
    }
}

impl VcsDownloader for HgDownloader {
    fn download(&self, _url: &str, _reference: &str, _target: &Path) -> Result<()> {
        Ok(())
    }

    fn install(&self, url: &str, reference: &str, target: &Path) -> Result<()> {
        let target_str = target.to_string_lossy().to_string();
        self.hg_util
            .execute(&["clone", "--", url, &target_str], None)?;
        self.hg_util
            .execute(&["update", "-r", reference], Some(target))?;
        Ok(())
    }

    fn update(&self, url: &str, _old_ref: &str, new_ref: &str, target: &Path) -> Result<()> {
        self.hg_util.execute(&["pull", url], Some(target))?;
        self.hg_util
            .execute(&["update", "-r", new_ref], Some(target))?;
        Ok(())
    }

    fn remove(&self, target: &Path) -> Result<()> {
        if target.exists() {
            std::fs::remove_dir_all(target)?;
        }
        Ok(())
    }

    fn local_changes(&self, target: &Path) -> Result<Option<String>> {
        let output = self.hg_util.execute(&["st"], Some(target))?;
        if output.stdout.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(output.stdout))
        }
    }

    fn commit_logs(&self, from: &str, to: &str, target: &Path) -> Result<String> {
        let range = format!("{from}:{to}");
        let output = self.hg_util.execute(
            &[
                "log",
                "-r",
                &range,
                "--template",
                "{rev}:{node|short} {desc|firstline}\\n",
            ],
            Some(target),
        )?;
        Ok(output.stdout)
    }
}
