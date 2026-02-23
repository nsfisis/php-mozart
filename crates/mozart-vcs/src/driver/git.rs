use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::process::ProcessExecutor;
use crate::util::git::GitUtil;

use super::{DistReference, DriverConfig, SourceReference, VcsDriver};

/// Git VCS driver.
///
/// Corresponds to Composer's `Repository\Vcs\GitDriver`.
pub struct GitDriver {
    url: String,
    repo_dir: Option<PathBuf>,
    root_identifier: Option<String>,
    tags: Option<BTreeMap<String, String>>,
    branches: Option<BTreeMap<String, String>>,
    info_cache: HashMap<String, Option<serde_json::Value>>,
    git_util: GitUtil,
    is_local: bool,
}

impl GitDriver {
    pub fn new(url: &str, config: DriverConfig) -> Self {
        let is_local = Self::is_local_path(url);
        let process = ProcessExecutor::new();
        let git_util = GitUtil::new(process, config.cache_dir.join("git"));
        Self {
            url: url.to_string(),
            repo_dir: if is_local {
                Some(PathBuf::from(url))
            } else {
                None
            },
            root_identifier: None,
            tags: None,
            branches: None,
            info_cache: HashMap::new(),
            git_util,
            is_local,
        }
    }

    /// Check if a URL is supported by the Git driver.
    pub fn supports(url: &str) -> bool {
        if Self::is_local_path(url) {
            return Path::new(url).join(".git").is_dir() || url.ends_with(".git");
        }
        url.starts_with("git://")
            || url.starts_with("git@")
            || url.ends_with(".git")
            || url.contains("git.")
    }

    fn is_local_path(url: &str) -> bool {
        !url.contains("://") && !url.starts_with("git@") && Path::new(url).exists()
    }

    fn get_repo_dir(&self) -> Result<&Path> {
        self.repo_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("GitDriver not initialized"))
    }

    fn parse_branches(output: &str) -> BTreeMap<String, String> {
        let mut branches = BTreeMap::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || line.contains("HEAD detached") || line.contains("->") {
                continue;
            }
            // Remove leading "* " for current branch
            let line = line.strip_prefix("* ").unwrap_or(line);
            // Format: "branch_name  commit_hash ..."
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                branches.insert(parts[0].to_string(), parts[1].to_string());
            }
        }
        branches
    }

    fn parse_tags(output: &str) -> BTreeMap<String, String> {
        let mut tags = BTreeMap::new();
        // First pass: collect dereferenced tags (^{})
        let mut dereferenced = HashMap::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format: "commit_hash refs/tags/tag_name" or "commit_hash refs/tags/tag_name^{}"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let hash = parts[0];
                let refname = parts[1];
                if let Some(tag_name) = refname.strip_prefix("refs/tags/")
                    && let Some(tag_name) = tag_name.strip_suffix("^{}")
                {
                    // Dereferenced tag - this is the actual commit
                    dereferenced.insert(tag_name.to_string(), hash.to_string());
                }
            }
        }
        // Second pass: collect all tags, preferring dereferenced values
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let hash = parts[0];
                let refname = parts[1];
                if let Some(tag_name) = refname.strip_prefix("refs/tags/") {
                    if tag_name.ends_with("^{}") {
                        continue; // Skip dereferenced entries themselves
                    }
                    let resolved = dereferenced
                        .get(tag_name)
                        .cloned()
                        .unwrap_or_else(|| hash.to_string());
                    tags.insert(tag_name.to_string(), resolved);
                }
            }
        }
        tags
    }
}

impl VcsDriver for GitDriver {
    fn initialize(&mut self) -> Result<()> {
        if self.is_local {
            // Local repo: use directly (or its .git subdir)
            let path = Path::new(&self.url);
            if path.join(".git").is_dir() {
                self.repo_dir = Some(path.join(".git"));
            } else {
                self.repo_dir = Some(path.to_path_buf());
            }
        } else {
            // Remote repo: sync mirror
            let mirror_dir = self.git_util.sync_mirror(&self.url)?;
            self.repo_dir = Some(mirror_dir);
        }

        // Determine root identifier (default branch)
        let repo_dir = self.repo_dir.clone().unwrap();
        if let Ok(Some(branch)) = self.git_util.get_default_branch(&repo_dir) {
            self.root_identifier = Some(branch);
        } else {
            // Fallback: try common branch names
            let process = ProcessExecutor::new();
            for name in &["main", "master"] {
                let output =
                    process.execute(&["git", "rev-parse", "--verify", name], Some(&repo_dir))?;
                if output.status == 0 {
                    self.root_identifier = Some(name.to_string());
                    break;
                }
            }
        }

        if self.root_identifier.is_none() {
            self.root_identifier = Some("master".to_string());
        }

        Ok(())
    }

    fn root_identifier(&self) -> &str {
        self.root_identifier.as_deref().unwrap_or("master")
    }

    fn branches(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.branches.is_none() {
            let repo_dir = self.get_repo_dir()?.to_path_buf();
            let process = ProcessExecutor::new();
            let output = process.execute_checked(
                &["git", "branch", "--no-color", "--no-abbrev", "-v"],
                Some(&repo_dir),
            )?;
            self.branches = Some(Self::parse_branches(&output.stdout));
        }
        Ok(self.branches.as_ref().unwrap())
    }

    fn tags(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.tags.is_none() {
            let repo_dir = self.get_repo_dir()?.to_path_buf();
            let process = ProcessExecutor::new();
            let output = process.execute(
                &["git", "show-ref", "--tags", "--dereference"],
                Some(&repo_dir),
            )?;
            self.tags = Some(if output.status == 0 {
                Self::parse_tags(&output.stdout)
            } else {
                BTreeMap::new()
            });
        }
        Ok(self.tags.as_ref().unwrap())
    }

    fn composer_information(&mut self, identifier: &str) -> Result<Option<serde_json::Value>> {
        if let Some(cached) = self.info_cache.get(identifier) {
            return Ok(cached.clone());
        }

        let content = self.file_content("composer.json", identifier)?;
        let value = match content {
            Some(c) => serde_json::from_str(&c).ok(),
            None => None,
        };

        self.info_cache
            .insert(identifier.to_string(), value.clone());
        Ok(value)
    }

    fn file_content(&self, file: &str, identifier: &str) -> Result<Option<String>> {
        let repo_dir = self.get_repo_dir()?;
        let process = ProcessExecutor::new();
        let resource = format!("{identifier}:{file}");
        let output = process.execute(&["git", "show", &resource], Some(repo_dir))?;
        if output.status == 0 {
            Ok(Some(output.stdout))
        } else {
            Ok(None)
        }
    }

    fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        let repo_dir = self.get_repo_dir()?;
        let process = ProcessExecutor::new();
        let output = process.execute(
            &["git", "log", "-1", "--format=%aI", identifier],
            Some(repo_dir),
        )?;
        if output.status == 0 {
            let date = output.stdout.trim().to_string();
            if date.is_empty() {
                Ok(None)
            } else {
                Ok(Some(date))
            }
        } else {
            Ok(None)
        }
    }

    fn dist(&self, _identifier: &str) -> Result<Option<DistReference>> {
        // Plain git repos don't provide dist archives
        Ok(None)
    }

    fn source(&self, identifier: &str) -> SourceReference {
        SourceReference {
            source_type: "git".to_string(),
            url: self.url.clone(),
            reference: identifier.to_string(),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn cleanup(&mut self) -> Result<()> {
        Ok(())
    }
}
