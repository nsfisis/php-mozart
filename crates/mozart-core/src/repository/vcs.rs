mod forgejo_driver;
mod fossil_driver;
mod git_bitbucket_driver;
mod git_driver;
mod github_driver;
mod gitlab_driver;
mod hg_driver;
mod perforce_driver;
mod svn_driver;
mod vcs_driver;
mod vcs_driver_interface;

pub use forgejo_driver::*;
pub use fossil_driver::*;
pub use git_bitbucket_driver::*;
pub use git_driver::*;
pub use github_driver::*;
pub use gitlab_driver::*;
pub use hg_driver::*;
pub use perforce_driver::*;
pub use svn_driver::*;
pub use vcs_driver::*;
pub use vcs_driver_interface::*;

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
    /// Composer's `cache-vcs-dir`: root for VCS mirrors, one
    /// subdirectory per sanitized repository URL.
    pub cache_vcs_dir: PathBuf,
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
            cache_vcs_dir: default_cache_vcs_dir(),
            github_token: None,
            gitlab_token: None,
            bitbucket_oauth: None,
            forgejo_token: None,
            gitlab_domains: vec!["gitlab.com".to_string()],
            forgejo_domains: vec!["codeberg.org".to_string()],
        }
    }
}

/// Resolve the default `cache-vcs-dir`, honoring Composer's env vars.
///
/// Priority: `COMPOSER_CACHE_VCS_DIR` → `COMPOSER_CACHE_DIR/vcs` →
/// `XDG_CACHE_HOME/mozart/vcs` → `$HOME/.cache/mozart/vcs`.
fn default_cache_vcs_dir() -> PathBuf {
    if let Ok(p) = std::env::var("COMPOSER_CACHE_VCS_DIR") {
        return PathBuf::from(p);
    }
    let base = if let Ok(p) = std::env::var("COMPOSER_CACHE_DIR") {
        PathBuf::from(p)
    } else if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("mozart")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache").join("mozart")
    } else {
        PathBuf::from("/tmp").join("mozart")
    };
    base.join("vcs")
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

/// Enum-dispatched VCS driver.
///
/// Wraps all concrete driver types to allow static dispatch with async trait methods.
pub enum AnyVcsDriver {
    GitHub(GitHubDriver),
    GitLab(GitLabDriver),
    Bitbucket(GitBitbucketDriver),
    Forgejo(ForgejoDriver),
    Git(GitDriver),
    Svn(SvnDriver),
    Hg(HgDriver),
}

macro_rules! dispatch {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            AnyVcsDriver::GitHub(d) => d.$method($($arg),*),
            AnyVcsDriver::GitLab(d) => d.$method($($arg),*),
            AnyVcsDriver::Bitbucket(d) => d.$method($($arg),*),
            AnyVcsDriver::Forgejo(d) => d.$method($($arg),*),
            AnyVcsDriver::Git(d) => d.$method($($arg),*),
            AnyVcsDriver::Svn(d) => d.$method($($arg),*),
            AnyVcsDriver::Hg(d) => d.$method($($arg),*),
        }
    };
}

macro_rules! dispatch_async {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            AnyVcsDriver::GitHub(d) => d.$method($($arg),*).await,
            AnyVcsDriver::GitLab(d) => d.$method($($arg),*).await,
            AnyVcsDriver::Bitbucket(d) => d.$method($($arg),*).await,
            AnyVcsDriver::Forgejo(d) => d.$method($($arg),*).await,
            AnyVcsDriver::Git(d) => d.$method($($arg),*).await,
            AnyVcsDriver::Svn(d) => d.$method($($arg),*).await,
            AnyVcsDriver::Hg(d) => d.$method($($arg),*).await,
        }
    };
}

impl AnyVcsDriver {
    pub async fn initialize(&mut self) -> Result<()> {
        dispatch_async!(self, initialize)
    }

    pub fn root_identifier(&self) -> &str {
        dispatch!(self, root_identifier)
    }

    pub async fn branches(&mut self) -> Result<&BTreeMap<String, String>> {
        dispatch_async!(self, branches)
    }

    pub async fn tags(&mut self) -> Result<&BTreeMap<String, String>> {
        dispatch_async!(self, tags)
    }

    pub async fn composer_information(
        &mut self,
        identifier: &str,
    ) -> Result<Option<serde_json::Value>> {
        dispatch_async!(self, composer_information, identifier)
    }

    pub async fn file_content(&self, file: &str, identifier: &str) -> Result<Option<String>> {
        dispatch_async!(self, file_content, file, identifier)
    }

    pub async fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        dispatch_async!(self, change_date, identifier)
    }

    pub async fn dist(&self, identifier: &str) -> Result<Option<DistReference>> {
        dispatch_async!(self, dist, identifier)
    }

    pub fn source(&self, identifier: &str) -> SourceReference {
        dispatch!(self, source, identifier)
    }

    pub fn url(&self) -> &str {
        dispatch!(self, url)
    }

    pub async fn cleanup(&mut self) -> Result<()> {
        dispatch_async!(self, cleanup)
    }
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
    if GitHubDriver::supports(url) {
        return Some(DriverType::GitHub);
    }

    // GitLab
    if GitLabDriver::supports(url, &config.gitlab_domains) {
        return Some(DriverType::GitLab);
    }

    // Bitbucket
    if GitBitbucketDriver::supports(url) {
        return Some(DriverType::Bitbucket);
    }

    // Forgejo
    if ForgejoDriver::supports(url, &config.forgejo_domains) {
        return Some(DriverType::Forgejo);
    }

    // Git
    if GitDriver::supports(url) {
        return Some(DriverType::Git);
    }

    // Hg
    if HgDriver::supports(url) {
        return Some(DriverType::Hg);
    }

    // SVN
    if url_lower.contains("svn") || SvnDriver::supports(url) {
        return Some(DriverType::Svn);
    }

    // Default to git for generic URLs
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(DriverType::Git);
    }

    None
}

/// Create a driver instance for the given URL and type.
pub fn create_driver(url: &str, driver_type: DriverType, config: DriverConfig) -> AnyVcsDriver {
    match driver_type {
        DriverType::GitHub => AnyVcsDriver::GitHub(GitHubDriver::new(url, config)),
        DriverType::GitLab => AnyVcsDriver::GitLab(GitLabDriver::new(url, config)),
        DriverType::Bitbucket => AnyVcsDriver::Bitbucket(GitBitbucketDriver::new(url, config)),
        DriverType::Forgejo => AnyVcsDriver::Forgejo(ForgejoDriver::new(url, config)),
        DriverType::Git => AnyVcsDriver::Git(GitDriver::new(url, config)),
        DriverType::Svn => AnyVcsDriver::Svn(SvnDriver::new(url, config)),
        DriverType::Hg => AnyVcsDriver::Hg(HgDriver::new(url, config)),
    }
}
