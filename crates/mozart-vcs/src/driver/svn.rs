use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use regex::Regex;

use crate::process::ProcessExecutor;
use crate::util::svn::SvnUtil;

use super::{DistReference, DriverConfig, SourceReference, VcsDriver};

/// SVN VCS driver.
///
/// Corresponds to Composer's `Repository\Vcs\SvnDriver`.
pub struct SvnDriver {
    url: String,
    base_url: String,
    trunk_path: String,
    branches_path: String,
    tags_path: String,
    root_identifier: Option<String>,
    tags: Option<BTreeMap<String, String>>,
    branches: Option<BTreeMap<String, String>>,
    info_cache: HashMap<String, Option<serde_json::Value>>,
    svn_util: SvnUtil,
}

impl SvnDriver {
    pub fn new(url: &str, _config: DriverConfig) -> Self {
        let process = ProcessExecutor::new();
        Self {
            url: url.to_string(),
            base_url: url.to_string(),
            trunk_path: "trunk".to_string(),
            branches_path: "branches".to_string(),
            tags_path: "tags".to_string(),
            root_identifier: None,
            tags: None,
            branches: None,
            info_cache: HashMap::new(),
            svn_util: SvnUtil::new(process),
        }
    }

    pub fn supports(url: &str) -> bool {
        url.starts_with("svn://") || url.starts_with("svn+ssh://")
    }

    fn svn_info(&self, url: &str) -> Result<serde_json::Value> {
        let output = self.svn_util.execute(&["info", "--xml", url], None)?;
        // Parse minimal info from XML output
        let stdout = &output.stdout;
        let mut info = serde_json::Map::new();

        if let Some(rev) = extract_xml_attr(stdout, "entry", "revision") {
            info.insert("revision".to_string(), serde_json::Value::String(rev));
        }
        if let Some(url_val) = extract_xml_content(stdout, "url") {
            info.insert("url".to_string(), serde_json::Value::String(url_val));
        }
        if let Some(date) = extract_xml_content(stdout, "date") {
            info.insert("date".to_string(), serde_json::Value::String(date));
        }

        Ok(serde_json::Value::Object(info))
    }

    fn svn_ls(&self, url: &str) -> Result<Vec<String>> {
        let output = self.svn_util.execute(&["ls", url], None)?;
        Ok(ProcessExecutor::split_lines(&output.stdout)
            .into_iter()
            .map(|s| s.trim_end_matches('/').to_string())
            .collect())
    }
}

impl VcsDriver for SvnDriver {
    async fn initialize(&mut self) -> Result<()> {
        let info = self.svn_info(&self.url)?;
        if let Some(url) = info["url"].as_str() {
            self.base_url = url.to_string();
        }
        self.root_identifier = info["revision"].as_str().map(|s| s.to_string());
        Ok(())
    }

    fn root_identifier(&self) -> &str {
        self.root_identifier.as_deref().unwrap_or("HEAD")
    }

    async fn branches(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.branches.is_none() {
            let mut branches = BTreeMap::new();

            // Add trunk
            let trunk_url = format!("{}/{}", self.base_url, self.trunk_path);
            if let Ok(info) = self.svn_info(&trunk_url)
                && let Some(rev) = info["revision"].as_str()
            {
                branches.insert("trunk".to_string(), rev.to_string());
            }

            // List branches directory
            let branches_url = format!("{}/{}", self.base_url, self.branches_path);
            if let Ok(items) = self.svn_ls(&branches_url) {
                for name in items {
                    let branch_url = format!("{}/{}", branches_url, name);
                    if let Ok(info) = self.svn_info(&branch_url)
                        && let Some(rev) = info["revision"].as_str()
                    {
                        branches.insert(name, rev.to_string());
                    }
                }
            }

            self.branches = Some(branches);
        }
        Ok(self.branches.as_ref().unwrap())
    }

    async fn tags(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.tags.is_none() {
            let mut tags = BTreeMap::new();
            let tags_url = format!("{}/{}", self.base_url, self.tags_path);
            if let Ok(items) = self.svn_ls(&tags_url) {
                for name in items {
                    let tag_url = format!("{}/{}", tags_url, name);
                    if let Ok(info) = self.svn_info(&tag_url)
                        && let Some(rev) = info["revision"].as_str()
                    {
                        tags.insert(name, rev.to_string());
                    }
                }
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
        // identifier is either a path (trunk, branches/x, tags/y) or a revision number
        let url = if identifier.contains('/') || identifier == "trunk" {
            format!("{}/{}/{}", self.base_url, identifier, file)
        } else {
            format!(
                "{}/{}/{}@{}",
                self.base_url, self.trunk_path, file, identifier
            )
        };
        let output = self.svn_util.execute(&["cat", &url], None);
        match output {
            Ok(o) if !o.stdout.is_empty() => Ok(Some(o.stdout)),
            _ => Ok(None),
        }
    }

    async fn change_date(&self, identifier: &str) -> Result<Option<String>> {
        let url = if identifier.contains('/') || identifier == "trunk" {
            format!("{}/{}", self.base_url, identifier)
        } else {
            format!("{}@{}", self.base_url, identifier)
        };
        match self.svn_info(&url) {
            Ok(info) => Ok(info["date"].as_str().map(|s| s.to_string())),
            Err(_) => Ok(None),
        }
    }

    async fn dist(&self, _identifier: &str) -> Result<Option<DistReference>> {
        // SVN doesn't provide dist archives
        Ok(None)
    }

    fn source(&self, identifier: &str) -> SourceReference {
        SourceReference {
            source_type: "svn".to_string(),
            url: self.base_url.clone(),
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

/// Extract an XML attribute value from a simple XML string.
fn extract_xml_attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let pattern = format!(r#"<{tag}\s[^>]*{attr}="([^"]*)"#);
    let re = Regex::new(&pattern).ok()?;
    re.captures(xml).map(|c| c[1].to_string())
}

/// Extract text content between XML tags.
fn extract_xml_content(xml: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"<{tag}>([^<]*)</{tag}>");
    let re = Regex::new(&pattern).ok()?;
    re.captures(xml).map(|c| c[1].to_string())
}
