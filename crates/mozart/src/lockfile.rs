use crate::package::to_json_pretty;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn default_stability() -> String {
    "stable".to_string()
}

fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Represents the content of a composer.lock file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    #[serde(rename = "_readme")]
    pub readme: Vec<String>,

    #[serde(rename = "content-hash")]
    pub content_hash: String,

    pub packages: Vec<LockedPackage>,

    #[serde(rename = "packages-dev")]
    pub packages_dev: Option<Vec<LockedPackage>>,

    #[serde(default)]
    pub aliases: Vec<LockAlias>,

    #[serde(rename = "minimum-stability", default = "default_stability")]
    pub minimum_stability: String,

    #[serde(rename = "stability-flags", default = "default_empty_object")]
    pub stability_flags: serde_json::Value,

    #[serde(rename = "prefer-stable", default)]
    pub prefer_stable: bool,

    #[serde(rename = "prefer-lowest", default)]
    pub prefer_lowest: bool,

    #[serde(default = "default_empty_object")]
    pub platform: serde_json::Value,

    #[serde(rename = "platform-dev", default = "default_empty_object")]
    pub platform_dev: serde_json::Value,

    #[serde(rename = "plugin-api-version", skip_serializing_if = "Option::is_none")]
    pub plugin_api_version: Option<String>,
}

/// A locked package entry in composer.lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,

    #[serde(rename = "version_normalized", skip_serializing_if = "Option::is_none")]
    pub version_normalized: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<LockedSource>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dist: Option<LockedDist>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub require: BTreeMap<String, String>,

    #[serde(
        rename = "require-dev",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub require_dev: BTreeMap<String, String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggest: Option<BTreeMap<String, String>>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub package_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoload: Option<serde_json::Value>,

    #[serde(rename = "autoload-dev", skip_serializing_if = "Option::is_none")]
    pub autoload_dev: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub support: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding: Option<Vec<serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,

    /// Catch-all for extra fields we don't explicitly model
    #[serde(flatten)]
    pub extra_fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: String,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedDist {
    #[serde(rename = "type")]
    pub dist_type: String,
    pub url: String,
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shasum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockAlias {
    pub package: String,
    pub version: String,
    pub alias: String,
    pub alias_normalized: String,
}

impl LockFile {
    /// Create default readme entries.
    pub fn default_readme() -> Vec<String> {
        vec![
            "This file locks the dependencies of your project to a known state".to_string(),
            "Read more about it at https://getcomposer.org/doc/01-basic-usage.md#installing-dependencies".to_string(),
            "This file is @generated automatically".to_string(),
        ]
    }

    /// Read a composer.lock file from disk.
    pub fn read_from_file(path: &Path) -> anyhow::Result<LockFile> {
        let content = fs::read_to_string(path)?;
        let lock: LockFile = serde_json::from_str(&content)?;
        Ok(lock)
    }

    /// Write a composer.lock file to disk with deterministic formatting.
    pub fn write_to_file(&self, path: &Path) -> anyhow::Result<()> {
        let json = to_json_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Check if the lock file is fresh (content-hash matches composer.json).
    pub fn is_fresh(&self, composer_json_content: &str) -> bool {
        match Self::compute_content_hash(composer_json_content) {
            Ok(hash) => hash == self.content_hash,
            Err(_) => false,
        }
    }

    /// Compute the content hash from composer.json content.
    /// Matches Composer's `Locker::getContentHash()`.
    pub fn compute_content_hash(composer_json_content: &str) -> anyhow::Result<String> {
        let value: serde_json::Value = serde_json::from_str(composer_json_content)?;
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("composer.json must be a JSON object"))?;

        // Keys that affect the content hash (Composer's relevantKeys)
        let relevant_keys = [
            "name",
            "version",
            "require",
            "require-dev",
            "conflict",
            "replace",
            "provide",
            "minimum-stability",
            "prefer-stable",
            "repositories",
            "extra",
        ];

        // Collect relevant keys into a BTreeMap (auto-sorted by key)
        let mut filtered: BTreeMap<&str, &serde_json::Value> = BTreeMap::new();
        for key in &relevant_keys {
            if let Some(v) = obj.get(*key) {
                filtered.insert(key, v);
            }
        }

        // Also include config.platform if present
        if let Some(config) = obj.get("config")
            && let Some(platform) = config.get("platform")
        {
            filtered.insert("config.platform", platform);
        }

        // Encode to compact JSON
        let compact = serde_json::to_string(&filtered)?;

        // Compute MD5
        let digest = md5::compute(compact.as_bytes());
        Ok(format!("{:x}", digest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn minimal_lock() -> LockFile {
        LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![],
            packages_dev: Some(vec![]),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        }
    }

    #[test]
    fn test_roundtrip_minimal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("composer.lock");

        let lock = minimal_lock();
        lock.write_to_file(&path).unwrap();

        let loaded = LockFile::read_from_file(&path).unwrap();
        assert_eq!(loaded.content_hash, "abc123");
        assert_eq!(loaded.minimum_stability, "stable");
        assert!(!loaded.prefer_stable);
        assert_eq!(loaded.packages.len(), 0);
    }

    #[test]
    fn test_roundtrip_with_package() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("composer.lock");

        let mut lock = minimal_lock();
        lock.packages.push(LockedPackage {
            name: "monolog/monolog".to_string(),
            version: "3.8.0".to_string(),
            version_normalized: None,
            source: None,
            dist: Some(LockedDist {
                dist_type: "zip".to_string(),
                url: "https://example.com/monolog.zip".to_string(),
                reference: Some("abc123".to_string()),
                shasum: Some("".to_string()),
            }),
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            suggest: None,
            package_type: Some("library".to_string()),
            autoload: None,
            autoload_dev: None,
            license: Some(vec!["MIT".to_string()]),
            description: Some("A logging library".to_string()),
            homepage: None,
            keywords: None,
            authors: None,
            support: None,
            funding: None,
            time: None,
            extra_fields: BTreeMap::new(),
        });

        lock.write_to_file(&path).unwrap();
        let loaded = LockFile::read_from_file(&path).unwrap();

        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0].name, "monolog/monolog");
        assert_eq!(loaded.packages[0].version, "3.8.0");
        assert_eq!(
            loaded.packages[0].description.as_deref(),
            Some("A logging library")
        );
    }

    #[test]
    fn test_content_hash_deterministic() {
        let composer_json = r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let h1 = LockFile::compute_content_hash(composer_json).unwrap();
        let h2 = LockFile::compute_content_hash(composer_json).unwrap();
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn test_content_hash_changes_on_require_change() {
        let composer1 = r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let composer2 = r#"{"name": "test/project", "require": {"monolog/monolog": "^2.0"}}"#;
        let h1 = LockFile::compute_content_hash(composer1).unwrap();
        let h2 = LockFile::compute_content_hash(composer2).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_is_fresh() {
        let composer_json = r#"{"name": "test/project", "require": {"php": ">=8.1"}}"#;
        let hash = LockFile::compute_content_hash(composer_json).unwrap();

        let mut lock = minimal_lock();
        lock.content_hash = hash;

        assert!(lock.is_fresh(composer_json));
        assert!(!lock.is_fresh(r#"{"name": "test/project", "require": {"php": ">=8.0"}}"#));
    }

    #[test]
    fn test_default_readme() {
        let readme = LockFile::default_readme();
        assert_eq!(readme.len(), 3);
        assert!(readme[0].contains("locks the dependencies"));
    }
}
