use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};

use super::git::GitDriver;
use super::{DistReference, DriverConfig, SourceReference, VcsDriver};

/// GitHub VCS driver using the REST API v3.
///
/// Falls back to `GitDriver` when API access fails.
pub struct GitHubDriver {
    owner: String,
    repo: String,
    url: String,
    root_identifier: Option<String>,
    tags: Option<BTreeMap<String, String>>,
    branches: Option<BTreeMap<String, String>>,
    repo_data: Option<serde_json::Value>,
    info_cache: HashMap<String, Option<serde_json::Value>>,
    git_driver: Option<Box<GitDriver>>,
    http_client: Client,
    config: DriverConfig,
    api_failed: bool,
}

impl GitHubDriver {
    pub fn new(url: &str, config: DriverConfig) -> Self {
        let (owner, repo) = Self::parse_url(url).unwrap_or_default();
        Self {
            owner,
            repo,
            url: url.to_string(),
            root_identifier: None,
            tags: None,
            branches: None,
            repo_data: None,
            info_cache: HashMap::new(),
            git_driver: None,
            http_client: Client::new(),
            config,
            api_failed: false,
        }
    }

    /// Check if a URL points to GitHub.
    pub fn supports(url: &str) -> bool {
        let url_lower = url.to_lowercase();
        url_lower.contains("github.com")
            && (url_lower.contains("github.com/") || url_lower.contains("github.com:"))
    }

    fn parse_url(url: &str) -> Option<(String, String)> {
        let re = Regex::new(r"github\.com[:/]([^/]+)/([^/.\s]+?)(?:\.git)?(?:[/#?].*)?$").ok()?;
        let caps = re.captures(url)?;
        Some((caps[1].to_string(), caps[2].to_string()))
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "https://api.github.com/repos/{}/{}{}",
            self.owner, self.repo, path
        )
    }

    fn api_get(&self, path: &str) -> Result<serde_json::Value> {
        let handle = tokio::runtime::Handle::current();
        let url = self.api_url(path);
        let mut req = self
            .http_client
            .get(&url)
            .header(USER_AGENT, "mozart/0.1")
            .header(ACCEPT, "application/vnd.github.v3+json");

        if let Some(token) = &self.config.github_token {
            req = req.header(AUTHORIZATION, format!("token {token}"));
        }

        let response = handle.block_on(req.send())?;
        if !response.status().is_success() {
            bail!(
                "GitHub API request to {} failed with status {}",
                url,
                response.status()
            );
        }
        Ok(handle.block_on(response.json())?)
    }

    fn api_get_paginated(&self, path: &str) -> Result<Vec<serde_json::Value>> {
        let handle = tokio::runtime::Handle::current();
        let mut items = Vec::new();
        let mut page = 1;
        loop {
            let separator = if path.contains('?') { "&" } else { "?" };
            let url = format!(
                "https://api.github.com/repos/{}/{}{}{}per_page=100&page={}",
                self.owner, self.repo, path, separator, page,
            );
            let mut req = self
                .http_client
                .get(&url)
                .header(USER_AGENT, "mozart/0.1")
                .header(ACCEPT, "application/vnd.github.v3+json");
            if let Some(token) = &self.config.github_token {
                req = req.header(AUTHORIZATION, format!("token {token}"));
            }

            let response = handle.block_on(req.send())?;
            if !response.status().is_success() {
                bail!("GitHub API paginated request failed: {}", response.status());
            }

            let batch: Vec<serde_json::Value> = handle.block_on(response.json())?;
            if batch.is_empty() {
                break;
            }
            items.extend(batch);
            page += 1;
            // Safety: limit to 10 pages (1000 items)
            if page > 10 {
                break;
            }
        }
        Ok(items)
    }

    fn use_git_fallback(&mut self) -> Result<&mut GitDriver> {
        if self.git_driver.is_none() {
            let git_url = format!("https://github.com/{}/{}.git", self.owner, self.repo);
            let mut driver = GitDriver::new(&git_url, self.config.clone());
            driver.initialize()?;
            self.git_driver = Some(Box::new(driver));
        }
        Ok(self.git_driver.as_mut().unwrap())
    }
}

impl VcsDriver for GitHubDriver {
    fn initialize(&mut self) -> Result<()> {
        // Try to fetch repo data from API
        match self.api_get("") {
            Ok(data) => {
                let default_branch = data["default_branch"]
                    .as_str()
                    .unwrap_or("main")
                    .to_string();
                self.root_identifier = Some(default_branch);
                self.repo_data = Some(data);
            }
            Err(_) => {
                self.api_failed = true;
                let driver = self.use_git_fallback()?;
                self.root_identifier = Some(driver.root_identifier().to_string());
            }
        }
        Ok(())
    }

    fn root_identifier(&self) -> &str {
        self.root_identifier.as_deref().unwrap_or("main")
    }

    fn branches(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.branches.is_none() {
            if self.api_failed {
                let driver = self.use_git_fallback()?;
                let branches = driver.branches()?.clone();
                self.branches = Some(branches);
            } else {
                let items = self.api_get_paginated("/branches")?;
                let mut branches = BTreeMap::new();
                for item in items {
                    if let (Some(name), Some(sha)) =
                        (item["name"].as_str(), item["commit"]["sha"].as_str())
                    {
                        branches.insert(name.to_string(), sha.to_string());
                    }
                }
                self.branches = Some(branches);
            }
        }
        Ok(self.branches.as_ref().unwrap())
    }

    fn tags(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.tags.is_none() {
            if self.api_failed {
                let driver = self.use_git_fallback()?;
                let tags = driver.tags()?.clone();
                self.tags = Some(tags);
            } else {
                let items = self.api_get_paginated("/tags")?;
                let mut tags = BTreeMap::new();
                for item in items {
                    if let (Some(name), Some(sha)) =
                        (item["name"].as_str(), item["commit"]["sha"].as_str())
                    {
                        tags.insert(name.to_string(), sha.to_string());
                    }
                }
                self.tags = Some(tags);
            }
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
        if self.api_failed {
            // Can't use API, would need git fallback
            // For simplicity, return None (git_driver is mutable)
            return Ok(None);
        }

        let path = format!("/contents/{}?ref={}", file, identifier);
        match self.api_get(&path) {
            Ok(data) => {
                if let Some(content) = data["content"].as_str() {
                    // GitHub returns base64-encoded content
                    let decoded = base64_decode_content(content)?;
                    Ok(Some(decoded))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        if self.api_failed {
            return Ok(None);
        }

        let path = format!("/commits/{}", identifier);
        match self.api_get(&path) {
            Ok(data) => {
                let date = data["commit"]["committer"]["date"]
                    .as_str()
                    .map(|s| s.to_string());
                Ok(date)
            }
            Err(_) => Ok(None),
        }
    }

    fn dist(&self, identifier: &str) -> Result<Option<DistReference>> {
        Ok(Some(DistReference {
            dist_type: "zip".to_string(),
            url: format!(
                "https://api.github.com/repos/{}/{}/zipball/{}",
                self.owner, self.repo, identifier,
            ),
            reference: identifier.to_string(),
            shasum: None,
        }))
    }

    fn source(&self, identifier: &str) -> SourceReference {
        SourceReference {
            source_type: "git".to_string(),
            url: format!("https://github.com/{}/{}.git", self.owner, self.repo),
            reference: identifier.to_string(),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn cleanup(&mut self) -> Result<()> {
        if let Some(driver) = &mut self.git_driver {
            driver.cleanup()?;
        }
        Ok(())
    }
}

/// Decode base64-encoded content from API responses.
/// Also used by Forgejo driver as `base64_decode_content`.
pub fn base64_decode_content(input: &str) -> Result<String> {
    use base64::Engine;
    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'\n' && b != b'\r')
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .map_err(|e| anyhow::anyhow!("Base64 decode error: {e}"))?;
    String::from_utf8(decoded).map_err(|e| anyhow::anyhow!("Invalid UTF-8 in base64 content: {e}"))
}
