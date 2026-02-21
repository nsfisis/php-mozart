use serde::Deserialize;
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
    pub dist: Option<PackagistDist>,
    pub source: Option<PackagistSource>,
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
}
