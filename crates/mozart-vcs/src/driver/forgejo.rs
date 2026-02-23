use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};

use super::git::GitDriver;
use super::{DistReference, DriverConfig, SourceReference, VcsDriver};

/// Forgejo/Gitea VCS driver using the REST API v1.
///
/// Supports self-hosted instances (Codeberg, etc.).
pub struct ForgejoDriver {
    owner: String,
    repo: String,
    host: String,
    scheme: String,
    url: String,
    root_identifier: Option<String>,
    tags: Option<BTreeMap<String, String>>,
    branches: Option<BTreeMap<String, String>>,
    info_cache: HashMap<String, Option<serde_json::Value>>,
    git_driver: Option<Box<GitDriver>>,
    http_client: Client,
    config: DriverConfig,
    api_failed: bool,
}

impl ForgejoDriver {
    pub fn new(url: &str, config: DriverConfig) -> Self {
        let (host, scheme, owner, repo) = Self::parse_url(url).unwrap_or_default();
        Self {
            owner,
            repo,
            host,
            scheme,
            url: url.to_string(),
            root_identifier: None,
            tags: None,
            branches: None,
            info_cache: HashMap::new(),
            git_driver: None,
            http_client: Client::new(),
            config,
            api_failed: false,
        }
    }

    pub fn supports(url: &str, forgejo_domains: &[String]) -> bool {
        let url_lower = url.to_lowercase();
        for domain in forgejo_domains {
            if url_lower.contains(domain) {
                return true;
            }
        }
        false
    }

    fn parse_url(url: &str) -> Option<(String, String, String, String)> {
        let re = Regex::new(r"(?i)(https?)://([^/]+)/([^/]+)/([^/.\s]+?)(?:\.git)?(?:[/#?].*)?$")
            .ok()?;
        let caps = re.captures(url)?;
        Some((
            caps[2].to_string(),
            caps[1].to_string(),
            caps[3].to_string(),
            caps[4].to_string(),
        ))
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "{}://{}/api/v1/repos/{}/{}{}",
            self.scheme, self.host, self.owner, self.repo, path,
        )
    }

    fn api_get(&self, path: &str) -> Result<serde_json::Value> {
        let handle = tokio::runtime::Handle::current();
        let url = self.api_url(path);
        let mut req = self
            .http_client
            .get(&url)
            .header(USER_AGENT, "mozart/0.1")
            .header(ACCEPT, "application/json");
        if let Some(token) = &self.config.forgejo_token {
            req = req.header(AUTHORIZATION, format!("token {token}"));
        }
        let response = handle.block_on(req.send())?;
        if !response.status().is_success() {
            bail!(
                "Forgejo API request to {} failed: {}",
                url,
                response.status()
            );
        }
        Ok(handle.block_on(response.json())?)
    }

    fn api_get_paginated(&self, path: &str) -> Result<Vec<serde_json::Value>> {
        let mut items = Vec::new();
        let mut page = 1;
        loop {
            let sep = if path.contains('?') { "&" } else { "?" };
            let paged_path = format!("{path}{sep}limit=50&page={page}");
            let data = self.api_get(&paged_path)?;
            let batch: Vec<serde_json::Value> = match data {
                serde_json::Value::Array(arr) => arr,
                _ => break,
            };
            if batch.is_empty() {
                break;
            }
            items.extend(batch);
            page += 1;
            if page > 20 {
                break;
            }
        }
        Ok(items)
    }

    fn use_git_fallback(&mut self) -> Result<&mut GitDriver> {
        if self.git_driver.is_none() {
            let git_url = format!(
                "{}://{}/{}/{}.git",
                self.scheme, self.host, self.owner, self.repo
            );
            let mut driver = GitDriver::new(&git_url, self.config.clone());
            driver.initialize()?;
            self.git_driver = Some(Box::new(driver));
        }
        Ok(self.git_driver.as_mut().unwrap())
    }
}

impl VcsDriver for ForgejoDriver {
    fn initialize(&mut self) -> Result<()> {
        match self.api_get("") {
            Ok(data) => {
                let default_branch = data["default_branch"]
                    .as_str()
                    .unwrap_or("main")
                    .to_string();
                self.root_identifier = Some(default_branch);
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
                        (item["name"].as_str(), item["commit"]["id"].as_str())
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
                    if let (Some(name), Some(sha)) = (
                        item["name"].as_str(),
                        item["id"].as_str().or(item["commit"]["sha"].as_str()),
                    ) {
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
        let value = content.and_then(|c| serde_json::from_str(&c).ok());
        self.info_cache
            .insert(identifier.to_string(), value.clone());
        Ok(value)
    }

    fn file_content(&self, file: &str, identifier: &str) -> Result<Option<String>> {
        if self.api_failed {
            return Ok(None);
        }
        let path = format!("/contents/{}?ref={}", file, identifier);
        match self.api_get(&path) {
            Ok(data) => {
                if let Some(content) = data["content"].as_str() {
                    // Forgejo returns base64-encoded content
                    let decoded = super::github::base64_decode_content(content)?;
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
        match self.api_get(&format!("/git/commits/{identifier}")) {
            Ok(data) => Ok(data["created"].as_str().map(|s| s.to_string())),
            Err(_) => Ok(None),
        }
    }

    fn dist(&self, identifier: &str) -> Result<Option<DistReference>> {
        Ok(Some(DistReference {
            dist_type: "zip".to_string(),
            url: format!(
                "{}://{}/{}/{}/archive/{}.zip",
                self.scheme, self.host, self.owner, self.repo, identifier,
            ),
            reference: identifier.to_string(),
            shasum: None,
        }))
    }

    fn source(&self, identifier: &str) -> SourceReference {
        SourceReference {
            source_type: "git".to_string(),
            url: format!(
                "{}://{}/{}/{}.git",
                self.scheme, self.host, self.owner, self.repo
            ),
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
