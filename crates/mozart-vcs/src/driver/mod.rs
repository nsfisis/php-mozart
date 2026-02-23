pub mod bitbucket;
pub mod forgejo;
pub mod git;
pub mod github;
pub mod gitlab;
pub mod hg;
pub mod svn;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Reference to a source distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceReference {
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: String,
    pub reference: String,
}

/// Reference to a dist (archive) distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistReference {
    #[serde(rename = "type")]
    pub dist_type: String,
    pub url: String,
    pub reference: String,
    pub shasum: Option<String>,
}

/// Configuration passed to VCS drivers.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// Path for caching VCS mirrors.
    pub cache_dir: PathBuf,
    /// GitHub OAuth token (from `GITHUB_TOKEN` or config).
    pub github_token: Option<String>,
    /// GitLab OAuth token.
    pub gitlab_token: Option<String>,
    /// Bitbucket OAuth consumer key/secret.
    pub bitbucket_oauth: Option<(String, String)>,
    /// Forgejo token.
    pub forgejo_token: Option<String>,
    /// Custom GitLab domains (for self-hosted).
    pub gitlab_domains: Vec<String>,
    /// Custom Forgejo domains (for self-hosted).
    pub forgejo_domains: Vec<String>,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from(".cache/mozart/vcs"),
            github_token: None,
            gitlab_token: None,
            bitbucket_oauth: None,
            forgejo_token: None,
            gitlab_domains: vec!["gitlab.com".to_string()],
            forgejo_domains: vec!["codeberg.org".to_string()],
        }
    }
}

/// Type of VCS driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverType {
    GitHub,
    GitLab,
    Bitbucket,
    Forgejo,
    Git,
    Svn,
    Hg,
}

/// The VCS driver interface.
///
/// Corresponds to Composer's `VcsDriverInterface`.
pub trait VcsDriver {
    /// Initialize the driver (e.g., clone mirror, fetch API metadata).
    fn initialize(&mut self) -> Result<()>;

    /// The root identifier (default branch/trunk).
    fn root_identifier(&self) -> &str;

    /// All branches as `name -> commit_hash`.
    fn branches(&mut self) -> Result<&BTreeMap<String, String>>;

    /// All tags as `name -> commit_hash`.
    fn tags(&mut self) -> Result<&BTreeMap<String, String>>;

    /// Get composer.json content parsed as JSON for a given identifier.
    fn composer_information(&mut self, identifier: &str) -> Result<Option<serde_json::Value>>;

    /// Get raw file content at a given path and identifier.
    fn file_content(&self, file: &str, identifier: &str) -> Result<Option<String>>;

    /// Get the change date for a given identifier (ISO 8601).
    fn change_date(&self, identifier: &str) -> Result<Option<String>>;

    /// Get the dist reference for a given identifier.
    fn dist(&self, identifier: &str) -> Result<Option<DistReference>>;

    /// Get the source reference for a given identifier.
    fn source(&self, identifier: &str) -> SourceReference;

    /// The canonical URL of this repository.
    fn url(&self) -> &str;

    /// Clean up resources (temp dirs, etc.).
    fn cleanup(&mut self) -> Result<()>;
}

/// Detect which driver type should handle a given URL.
///
/// Priority order matches Composer:
/// 1. GitHub → 2. GitLab → 3. Bitbucket → 4. Forgejo → 5. Git → 6. Hg → 7. SVN
pub fn detect_driver(
    url: &str,
    forced_type: Option<&str>,
    config: &DriverConfig,
) -> Option<DriverType> {
    if let Some(t) = forced_type {
        return match t {
            "github" => Some(DriverType::GitHub),
            "gitlab" => Some(DriverType::GitLab),
            "bitbucket" => Some(DriverType::Bitbucket),
            "forgejo" => Some(DriverType::Forgejo),
            "git" => Some(DriverType::Git),
            "svn" => Some(DriverType::Svn),
            "hg" | "mercurial" => Some(DriverType::Hg),
            _ => None,
        };
    }

    let url_lower = url.to_lowercase();

    // GitHub
    if github::GitHubDriver::supports(url) {
        return Some(DriverType::GitHub);
    }

    // GitLab
    if gitlab::GitLabDriver::supports(url, &config.gitlab_domains) {
        return Some(DriverType::GitLab);
    }

    // Bitbucket
    if bitbucket::BitbucketDriver::supports(url) {
        return Some(DriverType::Bitbucket);
    }

    // Forgejo
    if forgejo::ForgejoDriver::supports(url, &config.forgejo_domains) {
        return Some(DriverType::Forgejo);
    }

    // Git
    if git::GitDriver::supports(url) {
        return Some(DriverType::Git);
    }

    // Hg
    if hg::HgDriver::supports(url) {
        return Some(DriverType::Hg);
    }

    // SVN
    if url_lower.contains("svn") || svn::SvnDriver::supports(url) {
        return Some(DriverType::Svn);
    }

    // Default to git for generic URLs
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(DriverType::Git);
    }

    None
}

/// Create a driver instance for the given URL and type.
pub fn create_driver(
    url: &str,
    driver_type: DriverType,
    config: DriverConfig,
) -> Box<dyn VcsDriver> {
    match driver_type {
        DriverType::GitHub => Box::new(github::GitHubDriver::new(url, config)),
        DriverType::GitLab => Box::new(gitlab::GitLabDriver::new(url, config)),
        DriverType::Bitbucket => Box::new(bitbucket::BitbucketDriver::new(url, config)),
        DriverType::Forgejo => Box::new(forgejo::ForgejoDriver::new(url, config)),
        DriverType::Git => Box::new(git::GitDriver::new(url, config)),
        DriverType::Svn => Box::new(svn::SvnDriver::new(url, config)),
        DriverType::Hg => Box::new(hg::HgDriver::new(url, config)),
    }
}
