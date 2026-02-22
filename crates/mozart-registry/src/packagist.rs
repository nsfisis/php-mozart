use crate::cache::Cache;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Deserialize a field that may contain the Packagist minifier sentinel `"__unset"`.
///
/// Packagist's metadata minifier (see `composer/metadata-minifier`) encodes
/// deleted fields as the literal string `"__unset"` in version diffs.  When we
/// encounter this sentinel we treat the field as absent (`None` / default).
fn deserialize_unset_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.as_str() == Some("__unset") {
        return Ok(None);
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

/// Like [`deserialize_unset_as_none`] but returns a default `T` instead of `Option`.
fn deserialize_unset_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.as_str() == Some("__unset") {
        return Ok(T::default());
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

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
    #[serde(default, deserialize_with = "deserialize_unset_as_default")]
    pub require: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_unset_as_default")]
    pub replace: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_unset_as_default")]
    pub provide: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_unset_as_default")]
    pub conflict: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub dist: Option<PackagistDist>,
    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub source: Option<PackagistSource>,

    #[serde(
        rename = "require-dev",
        default,
        deserialize_with = "deserialize_unset_as_default"
    )]
    pub require_dev: BTreeMap<String, String>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub suggest: Option<BTreeMap<String, String>>,

    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_unset_as_none"
    )]
    pub package_type: Option<String>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub autoload: Option<serde_json::Value>,

    #[serde(
        rename = "autoload-dev",
        default,
        deserialize_with = "deserialize_unset_as_none"
    )]
    pub autoload_dev: Option<serde_json::Value>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub license: Option<Vec<String>>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub description: Option<String>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub homepage: Option<String>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub keywords: Option<Vec<String>>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub authors: Option<Vec<serde_json::Value>>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub support: Option<serde_json::Value>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub funding: Option<Vec<serde_json::Value>>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub time: Option<String>,

    #[serde(default, deserialize_with = "deserialize_unset_as_none")]
    pub extra: Option<serde_json::Value>,

    #[serde(
        rename = "notification-url",
        default,
        deserialize_with = "deserialize_unset_as_none"
    )]
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
/// The response format is:
/// ```json
/// {
///   "packages": {"vendor/package": [...]},
///   "minified": "composer/2.0"   // optional
/// }
/// ```
///
/// When the `"minified"` key is present the version list is delta-encoded by
/// Composer's `MetadataMinifier`.  This function transparently expands the
/// minified data before deserializing into [`PackagistVersion`] structs.
pub fn parse_p2_response(json: &str, package_name: &str) -> anyhow::Result<Vec<PackagistVersion>> {
    let raw: serde_json::Value = serde_json::from_str(json)?;

    // Check whether the response is minified.
    let is_minified = raw
        .get("minified")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "composer/2.0");

    // Extract the version array for the requested package.
    let versions_value = raw
        .get("packages")
        .and_then(|p| p.get(package_name))
        .ok_or_else(|| anyhow::anyhow!("Package \"{package_name}\" not found in response"))?;

    let versions_array = versions_value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Expected array for package \"{package_name}\""))?;

    // Expand minified diffs into full version objects if necessary.
    let versions: Vec<serde_json::Value> = if is_minified {
        mozart_metadata_minifier::expand(versions_array)
    } else {
        versions_array.clone()
    };

    // Deserialize the (possibly expanded) version objects.
    versions
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(Into::into))
        .collect()
}

/// Fetch package version metadata from the Packagist p2 API.
///
/// If `repo_cache` is provided, the JSON response is cached on disk under the
/// key `"provider-{vendor}~{package}.json"`. Subsequent calls for the same
/// package are served from cache without a network request.
pub async fn fetch_package_versions(
    package_name: &str,
    repo_cache: Option<&Cache>,
) -> anyhow::Result<Vec<PackagistVersion>> {
    // Build cache key: replace `/` with `~` per cache key convention
    let cache_key = format!("provider-{}.json", package_name.replace('/', "~"));

    // Check cache first
    if let Some(cache) = repo_cache
        && let Some(cached) = cache.read(&cache_key)
    {
        return parse_p2_response(&cached, package_name);
    }

    // Cache miss — fetch from Packagist
    let url = format!("https://repo.packagist.org/p2/{package_name}.json");
    let client = reqwest::Client::builder()
        .user_agent(mozart_core::http::user_agent())
        .build()?;
    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Failed to fetch package \"{package_name}\" from Packagist (HTTP {})",
            response.status()
        );
    }

    let body = response.text().await?;

    // Write to cache
    if let Some(cache) = repo_cache {
        let _ = cache.write(&cache_key, &body);
    }

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
    /// Abandonment status: absent/false means active, a string indicates the
    /// replacement package name, `true` means abandoned with no replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandoned: Option<serde_json::Value>,
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
pub async fn search_packages(
    query: &str,
    package_type: Option<&str>,
) -> anyhow::Result<(Vec<SearchResult>, u64)> {
    let client = reqwest::Client::builder()
        .user_agent(mozart_core::http::user_agent())
        .build()?;

    let mut all_results: Vec<SearchResult> = Vec::new();
    let mut page = 1usize;
    let mut next_url: Option<String> = None;
    let mut total: u64 = 0;

    loop {
        let response: SearchResponse = if let Some(ref url) = next_url {
            let resp = client.get(url).send().await?;
            if !resp.status().is_success() {
                anyhow::bail!("Packagist search request failed (HTTP {})", resp.status());
            }
            resp.json().await?
        } else {
            let encoded_query = url_encode(query);
            let mut url = format!("https://packagist.org/search.json?q={encoded_query}");
            if let Some(t) = package_type {
                url.push_str("&type=");
                url.push_str(&url_encode(t));
            }

            let resp = client.get(&url).send().await?;
            if !resp.status().is_success() {
                anyhow::bail!("Packagist search request failed (HTTP {})", resp.status());
            }
            resp.json().await?
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

// ─────────────────────────────────────────────────────────────────────────────
// Security Advisories API
// ─────────────────────────────────────────────────────────────────────────────

/// A single security advisory from the Packagist API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityAdvisory {
    #[serde(rename = "advisoryId")]
    pub advisory_id: String,

    #[serde(rename = "packageName")]
    pub package_name: String,

    #[serde(rename = "remoteId")]
    pub remote_id: String,

    pub title: String,

    pub link: Option<String>,

    pub cve: Option<String>,

    /// Composer version constraint string, e.g. ">=1.0,<1.5.1|>=2.0,<2.3"
    #[serde(rename = "affectedVersions")]
    pub affected_versions: String,

    pub source: String,

    #[serde(rename = "reportedAt")]
    pub reported_at: String,

    #[serde(rename = "composerRepository")]
    pub composer_repository: Option<String>,

    pub severity: Option<String>,

    #[serde(default)]
    pub sources: Vec<AdvisorySource>,
}

/// A source entry within a security advisory.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdvisorySource {
    pub name: String,
    #[serde(rename = "remoteId")]
    pub remote_id: String,
}

/// Response from POST `https://packagist.org/api/security-advisories/`.
#[derive(Debug, Deserialize)]
pub struct SecurityAdvisoriesResponse {
    pub advisories: BTreeMap<String, Vec<SecurityAdvisory>>,
}

/// Fetch security advisories for the given package names from the Packagist API.
///
/// Sends a POST request to `https://packagist.org/api/security-advisories/`
/// with form-encoded package names. Returns advisories grouped by package name.
///
/// If the package list is very large (500+), requests are batched in chunks of
/// 500 names per request and the results are merged.
pub async fn fetch_security_advisories(
    package_names: &[&str],
) -> anyhow::Result<BTreeMap<String, Vec<SecurityAdvisory>>> {
    let client = reqwest::Client::builder()
        .user_agent(mozart_core::http::user_agent())
        .build()?;

    let mut all_advisories: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();

    for chunk in package_names.chunks(500) {
        // Build an application/x-www-form-urlencoded body manually.
        // Each package is encoded as `packages[]=<name>` and joined with `&`.
        let body: String = chunk
            .iter()
            .map(|name| format!("packages[]={}", url_encode(name)))
            .collect::<Vec<_>>()
            .join("&");

        let response = client
            .post("https://packagist.org/api/security-advisories/")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Packagist security advisories request failed (HTTP {})",
                response.status()
            );
        }

        let parsed: SecurityAdvisoriesResponse = response.json().await?;

        for (pkg_name, advisories) in parsed.advisories {
            if !advisories.is_empty() {
                all_advisories
                    .entry(pkg_name)
                    .or_default()
                    .extend(advisories);
            }
        }
    }

    Ok(all_advisories)
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

    // ──────────── __unset sentinel handling ────────────────────────────────

    #[test]
    fn parse_p2_response_unset_fields() {
        // Packagist metadata minifier uses "__unset" to mark deleted fields.
        let json = r#"{
            "packages": {
                "test/pkg": [
                    {
                        "version": "2.0.0",
                        "version_normalized": "2.0.0.0",
                        "require": {"php": ">=8.1"},
                        "license": ["MIT"],
                        "keywords": ["framework"],
                        "authors": [{"name": "Alice"}],
                        "funding": [{"type": "github", "url": "https://github.com/sponsors/alice"}]
                    },
                    {
                        "version": "1.0.0",
                        "version_normalized": "1.0.0.0",
                        "license": "__unset",
                        "keywords": "__unset",
                        "authors": "__unset",
                        "funding": "__unset",
                        "require": "__unset",
                        "homepage": "__unset",
                        "description": "__unset",
                        "extra": "__unset",
                        "suggest": "__unset"
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "test/pkg").unwrap();
        assert_eq!(versions.len(), 2);

        // First version has normal values
        assert_eq!(versions[0].license.as_ref().unwrap(), &["MIT"]);
        assert_eq!(versions[0].keywords.as_ref().unwrap(), &["framework"]);

        // Second version has __unset → treated as absent
        assert!(versions[1].license.is_none());
        assert!(versions[1].keywords.is_none());
        assert!(versions[1].authors.is_none());
        assert!(versions[1].funding.is_none());
        assert!(versions[1].require.is_empty());
        assert!(versions[1].homepage.is_none());
        assert!(versions[1].description.is_none());
        assert!(versions[1].extra.is_none());
        assert!(versions[1].suggest.is_none());
    }

    // ──────────── minified metadata expansion ──────────────────────────────

    #[test]
    fn parse_p2_response_minified_expand() {
        // Mirrors the Composer MetadataMinifierTest: 3 versions where only
        // the first carries all fields and subsequent entries are diffs.
        let json = r#"{
            "packages": {
                "foo/bar": [
                    {
                        "name": "foo/bar",
                        "version": "2.0.0",
                        "version_normalized": "2.0.0.0",
                        "type": "library",
                        "license": ["MIT"],
                        "require": {"php": ">=8.1"},
                        "description": "A great package"
                    },
                    {
                        "version": "1.2.0",
                        "version_normalized": "1.2.0.0",
                        "license": ["GPL"],
                        "homepage": "https://example.org"
                    },
                    {
                        "version": "1.0.0",
                        "version_normalized": "1.0.0.0",
                        "homepage": "__unset"
                    }
                ]
            },
            "minified": "composer/2.0"
        }"#;

        let versions = parse_p2_response(json, "foo/bar").unwrap();
        assert_eq!(versions.len(), 3);

        // Version 2.0.0 — full data (first entry).
        assert_eq!(versions[0].version, "2.0.0");
        assert_eq!(versions[0].package_type.as_deref(), Some("library"));
        assert_eq!(versions[0].license.as_ref().unwrap(), &["MIT"]);
        assert_eq!(versions[0].require.get("php").unwrap(), ">=8.1");
        assert_eq!(versions[0].description.as_deref(), Some("A great package"));
        assert!(versions[0].homepage.is_none());

        // Version 1.2.0 — inherits name, type, require, description from 2.0.0;
        // license changed to GPL; homepage added.
        assert_eq!(versions[1].version, "1.2.0");
        assert_eq!(versions[1].package_type.as_deref(), Some("library"));
        assert_eq!(versions[1].license.as_ref().unwrap(), &["GPL"]);
        assert_eq!(versions[1].require.get("php").unwrap(), ">=8.1");
        assert_eq!(versions[1].description.as_deref(), Some("A great package"));
        assert_eq!(versions[1].homepage.as_deref(), Some("https://example.org"));

        // Version 1.0.0 — inherits everything from 1.2.0 except homepage
        // which is __unset (deleted).
        assert_eq!(versions[2].version, "1.0.0");
        assert_eq!(versions[2].package_type.as_deref(), Some("library"));
        assert_eq!(versions[2].license.as_ref().unwrap(), &["GPL"]);
        assert_eq!(versions[2].require.get("php").unwrap(), ">=8.1");
        assert_eq!(versions[2].description.as_deref(), Some("A great package"));
        assert!(versions[2].homepage.is_none());
    }

    #[test]
    fn parse_p2_response_not_minified_no_inheritance() {
        // Without "minified" key, each version stands alone — no inheritance.
        let json = r#"{
            "packages": {
                "foo/bar": [
                    {
                        "version": "2.0.0",
                        "version_normalized": "2.0.0.0",
                        "license": ["MIT"],
                        "description": "A great package"
                    },
                    {
                        "version": "1.0.0",
                        "version_normalized": "1.0.0.0"
                    }
                ]
            }
        }"#;

        let versions = parse_p2_response(json, "foo/bar").unwrap();
        assert_eq!(versions.len(), 2);

        assert_eq!(versions[0].license.as_ref().unwrap(), &["MIT"]);
        assert_eq!(versions[0].description.as_deref(), Some("A great package"));

        // Without minified flag, version 1.0.0 does NOT inherit from 2.0.0.
        assert!(versions[1].license.is_none());
        assert!(versions[1].description.is_none());
    }

    #[test]
    fn parse_p2_response_minified_single_version() {
        // Edge case: minified response with only one version.
        let json = r#"{
            "packages": {
                "foo/bar": [
                    {
                        "version": "1.0.0",
                        "version_normalized": "1.0.0.0",
                        "license": ["MIT"]
                    }
                ]
            },
            "minified": "composer/2.0"
        }"#;

        let versions = parse_p2_response(json, "foo/bar").unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].license.as_ref().unwrap(), &["MIT"]);
    }

    #[test]
    fn parse_p2_response_minified_empty_versions() {
        let json = r#"{
            "packages": {
                "foo/bar": []
            },
            "minified": "composer/2.0"
        }"#;

        let versions = parse_p2_response(json, "foo/bar").unwrap();
        assert!(versions.is_empty());
    }

    #[test]
    fn parse_p2_response_minified_map_fields_inherited() {
        // Verify BTreeMap fields (require, replace, etc.) are inherited.
        let json = r#"{
            "packages": {
                "foo/bar": [
                    {
                        "version": "2.0.0",
                        "version_normalized": "2.0.0.0",
                        "require": {"php": ">=8.1", "ext-json": "*"},
                        "replace": {"foo/old": "self.version"}
                    },
                    {
                        "version": "1.0.0",
                        "version_normalized": "1.0.0.0",
                        "replace": "__unset"
                    }
                ]
            },
            "minified": "composer/2.0"
        }"#;

        let versions = parse_p2_response(json, "foo/bar").unwrap();
        assert_eq!(versions.len(), 2);

        // Version 1.0.0 inherits require from 2.0.0, replace is unset.
        assert_eq!(versions[1].require.get("php").unwrap(), ">=8.1");
        assert_eq!(versions[1].require.get("ext-json").unwrap(), "*");
        assert!(versions[1].replace.is_empty());
    }

    // ──────────── SecurityAdvisory parsing tests ─────────────────────────────

    #[test]
    fn test_parse_security_advisories_response() {
        let json = r#"{
            "advisories": {
                "monolog/monolog": [
                    {
                        "advisoryId": "PKSA-b2m0-qqf7-qck4",
                        "packageName": "monolog/monolog",
                        "remoteId": "monolog/monolog/2017-11-13-1.yaml",
                        "title": "Header injection in NativeMailerHandler",
                        "link": "https://github.com/Seldaek/monolog/pull/683",
                        "cve": null,
                        "affectedVersions": ">=1.8.0,<1.12.0",
                        "source": "FriendsOfPHP/security-advisories",
                        "reportedAt": "2017-11-13T00:00:00+00:00",
                        "composerRepository": "https://packagist.org",
                        "severity": "low",
                        "sources": [
                            {
                                "name": "FriendsOfPHP/security-advisories",
                                "remoteId": "monolog/monolog/2017-11-13-1.yaml"
                            }
                        ]
                    }
                ]
            }
        }"#;

        let response: SecurityAdvisoriesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.advisories.len(), 1);
        let advisories = response.advisories.get("monolog/monolog").unwrap();
        assert_eq!(advisories.len(), 1);
        let adv = &advisories[0];
        assert_eq!(adv.advisory_id, "PKSA-b2m0-qqf7-qck4");
        assert_eq!(adv.package_name, "monolog/monolog");
        assert_eq!(adv.title, "Header injection in NativeMailerHandler");
        assert_eq!(adv.affected_versions, ">=1.8.0,<1.12.0");
        assert_eq!(adv.severity.as_deref(), Some("low"));
        assert!(adv.cve.is_none());
        assert_eq!(adv.sources.len(), 1);
        assert_eq!(adv.sources[0].name, "FriendsOfPHP/security-advisories");
    }

    #[test]
    fn test_parse_security_advisories_empty() {
        let json = r#"{"advisories": {"other/package": []}}"#;
        let response: SecurityAdvisoriesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.advisories.len(), 1);
        let advisories = response.advisories.get("other/package").unwrap();
        assert!(advisories.is_empty());
    }

    #[test]
    fn test_parse_security_advisories_null_fields() {
        let json = r#"{
            "advisories": {
                "vendor/pkg": [
                    {
                        "advisoryId": "PKSA-0000-0000-0000",
                        "packageName": "vendor/pkg",
                        "remoteId": "vendor/pkg/2024-01-01.yaml",
                        "title": "Some vulnerability",
                        "link": null,
                        "cve": null,
                        "affectedVersions": ">=1.0,<2.0",
                        "source": "FriendsOfPHP/security-advisories",
                        "reportedAt": "2024-01-01T00:00:00+00:00",
                        "composerRepository": null,
                        "severity": null,
                        "sources": []
                    }
                ]
            }
        }"#;

        let response: SecurityAdvisoriesResponse = serde_json::from_str(json).unwrap();
        let advisories = response.advisories.get("vendor/pkg").unwrap();
        assert_eq!(advisories.len(), 1);
        let adv = &advisories[0];
        assert!(adv.link.is_none());
        assert!(adv.cve.is_none());
        assert!(adv.severity.is_none());
        assert!(adv.composer_repository.is_none());
        assert!(adv.sources.is_empty());
    }
}
