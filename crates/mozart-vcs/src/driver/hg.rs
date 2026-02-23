use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::Result;

use crate::process::ProcessExecutor;
use crate::util::hg::HgUtil;

use super::{DistReference, DriverConfig, SourceReference, VcsDriver};

/// Mercurial VCS driver.
///
/// Corresponds to Composer's `Repository\Vcs\HgDriver`.
pub struct HgDriver {
    url: String,
    repo_dir: Option<PathBuf>,
    root_identifier: Option<String>,
    tags: Option<BTreeMap<String, String>>,
    branches: Option<BTreeMap<String, String>>,
    info_cache: HashMap<String, Option<serde_json::Value>>,
    hg_util: HgUtil,
    config: DriverConfig,
}

impl HgDriver {
    pub fn new(url: &str, config: DriverConfig) -> Self {
        let process = ProcessExecutor::new();
        Self {
            url: url.to_string(),
            repo_dir: None,
            root_identifier: None,
            tags: None,
            branches: None,
            info_cache: HashMap::new(),
            hg_util: HgUtil::new(process),
            config,
        }
    }

    pub fn supports(url: &str) -> bool {
        url.starts_with("hg://") || url.contains("hg.") || url.ends_with(".hg")
    }

    fn get_repo_dir(&self) -> Result<&PathBuf> {
        self.repo_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("HgDriver not initialized"))
    }
}

impl VcsDriver for HgDriver {
    async fn initialize(&mut self) -> Result<()> {
        let cache_dir = self.config.cache_dir.join("hg");
        std::fs::create_dir_all(&cache_dir)?;
        let repo_dir = cache_dir.join(crate::util::git::GitUtil::sanitize_url(&self.url));

        if repo_dir.join(".hg").is_dir() {
            // Update existing clone
            self.hg_util.execute(&["pull"], Some(&repo_dir))?;
        } else {
            // Clone without checkout
            let dir_str = repo_dir.to_string_lossy().to_string();
            self.hg_util
                .execute(&["clone", "--noupdate", &self.url, &dir_str], None)?;
        }

        self.repo_dir = Some(repo_dir.clone());

        // Get default branch
        let output = self.hg_util.execute(
            &["log", "-r", "default", "--template", "{node|short}"],
            Some(&repo_dir),
        );
        self.root_identifier = match output {
            Ok(o) if !o.stdout.trim().is_empty() => Some("default".to_string()),
            _ => Some("tip".to_string()),
        };

        Ok(())
    }

    fn root_identifier(&self) -> &str {
        self.root_identifier.as_deref().unwrap_or("default")
    }

    async fn branches(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.branches.is_none() {
            let repo_dir = self.get_repo_dir()?.clone();
            let mut branches = BTreeMap::new();

            // Named branches
            let output = self.hg_util.execute(&["branches", "-q"], Some(&repo_dir))?;
            for name in ProcessExecutor::split_lines(&output.stdout) {
                let name = name.trim();
                let rev_output = self.hg_util.execute(
                    &["log", "-r", name, "--template", "{node}"],
                    Some(&repo_dir),
                )?;
                branches.insert(name.to_string(), rev_output.stdout.trim().to_string());
            }

            // Bookmarks
            let output = self
                .hg_util
                .execute_unchecked(&["bookmarks", "-q"], Some(&repo_dir))?;
            if output.status == 0 {
                for name in ProcessExecutor::split_lines(&output.stdout) {
                    let name = name.trim();
                    if !branches.contains_key(name) {
                        let rev_output = self.hg_util.execute(
                            &["log", "-r", name, "--template", "{node}"],
                            Some(&repo_dir),
                        )?;
                        branches.insert(name.to_string(), rev_output.stdout.trim().to_string());
                    }
                }
            }

            self.branches = Some(branches);
        }
        Ok(self.branches.as_ref().unwrap())
    }

    async fn tags(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.tags.is_none() {
            let repo_dir = self.get_repo_dir()?.clone();
            let output = self.hg_util.execute(&["tags", "-q"], Some(&repo_dir))?;
            let mut tags = BTreeMap::new();
            for name in ProcessExecutor::split_lines(&output.stdout) {
                let name = name.trim();
                if name == "tip" {
                    continue; // Skip the "tip" pseudo-tag
                }
                let rev_output = self.hg_util.execute(
                    &["log", "-r", name, "--template", "{node}"],
                    Some(&repo_dir),
                )?;
                tags.insert(name.to_string(), rev_output.stdout.trim().to_string());
            }
            self.tags = Some(tags);
        }
        Ok(self.tags.as_ref().unwrap())
    }

    async fn composer_information(
        &mut self,
        identifier: &str,
    ) -> Result<Option<serde_json::Value>> {
        if let Some(cached) = self.info_cache.get(identifier) {
            return Ok(cached.clone());
        }
        let content = self.file_content("composer.json", identifier).await?;
        let value = content.and_then(|c| serde_json::from_str(&c).ok());
        self.info_cache
            .insert(identifier.to_string(), value.clone());
        Ok(value)
    }

    async fn file_content(&self, file: &str, identifier: &str) -> Result<Option<String>> {
        let repo_dir = self.get_repo_dir()?;
        let output = self
            .hg_util
            .execute_unchecked(&["cat", "-r", identifier, "--", file], Some(repo_dir))?;
        if output.status == 0 {
            Ok(Some(output.stdout))
        } else {
            Ok(None)
        }
    }

    async fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        let repo_dir = self.get_repo_dir()?;
        let output = self.hg_util.execute(
            &["log", "-r", identifier, "--template", "{date|isodatesec}"],
            Some(repo_dir),
        )?;
        let date = output.stdout.trim().to_string();
        if date.is_empty() {
            Ok(None)
        } else {
            Ok(Some(date))
        }
    }

    async fn dist(&self, _identifier: &str) -> Result<Option<DistReference>> {
        Ok(None)
    }

    fn source(&self, identifier: &str) -> SourceReference {
        SourceReference {
            source_type: "hg".to_string(),
            url: self.url.clone(),
            reference: identifier.to_string(),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    async fn cleanup(&mut self) -> Result<()> {
        Ok(())
    }
}
