use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
pub struct PackagistDist {
    #[serde(rename = "type")]
    pub dist_type: String,
    pub url: String,
    pub reference: Option<String>,
    pub shasum: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackagistSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: String,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackagistVersion {
    pub version: String,
    pub version_normalized: String,
    #[serde(default)]
    pub require: BTreeMap<String, String>,
    #[serde(default)]
    pub replace: BTreeMap<String, String>,
    #[serde(default)]
    pub provide: BTreeMap<String, String>,
    #[serde(default)]
    pub conflict: BTreeMap<String, String>,
    pub dist: Option<PackagistDist>,
    pub source: Option<PackagistSource>,

    #[serde(rename = "require-dev", default)]
    pub require_dev: BTreeMap<String, String>,

    #[serde(default)]
    pub suggest: Option<BTreeMap<String, String>>,

    #[serde(rename = "type")]
    pub package_type: Option<String>,

    pub autoload: Option<serde_json::Value>,

    #[serde(rename = "autoload-dev")]
    pub autoload_dev: Option<serde_json::Value>,

    pub license: Option<Vec<String>>,

    pub description: Option<String>,

    pub homepage: Option<String>,

    pub keywords: Option<Vec<String>>,

    pub authors: Option<Vec<serde_json::Value>>,

    pub support: Option<serde_json::Value>,

    pub funding: Option<Vec<serde_json::Value>>,

    pub time: Option<String>,

    pub extra: Option<serde_json::Value>,

    #[serde(rename = "notification-url")]
    pub notification_url: Option<String>,
}

impl PackagistVersion {
    /// Extract the `extra.branch-alias` map from this version's metadata.
    ///
    /// Composer packages can declare branch aliases in `extra.branch-alias`:
    /// ```json
    /// {
    ///   "extra": {
    ///     "branch-alias": {
    ///       "dev-master": "2.x-dev"
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Returns a map from branch name (e.g. `"dev-master"`) to alias target
    /// (e.g. `"2.x-dev"`). Returns an empty map when no aliases are declared.
    pub fn branch_aliases(&self) -> BTreeMap<String, String> {
        let Some(extra) = &self.extra else {
            return BTreeMap::new();
        };

        let Some(branch_alias) = extra.get("branch-alias") else {
            return BTreeMap::new();
        };

        let Some(map) = branch_alias.as_object() else {
            return BTreeMap::new();
        };

        map.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()
    }
}

/// Parse a Packagist p2 API JSON response.
///
/// The response format is: `{"packages": {"vendor/package": [...]}}`.
pub fn parse_p2_response(json: &str, package_name: &str) -> anyhow::Result<Vec<PackagistVersion>> {
    #[derive(Deserialize)]
    struct P2Response {
        packages: BTreeMap<String, Vec<PackagistVersion>>,
    }

    let response: P2Response = serde_json::from_str(json)?;
    response
        .packages
        .into_iter()
        .find(|(key, _)| key == package_name)
        .map(|(_, versions)| versions)
        .ok_or_else(|| anyhow::anyhow!("Package \"{package_name}\" not found in response"))
}

/// Fetch package version metadata from the Packagist p2 API.
pub fn fetch_package_versions(package_name: &str) -> anyhow::Result<Vec<PackagistVersion>> {
    let url = format!("https://repo.packagist.org/p2/{package_name}.json");
    let response = reqwest::blocking::get(&url)?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Failed to fetch package \"{package_name}\" from Packagist (HTTP {})",
            response.status()
        );
    }

    let body = response.text()?;
    parse_p2_response(&body, package_name)
}

// ─────────────────────────────────────────────────────────────────────────────
// Packagist search API
// ─────────────────────────────────────────────────────────────────────────────

/// A single search result from the Packagist search API.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SearchResult {
    pub name: String,
    pub description: String,
    pub url: String,
    pub repository: Option<String>,
    pub downloads: u64,
    pub favers: u64,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total: u64,
    pub next: Option<String>,
}

/// Maximum number of pages to fetch from the Packagist search API.
const SEARCH_MAX_PAGES: usize = 20;

/// Percent-encode a string for use in a URL query parameter value.
fn url_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push_str("%20"),
            other => {
                encoded.push_str(&format!("%{other:02X}"));
            }
        }
    }
    encoded
}

/// Search Packagist for packages matching `query`.
///
/// Fetches up to `SEARCH_MAX_PAGES` pages of results and returns the full list.
/// An optional `package_type` filter can narrow results (e.g. `"library"`).
pub fn search_packages(
    query: &str,
    package_type: Option<&str>,
) -> anyhow::Result<(Vec<SearchResult>, u64)> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("mozart/0.1.0")
        .build()?;

    let mut all_results: Vec<SearchResult> = Vec::new();
    let mut page = 1usize;
    let mut next_url: Option<String> = None;
    let mut total: u64 = 0;

    loop {
        let response: SearchResponse = if let Some(ref url) = next_url {
            let resp = client.get(url).send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Packagist search request failed (HTTP {})", resp.status());
            }
            resp.json()?
        } else {
            let encoded_query = url_encode(query);
            let mut url = format!("https://packagist.org/search.json?q={encoded_query}");
            if let Some(t) = package_type {
                url.push_str("&type=");
                url.push_str(&url_encode(t));
            }

            let resp = client.get(&url).send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Packagist search request failed (HTTP {})", resp.status());
            }
            resp.json()?
        };

        if page == 1 {
            total = response.total;
        }

        all_results.extend(response.results);
        next_url = response.next;
        page += 1;

        if next_url.is_none() || page > SEARCH_MAX_PAGES {
            break;
        }
    }

    Ok((all_results, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_p2_response_basic() {
        let json = r#"{
            "packages": {
                "monolog/monolog": [
                    {
                        "version": "3.8.0",
                        "version_normalized": "3.8.0.0",
                        "require": {"php": ">=8.1"},
                        "dist": {
                            "type": "zip",
                            "url": "https://example.com/monolog-3.8.0.zip",
                            "reference": "abc123",
                            "shasum": ""
                        },
                        "source": {
                            "type": "git",
                            "url": "https://github.com/Seldaek/monolog.git",
                            "reference": "abc123"
                        }
                    },
                    {
                        "version": "3.7.0",
                        "version_normalized": "3.7.0.0",
                        "require": {"php": ">=8.1"}
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "monolog/monolog").unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, "3.8.0");
        assert_eq!(versions[0].version_normalized, "3.8.0.0");
        assert_eq!(versions[0].require.get("php").unwrap(), ">=8.1");
        assert!(versions[0].dist.is_some());
        assert!(versions[0].source.is_some());
        assert_eq!(versions[1].version, "3.7.0");
        assert!(versions[1].dist.is_none());
    }

    #[test]
    fn parse_p2_response_not_found() {
        let json = r#"{"packages": {"other/pkg": []}}"#;
        let result = parse_p2_response(json, "monolog/monolog");
        assert!(result.is_err());
    }

    #[test]
    fn parse_p2_response_with_dev_version() {
        let json = r#"{
            "packages": {
                "test/pkg": [
                    {
                        "version": "dev-master",
                        "version_normalized": "dev-master",
                        "require": {}
                    },
                    {
                        "version": "1.0.0",
                        "version_normalized": "1.0.0.0",
                        "require": {}
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "test/pkg").unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, "dev-master");
        assert_eq!(versions[1].version, "1.0.0");
    }

    // ──────────── branch_aliases() tests ────────────

    #[test]
    fn test_branch_aliases_present() {
        let json = r#"{
            "packages": {
                "test/pkg": [
                    {
                        "version": "dev-master",
                        "version_normalized": "dev-master",
                        "require": {},
                        "extra": {
                            "branch-alias": {
                                "dev-master": "2.x-dev"
                            }
                        }
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "test/pkg").unwrap();
        let aliases = versions[0].branch_aliases();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases.get("dev-master").unwrap(), "2.x-dev");
    }

    #[test]
    fn test_branch_aliases_multiple() {
        let json = r#"{
            "packages": {
                "test/pkg": [
                    {
                        "version": "dev-master",
                        "version_normalized": "dev-master",
                        "require": {},
                        "extra": {
                            "branch-alias": {
                                "dev-master": "2.x-dev",
                                "dev-1.x": "1.5.x-dev"
                            }
                        }
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "test/pkg").unwrap();
        let aliases = versions[0].branch_aliases();
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases.get("dev-master").unwrap(), "2.x-dev");
        assert_eq!(aliases.get("dev-1.x").unwrap(), "1.5.x-dev");
    }

    #[test]
    fn test_branch_aliases_no_extra() {
        let json = r#"{
            "packages": {
                "test/pkg": [
                    {
                        "version": "dev-master",
                        "version_normalized": "dev-master",
                        "require": {}
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "test/pkg").unwrap();
        let aliases = versions[0].branch_aliases();
        assert!(aliases.is_empty());
    }

    #[test]
    fn test_branch_aliases_extra_without_branch_alias_key() {
        let json = r#"{
            "packages": {
                "test/pkg": [
                    {
                        "version": "dev-master",
                        "version_normalized": "dev-master",
                        "require": {},
                        "extra": {
                            "installer-name": "my-plugin"
                        }
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "test/pkg").unwrap();
        let aliases = versions[0].branch_aliases();
        assert!(aliases.is_empty());
    }
}
