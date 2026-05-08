use mozart_core::installer::HasSuggests;
use mozart_core::package::to_json_pretty;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn default_true() -> bool {
    true
}

/// Represents `vendor/composer/installed.json`.
/// This is the Composer 2.x format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackages {
    pub packages: Vec<InstalledPackageEntry>,

    #[serde(rename = "dev-package-names", default)]
    pub dev_package_names: Vec<String>,

    #[serde(default = "default_true")]
    pub dev: bool,
}

/// An entry in installed.json's packages array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageEntry {
    pub name: String,
    pub version: String,

    #[serde(rename = "version_normalized", skip_serializing_if = "Option::is_none")]
    pub version_normalized: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dist: Option<serde_json::Value>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub package_type: Option<String>,

    #[serde(rename = "install-path", skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoload: Option<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<serde_json::Value>,

    #[serde(flatten)]
    pub extra_fields: BTreeMap<String, serde_json::Value>,
}

impl HasSuggests for InstalledPackageEntry {
    fn pretty_name(&self) -> &str {
        &self.name
    }

    fn suggests(&self) -> Vec<(String, String)> {
        let Some(val) = self.extra_fields.get("suggest") else {
            return Vec::new();
        };
        let Some(obj) = val.as_object() else {
            return Vec::new();
        };
        obj.iter()
            .filter_map(|(target, reason)| reason.as_str().map(|r| (target.clone(), r.to_string())))
            .collect()
    }
}

impl Default for InstalledPackages {
    fn default() -> Self {
        Self::new()
    }
}

impl InstalledPackages {
    /// Create an empty registry.
    pub fn new() -> InstalledPackages {
        InstalledPackages {
            packages: Vec::new(),
            dev_package_names: Vec::new(),
            dev: true,
        }
    }

    /// Read installed.json from `vendor/composer/installed.json`.
    /// If the file does not exist, returns an empty registry.
    ///
    /// Accepts both Composer formats, mirroring `FilesystemRepository::initialize`:
    /// - **v2** — object with a `packages` array, plus optional `dev-package-names`/`dev`
    ///   (the shape Composer 2.x writes).
    /// - **v1** — bare array of package entries (older shape; still legal input).
    pub fn read(vendor_dir: &Path) -> anyhow::Result<InstalledPackages> {
        let path = vendor_dir.join("composer/installed.json");
        if !path.exists() {
            return Ok(InstalledPackages::new());
        }
        let content = fs::read_to_string(&path)?;
        Self::from_json_str(&content)
    }

    /// Parse an installed.json document. See [`Self::read`] for the accepted shapes.
    pub fn from_json_str(content: &str) -> anyhow::Result<InstalledPackages> {
        use anyhow::{Context, anyhow};

        let value: serde_json::Value =
            serde_json::from_str(content).context("invalid installed.json")?;

        match value {
            serde_json::Value::Object(mut obj) => {
                let packages_value = obj.remove("packages").ok_or_else(|| {
                    anyhow!("Could not parse package list from installed.json (missing `packages`)")
                })?;
                let packages: Vec<InstalledPackageEntry> =
                    serde_json::from_value(packages_value)
                        .context("invalid `packages` array in installed.json")?;

                let dev_package_names: Vec<String> = match obj.remove("dev-package-names") {
                    Some(v) => serde_json::from_value(v)
                        .context("invalid `dev-package-names` in installed.json")?,
                    None => Vec::new(),
                };
                let dev: bool = match obj.remove("dev") {
                    Some(v) => {
                        serde_json::from_value(v).context("invalid `dev` flag in installed.json")?
                    }
                    None => true,
                };

                Ok(InstalledPackages {
                    packages,
                    dev_package_names,
                    dev,
                })
            }
            serde_json::Value::Array(_) => {
                let packages: Vec<InstalledPackageEntry> = serde_json::from_value(value)
                    .context("invalid v1 installed.json package array")?;
                Ok(InstalledPackages {
                    packages,
                    dev_package_names: Vec::new(),
                    dev: true,
                })
            }
            _ => Err(anyhow!(
                "Could not parse package list from installed.json (expected object or array)"
            )),
        }
    }

    /// Write installed.json to `vendor/composer/installed.json`.
    /// Creates the `vendor/composer/` directory if it doesn't exist.
    pub fn write(&self, vendor_dir: &Path) -> anyhow::Result<()> {
        let composer_dir = vendor_dir.join("composer");
        fs::create_dir_all(&composer_dir)?;
        let path = composer_dir.join("installed.json");
        let json = to_json_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Check if a package at a specific version is installed.
    pub fn is_installed(&self, name: &str, version: &str) -> bool {
        self.packages
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name) && p.version == version)
    }

    /// Add or update a package entry (replace if same name exists).
    pub fn upsert(&mut self, entry: InstalledPackageEntry) {
        if let Some(pos) = self
            .packages
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&entry.name))
        {
            self.packages[pos] = entry;
        } else {
            self.packages.push(entry);
        }
    }

    /// Remove a package by name.
    pub fn remove(&mut self, name: &str) {
        self.packages.retain(|p| !p.name.eq_ignore_ascii_case(name));
        self.dev_package_names
            .retain(|n| !n.eq_ignore_ascii_case(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_entry(name: &str, version: &str) -> InstalledPackageEntry {
        InstalledPackageEntry {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn test_new_is_empty() {
        let installed = InstalledPackages::new();
        assert!(installed.packages.is_empty());
        assert!(installed.dev_package_names.is_empty());
        assert!(installed.dev);
    }

    #[test]
    fn test_write_read_empty() {
        let dir = tempdir().unwrap();
        let vendor = dir.path().join("vendor");

        let installed = InstalledPackages::new();
        installed.write(&vendor).unwrap();

        let loaded = InstalledPackages::read(&vendor).unwrap();
        assert!(loaded.packages.is_empty());
        assert!(loaded.dev);
    }

    #[test]
    fn test_read_nonexistent_returns_empty() {
        let dir = tempdir().unwrap();
        let vendor = dir.path().join("vendor");
        // Don't create the directory
        let installed = InstalledPackages::read(&vendor).unwrap();
        assert!(installed.packages.is_empty());
    }

    #[test]
    fn test_upsert_and_is_installed() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_entry("monolog/monolog", "3.8.0"));

        assert!(installed.is_installed("monolog/monolog", "3.8.0"));
        assert!(!installed.is_installed("monolog/monolog", "3.7.0"));
        assert!(!installed.is_installed("other/pkg", "1.0.0"));
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_entry("monolog/monolog", "3.7.0"));
        installed.upsert(make_entry("monolog/monolog", "3.8.0"));

        assert_eq!(installed.packages.len(), 1);
        assert_eq!(installed.packages[0].version, "3.8.0");
    }

    #[test]
    fn test_remove() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_entry("monolog/monolog", "3.8.0"));
        installed.upsert(make_entry("psr/log", "3.0.0"));
        installed
            .dev_package_names
            .push("monolog/monolog".to_string());

        installed.remove("monolog/monolog");

        assert_eq!(installed.packages.len(), 1);
        assert_eq!(installed.packages[0].name, "psr/log");
        assert!(installed.dev_package_names.is_empty());
    }

    #[test]
    fn test_reads_v2_object_form() {
        let json = r#"{
            "packages": [
                {"name": "a/a", "version": "1.0.0"}
            ],
            "dev-package-names": ["a/a"],
            "dev": false
        }"#;
        let installed = InstalledPackages::from_json_str(json).unwrap();
        assert_eq!(installed.packages.len(), 1);
        assert_eq!(installed.packages[0].name, "a/a");
        assert_eq!(installed.dev_package_names, vec!["a/a".to_string()]);
        assert!(!installed.dev);
    }

    #[test]
    fn test_reads_v1_array_form() {
        // Composer 1.x / fixture-style: bare array of packages.
        // FilesystemRepository::initialize accepts this; so must Mozart.
        let json = r#"[
            {"name": "a/a", "version": "1.0.0"},
            {"name": "b/b", "version": "2.0.0"}
        ]"#;
        let installed = InstalledPackages::from_json_str(json).unwrap();
        assert_eq!(installed.packages.len(), 2);
        assert_eq!(installed.packages[0].name, "a/a");
        assert_eq!(installed.packages[1].name, "b/b");
        assert!(installed.dev_package_names.is_empty());
        assert!(installed.dev);
    }

    #[test]
    fn test_v2_defaults_when_optional_fields_missing() {
        let json = r#"{"packages": []}"#;
        let installed = InstalledPackages::from_json_str(json).unwrap();
        assert!(installed.packages.is_empty());
        assert!(installed.dev_package_names.is_empty());
        assert!(installed.dev);
    }

    #[test]
    fn test_rejects_non_object_non_array() {
        let err = InstalledPackages::from_json_str("\"oops\"").unwrap_err();
        assert!(
            err.to_string().contains("expected object or array"),
            "{err}"
        );
    }

    #[test]
    fn test_is_installed_case_insensitive() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_entry("Monolog/Monolog", "3.8.0"));
        assert!(installed.is_installed("monolog/monolog", "3.8.0"));
    }

    #[test]
    fn test_roundtrip_with_package() {
        let dir = tempdir().unwrap();
        let vendor = dir.path().join("vendor");

        let mut installed = InstalledPackages::new();
        installed.upsert(make_entry("monolog/monolog", "3.8.0"));
        installed.write(&vendor).unwrap();

        let loaded = InstalledPackages::read(&vendor).unwrap();
        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0].name, "monolog/monolog");
        assert_eq!(loaded.packages[0].version, "3.8.0");
    }

    #[test]
    fn test_homepage_and_support_roundtrip() {
        let json = r#"{
            "packages": [
                {
                    "name": "vendor/pkg",
                    "version": "1.0.0",
                    "homepage": "https://vendor.example.com",
                    "support": {"source": "https://github.com/vendor/pkg"}
                }
            ]
        }"#;
        let installed = InstalledPackages::from_json_str(json).unwrap();
        let pkg = &installed.packages[0];
        assert_eq!(pkg.homepage.as_deref(), Some("https://vendor.example.com"));
        assert_eq!(
            pkg.support
                .as_ref()
                .and_then(|s| s.get("source"))
                .and_then(|s| s.as_str()),
            Some("https://github.com/vendor/pkg")
        );
    }
}
