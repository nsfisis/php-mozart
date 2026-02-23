use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};

use super::git::GitDriver;
use super::{DistReference, DriverConfig, SourceReference, VcsDriver};

/// Bitbucket VCS driver using the REST API 2.0.
pub struct BitbucketDriver {
    owner: String,
    repo: String,
    url: String,
    root_identifier: Option<String>,
    tags: Option<BTreeMap<String, String>>,
    branches: Option<BTreeMap<String, String>>,
    info_cache: HashMap<String, Option<serde_json::Value>>,
    git_driver: Option<Box<GitDriver>>,
    http_client: Client,
    config: DriverConfig,
    api_failed: bool,
    vcs_type: String, // "git" or "hg"
}

impl BitbucketDriver {
    pub fn new(url: &str, config: DriverConfig) -> Self {
        let (owner, repo) = Self::parse_url(url).unwrap_or_default();
        Self {
            owner,
            repo,
            url: url.to_string(),
            root_identifier: None,
            tags: None,
            branches: None,
            info_cache: HashMap::new(),
            git_driver: None,
            http_client: Client::new(),
            config,
            api_failed: false,
            vcs_type: "git".to_string(),
        }
    }

    pub fn supports(url: &str) -> bool {
        let url_lower = url.to_lowercase();
        url_lower.contains("bitbucket.org")
    }

    fn parse_url(url: &str) -> Option<(String, String)> {
        let re =
            Regex::new(r"bitbucket\.org[:/]([^/]+)/([^/.\s]+?)(?:\.git)?(?:[/#?].*)?$").ok()?;
        let caps = re.captures(url)?;
        Some((caps[1].to_string(), caps[2].to_string()))
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "https://api.bitbucket.org/2.0/repositories/{}/{}{}",
            self.owner, self.repo, path,
        )
    }

    async fn api_get(&self, path: &str) -> Result<serde_json::Value> {
        let url = self.api_url(path);
        let mut req = self
            .http_client
            .get(&url)
            .header(USER_AGENT, "mozart/0.1")
            .header(ACCEPT, "application/json");

        if let Some((key, secret)) = &self.config.bitbucket_oauth {
            let credentials = format!("{key}:{secret}");
            req = req.header(AUTHORIZATION, format!("Basic {credentials}"));
        }

        let response = req.send().await?;
        if !response.status().is_success() {
            bail!(
                "Bitbucket API request to {} failed: {}",
                url,
                response.status()
            );
        }
        Ok(response.json().await?)
    }

    async fn api_get_paginated(&self, path: &str) -> Result<Vec<serde_json::Value>> {
        let mut items = Vec::new();
        let mut next_url = Some(self.api_url(path));
        let mut pages = 0;

        while let Some(url) = next_url {
            let mut req = self
                .http_client
                .get(&url)
                .header(USER_AGENT, "mozart/0.1")
                .header(ACCEPT, "application/json");
            if let Some((key, secret)) = &self.config.bitbucket_oauth {
                req = req.header(AUTHORIZATION, format!("Basic {key}:{secret}"));
            }
            let response = req.send().await?;
            if !response.status().is_success() {
                break;
            }
            let data: serde_json::Value = response.json().await?;
            if let Some(values) = data["values"].as_array() {
                items.extend(values.iter().cloned());
            }
            next_url = data["next"].as_str().map(|s: &str| s.to_string());
            pages += 1;
            if pages > 10 {
                break;
            }
        }
        Ok(items)
    }

    async fn use_git_fallback(&mut self) -> Result<&mut GitDriver> {
        if self.git_driver.is_none() {
            let git_url = format!("https://bitbucket.org/{}/{}.git", self.owner, self.repo);
            let mut driver = GitDriver::new(&git_url, self.config.clone());
            driver.initialize().await?;
            self.git_driver = Some(Box::new(driver));
        }
        Ok(self.git_driver.as_mut().unwrap())
    }
}

impl VcsDriver for BitbucketDriver {
    async fn initialize(&mut self) -> Result<()> {
        match self.api_get("").await {
            Ok(data) => {
                if let Some(scm) = data["scm"].as_str() {
                    self.vcs_type = scm.to_string();
                }
                let default_branch = data["mainbranch"]["name"]
                    .as_str()
                    .unwrap_or("main")
                    .to_string();
                self.root_identifier = Some(default_branch);
            }
            Err(_) => {
                self.api_failed = true;
                let driver = self.use_git_fallback().await?;
                self.root_identifier = Some(driver.root_identifier().to_string());
            }
        }
        Ok(())
    }

    fn root_identifier(&self) -> &str {
        self.root_identifier.as_deref().unwrap_or("main")
    }

    async fn branches(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.branches.is_none() {
            if self.api_failed {
                let driver = self.use_git_fallback().await?;
                let branches = driver.branches().await?.clone();
                self.branches = Some(branches);
            } else {
                let items = self.api_get_paginated("/refs/branches?pagelen=100").await?;
                let mut branches = BTreeMap::new();
                for item in items {
                    if let (Some(name), Some(sha)) =
                        (item["name"].as_str(), item["target"]["hash"].as_str())
                    {
                        branches.insert(name.to_string(), sha.to_string());
                    }
                }
                self.branches = Some(branches);
            }
        }
        Ok(self.branches.as_ref().unwrap())
    }

    async fn tags(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.tags.is_none() {
            if self.api_failed {
                let driver = self.use_git_fallback().await?;
                let tags = driver.tags().await?.clone();
                self.tags = Some(tags);
            } else {
                let items = self.api_get_paginated("/refs/tags?pagelen=100").await?;
                let mut tags = BTreeMap::new();
                for item in items {
                    if let (Some(name), Some(sha)) =
                        (item["name"].as_str(), item["target"]["hash"].as_str())
                    {
                        tags.insert(name.to_string(), sha.to_string());
                    }
                }
                self.tags = Some(tags);
            }
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
        if self.api_failed {
            return Ok(None);
        }
        let url = self.api_url(&format!("/src/{identifier}/{file}"));
        let mut req = self.http_client.get(&url).header(USER_AGENT, "mozart/0.1");
        if let Some((key, secret)) = &self.config.bitbucket_oauth {
            req = req.header(AUTHORIZATION, format!("Basic {key}:{secret}"));
        }
        let response = req.send().await?;
        if response.status().is_success() {
            Ok(Some(response.text().await?))
        } else {
            Ok(None)
        }
    }

    async fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        if self.api_failed {
            return Ok(None);
        }
        match self.api_get(&format!("/commit/{identifier}")).await {
            Ok(data) => Ok(data["date"].as_str().map(|s| s.to_string())),
            Err(_) => Ok(None),
        }
    }

    async fn dist(&self, identifier: &str) -> Result<Option<DistReference>> {
        Ok(Some(DistReference {
            dist_type: "zip".to_string(),
            url: format!(
                "https://bitbucket.org/{}/{}/get/{}.zip",
                self.owner, self.repo, identifier,
            ),
            reference: identifier.to_string(),
            shasum: None,
        }))
    }

    fn source(&self, identifier: &str) -> SourceReference {
        SourceReference {
            source_type: self.vcs_type.clone(),
            url: format!("https://bitbucket.org/{}/{}.git", self.owner, self.repo),
            reference: identifier.to_string(),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    async fn cleanup(&mut self) -> Result<()> {
        if let Some(driver) = &mut self.git_driver {
            driver.cleanup().await?;
        }
        Ok(())
    }
}
