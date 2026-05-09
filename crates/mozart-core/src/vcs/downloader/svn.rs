use super::super::util::svn::SvnUtil;
use super::VcsDownloader;
use anyhow::Result;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Match any non-`X` status line (mirror of Composer's
/// `{^ *[^X ] +}m`). Ignores externals (`X` prefix).
static SVN_STATUS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^ *[^X ] +").unwrap());

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

    fn get_local_changes(&self, target: &Path) -> Result<Option<String>> {
        if !target.join(".svn").is_dir() {
            return Ok(None);
        }
        let output = self
            .svn_util
            .execute(&["status", "--ignore-externals"], Some(target))?;
        if SVN_STATUS_RE.is_match(&output.stdout) {
            Ok(Some(output.stdout))
        } else {
            Ok(None)
        }
    }

    fn commit_logs(&self, from: &str, to: &str, target: &Path) -> Result<String> {
        let range = format!("{from}:{to}");
        let output = self
            .svn_util
            .execute(&["log", "-r", &range], Some(target))?;
        Ok(output.stdout)
    }

    fn is_change_report(&self) -> bool {
        true
    }

    fn is_vcs_capable_downloader(&self) -> bool {
        true
    }

    fn is_dvcs_downloader(&self) -> bool {
        false
    }
}
