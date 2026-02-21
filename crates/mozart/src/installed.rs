use crate::package::to_json_pretty;
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

    #[serde(flatten)]
    pub extra_fields: BTreeMap<String, serde_json::Value>,
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
    pub fn read(vendor_dir: &Path) -> anyhow::Result<InstalledPackages> {
        let path = vendor_dir.join("composer/installed.json");
        if !path.exists() {
            return Ok(InstalledPackages::new());
        }
        let content = fs::read_to_string(&path)?;
        let installed: InstalledPackages = serde_json::from_str(&content)?;
        Ok(installed)
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
}
