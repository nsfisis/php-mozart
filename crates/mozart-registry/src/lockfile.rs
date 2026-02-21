use crate::cache::Cache;
use crate::packagist::{self, PackagistDist, PackagistSource, PackagistVersion};
use crate::resolver::ResolvedPackage;
use mozart_core::package::{RawPackageData, to_json_pretty};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub conflict: BTreeMap<String, String>,

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

// ─────────────────────────────────────────────────────────────────────────────
// Lock file generation
// ─────────────────────────────────────────────────────────────────────────────

/// Input for lock file generation.
pub struct LockFileGenerationRequest {
    /// Resolved packages from the dependency resolver.
    pub resolved_packages: Vec<ResolvedPackage>,
    /// Raw composer.json content string (for content-hash computation).
    pub composer_json_content: String,
    /// Parsed composer.json data (for platform, minimum-stability, etc.).
    pub composer_json: RawPackageData,
    /// Whether require-dev was included in resolution.
    pub include_dev: bool,
    /// Optional repo cache for Packagist API calls made during generation.
    pub repo_cache: Option<Cache>,
}

/// Convert a `PackagistSource` to a `LockedSource`.
fn packagist_source_to_locked(ps: &PackagistSource) -> LockedSource {
    LockedSource {
        source_type: ps.source_type.clone(),
        url: ps.url.clone(),
        reference: ps.reference.clone(),
    }
}

/// Convert a `PackagistDist` to a `LockedDist`.
fn packagist_dist_to_locked(pd: &PackagistDist) -> LockedDist {
    LockedDist {
        dist_type: pd.dist_type.clone(),
        url: pd.url.clone(),
        reference: pd.reference.clone(),
        shasum: pd.shasum.clone(),
    }
}

/// Convert a `PackagistVersion` to a `LockedPackage`.
fn packagist_version_to_locked_package(name: &str, pv: &PackagistVersion) -> LockedPackage {
    let mut extra_fields: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    if let Some(extra) = &pv.extra {
        extra_fields.insert("extra".to_string(), extra.clone());
    }
    if let Some(notification_url) = &pv.notification_url {
        extra_fields.insert(
            "notification-url".to_string(),
            serde_json::Value::String(notification_url.clone()),
        );
    }

    LockedPackage {
        name: name.to_string(),
        version: pv.version.clone(),
        version_normalized: Some(pv.version_normalized.clone()),
        source: pv.source.as_ref().map(packagist_source_to_locked),
        dist: pv.dist.as_ref().map(packagist_dist_to_locked),
        require: pv.require.clone(),
        require_dev: pv.require_dev.clone(),
        conflict: pv.conflict.clone(),
        suggest: pv.suggest.clone(),
        package_type: pv.package_type.clone(),
        autoload: pv.autoload.clone(),
        autoload_dev: pv.autoload_dev.clone(),
        license: pv.license.clone(),
        description: pv.description.clone(),
        homepage: pv.homepage.clone(),
        keywords: pv.keywords.clone(),
        authors: pv.authors.clone(),
        support: pv.support.clone(),
        funding: pv.funding.clone(),
        time: pv.time.clone(),
        extra_fields,
    }
}

/// Determine which resolved packages are dev-only.
///
/// A package is dev-only if it is NOT reachable from the non-dev dependency tree
/// (i.e., only reachable through require-dev paths).
///
/// `package_metadata` must be pre-fetched full `PackagistVersion` data for each resolved package.
fn classify_dev_packages(
    resolved: &[ResolvedPackage],
    require: &BTreeMap<String, String>,
    _require_dev: &BTreeMap<String, String>,
    package_metadata: &HashMap<String, PackagistVersion>,
) -> HashSet<String> {
    // Build set of all resolved package names for quick lookup
    let resolved_names: HashSet<&str> = resolved.iter().map(|p| p.name.as_str()).collect();

    // BFS from non-dev root dependencies through each package's `require` map.
    // All reachable packages are production packages.
    let mut production: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    // Seed queue with non-dev root dependencies that are actual packages (not platform)
    for name in require.keys() {
        let name_lower = name.to_lowercase();
        // Skip platform packages (php, ext-*, lib-*, etc.)
        if is_platform_name(&name_lower) {
            continue;
        }
        if resolved_names.contains(name_lower.as_str()) && production.insert(name_lower.clone()) {
            queue.push_back(name_lower);
        }
    }

    // BFS: walk transitive `require` deps of each production package
    while let Some(pkg_name) = queue.pop_front() {
        if let Some(pv) = package_metadata.get(&pkg_name) {
            for dep_name in pv.require.keys() {
                let dep_lower = dep_name.to_lowercase();
                if is_platform_name(&dep_lower) {
                    continue;
                }
                if resolved_names.contains(dep_lower.as_str())
                    && production.insert(dep_lower.clone())
                {
                    queue.push_back(dep_lower);
                }
            }
        }
    }

    // Any resolved package not in `production` is dev-only
    resolved
        .iter()
        .filter(|p| !production.contains(&p.name))
        .map(|p| p.name.clone())
        .collect()
}

/// Returns true if the package name is a platform package (php, ext-*, lib-*, etc.).
fn is_platform_name(name: &str) -> bool {
    name == "php"
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || name == "php-64bit"
        || name == "php-ipv6"
        || name == "php-zts"
        || name == "php-debug"
}

/// Extract platform requirements from a requirements map.
///
/// Filters the map to include only platform package keys (`php`, `ext-*`, `lib-*`, etc.)
/// and returns them as a JSON object.
fn extract_platform_requirements(requirements: &BTreeMap<String, String>) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = requirements
        .iter()
        .filter(|(k, _)| is_platform_name(k))
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::Value::Object(map)
}

/// Generate a complete `LockFile` from resolution results.
///
/// This function:
/// 1. Fetches full metadata from Packagist for each resolved package
/// 2. Separates packages into production vs dev-only
/// 3. Computes the content-hash
/// 4. Assembles the complete `LockFile` struct
pub fn generate_lock_file(request: &LockFileGenerationRequest) -> anyhow::Result<LockFile> {
    // 1. Fetch full metadata for all resolved packages
    let mut package_metadata: HashMap<String, PackagistVersion> = HashMap::new();
    for pkg in &request.resolved_packages {
        let versions = packagist::fetch_package_versions(&pkg.name, request.repo_cache.as_ref())?;
        // Find the exact version matching pkg.version_normalized
        let matching = versions
            .into_iter()
            .find(|v| v.version_normalized == pkg.version_normalized)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find version {} for package {} in Packagist response",
                    pkg.version_normalized,
                    pkg.name
                )
            })?;
        package_metadata.insert(pkg.name.clone(), matching);
    }

    // 2. Classify dev vs non-dev packages
    let dev_only = classify_dev_packages(
        &request.resolved_packages,
        &request.composer_json.require,
        &request.composer_json.require_dev,
        &package_metadata,
    );

    // 3. Build LockedPackage lists
    let mut packages: Vec<LockedPackage> = Vec::new();
    let mut packages_dev: Vec<LockedPackage> = Vec::new();
    for pkg in &request.resolved_packages {
        let pv = &package_metadata[&pkg.name];
        let locked = packagist_version_to_locked_package(&pkg.name, pv);
        if dev_only.contains(&pkg.name) {
            packages_dev.push(locked);
        } else {
            packages.push(locked);
        }
    }

    // 4. Sort each list alphabetically by name (Composer does this)
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    packages_dev.sort_by(|a, b| a.name.cmp(&b.name));

    // 5. Compute content-hash
    let content_hash = LockFile::compute_content_hash(&request.composer_json_content)?;

    // 6. Extract platform requirements
    let platform = extract_platform_requirements(&request.composer_json.require);
    let platform_dev = extract_platform_requirements(&request.composer_json.require_dev);

    // 7. Determine minimum-stability and prefer-stable
    let minimum_stability = request
        .composer_json
        .minimum_stability
        .clone()
        .unwrap_or_else(|| "stable".to_string());

    let prefer_stable = request
        .composer_json
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 8. Assemble LockFile
    Ok(LockFile {
        readme: LockFile::default_readme(),
        content_hash,
        packages,
        packages_dev: if request.include_dev {
            Some(packages_dev)
        } else {
            Some(vec![])
        },
        aliases: vec![],
        minimum_stability,
        stability_flags: serde_json::json!({}),
        prefer_stable,
        prefer_lowest: false,
        platform,
        platform_dev,
        plugin_api_version: Some("2.6.0".to_string()),
    })
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
            conflict: BTreeMap::new(),
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

    // ──────────── Lock file generation tests ────────────

    fn make_packagist_version(
        version: &str,
        version_normalized: &str,
        require: BTreeMap<String, String>,
    ) -> PackagistVersion {
        PackagistVersion {
            version: version.to_string(),
            version_normalized: version_normalized.to_string(),
            require,
            replace: BTreeMap::new(),
            provide: BTreeMap::new(),
            conflict: BTreeMap::new(),
            dist: Some(crate::packagist::PackagistDist {
                dist_type: "zip".to_string(),
                url: format!("https://example.com/{version}.zip"),
                reference: Some("deadbeef".to_string()),
                shasum: Some("abc123".to_string()),
            }),
            source: Some(crate::packagist::PackagistSource {
                source_type: "git".to_string(),
                url: "https://github.com/example/pkg.git".to_string(),
                reference: Some("deadbeef".to_string()),
            }),
            require_dev: BTreeMap::new(),
            suggest: None,
            package_type: Some("library".to_string()),
            autoload: Some(serde_json::json!({"psr-4": {"Example\\": "src/"}})),
            autoload_dev: None,
            license: Some(vec!["MIT".to_string()]),
            description: Some("An example package".to_string()),
            homepage: Some("https://example.com".to_string()),
            keywords: Some(vec!["example".to_string(), "test".to_string()]),
            authors: Some(vec![
                serde_json::json!({"name": "Alice", "email": "alice@example.com"}),
            ]),
            support: Some(serde_json::json!({"issues": "https://github.com/example/pkg/issues"})),
            funding: Some(vec![
                serde_json::json!({"type": "github", "url": "https://github.com/sponsors/alice"}),
            ]),
            time: Some("2024-01-15T10:00:00+00:00".to_string()),
            extra: Some(serde_json::json!({"branch-alias": {"dev-main": "1.0.x-dev"}})),
            notification_url: Some("https://packagist.org/downloads/".to_string()),
        }
    }

    #[test]
    fn test_packagist_version_to_locked_package() {
        let pv = make_packagist_version("1.2.3", "1.2.3.0", BTreeMap::new());
        let locked = packagist_version_to_locked_package("example/pkg", &pv);

        assert_eq!(locked.name, "example/pkg");
        assert_eq!(locked.version, "1.2.3");
        assert_eq!(locked.version_normalized.as_deref(), Some("1.2.3.0"));
        assert_eq!(locked.description.as_deref(), Some("An example package"));
        assert_eq!(locked.homepage.as_deref(), Some("https://example.com"));
        assert_eq!(
            locked.license.as_deref(),
            Some(vec!["MIT".to_string()].as_slice())
        );
        assert_eq!(
            locked.keywords.as_ref().map(|k| k.as_slice()),
            Some(["example".to_string(), "test".to_string()].as_slice())
        );
        assert_eq!(locked.package_type.as_deref(), Some("library"));
        assert!(locked.autoload.is_some());
        assert!(locked.authors.is_some());
        assert!(locked.support.is_some());
        assert!(locked.funding.is_some());
        assert_eq!(locked.time.as_deref(), Some("2024-01-15T10:00:00+00:00"));

        // Check dist
        let dist = locked.dist.as_ref().unwrap();
        assert_eq!(dist.dist_type, "zip");
        assert_eq!(dist.reference.as_deref(), Some("deadbeef"));
        assert_eq!(dist.shasum.as_deref(), Some("abc123"));

        // Check source
        let source = locked.source.as_ref().unwrap();
        assert_eq!(source.source_type, "git");
        assert_eq!(source.reference.as_deref(), Some("deadbeef"));

        // Check extra_fields (extra and notification-url)
        assert!(locked.extra_fields.contains_key("extra"));
        assert!(locked.extra_fields.contains_key("notification-url"));
        assert_eq!(
            locked.extra_fields["notification-url"],
            serde_json::Value::String("https://packagist.org/downloads/".to_string())
        );
    }

    #[test]
    fn test_packagist_version_to_locked_package_no_optional_fields() {
        let pv = PackagistVersion {
            version: "1.0.0".to_string(),
            version_normalized: "1.0.0.0".to_string(),
            require: BTreeMap::new(),
            replace: BTreeMap::new(),
            provide: BTreeMap::new(),
            conflict: BTreeMap::new(),
            dist: None,
            source: None,
            require_dev: BTreeMap::new(),
            suggest: None,
            package_type: None,
            autoload: None,
            autoload_dev: None,
            license: None,
            description: None,
            homepage: None,
            keywords: None,
            authors: None,
            support: None,
            funding: None,
            time: None,
            extra: None,
            notification_url: None,
        };

        let locked = packagist_version_to_locked_package("vendor/pkg", &pv);
        assert_eq!(locked.name, "vendor/pkg");
        assert!(locked.dist.is_none());
        assert!(locked.source.is_none());
        assert!(locked.description.is_none());
        assert!(locked.license.is_none());
        assert!(locked.extra_fields.is_empty());
    }

    #[test]
    fn test_classify_dev_packages_simple() {
        // Root: require={A}, require-dev={B}
        // A depends on C; B depends on D
        // Expected dev-only: {B, D}
        let resolved = vec![
            ResolvedPackage {
                name: "vendor/a".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
            ResolvedPackage {
                name: "vendor/b".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
            ResolvedPackage {
                name: "vendor/c".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
            ResolvedPackage {
                name: "vendor/d".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
        ];

        let mut require = BTreeMap::new();
        require.insert("vendor/a".to_string(), "^1.0".to_string());

        let mut require_dev = BTreeMap::new();
        require_dev.insert("vendor/b".to_string(), "^1.0".to_string());

        let mut metadata: HashMap<String, PackagistVersion> = HashMap::new();

        // A requires C
        let mut a_require = BTreeMap::new();
        a_require.insert("vendor/c".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/a".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", a_require),
        );

        // B requires D
        let mut b_require = BTreeMap::new();
        b_require.insert("vendor/d".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/b".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", b_require),
        );

        // C and D have no deps
        metadata.insert(
            "vendor/c".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", BTreeMap::new()),
        );
        metadata.insert(
            "vendor/d".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", BTreeMap::new()),
        );

        let dev_only = classify_dev_packages(&resolved, &require, &require_dev, &metadata);

        assert!(!dev_only.contains("vendor/a"), "A is a production package");
        assert!(dev_only.contains("vendor/b"), "B is dev-only");
        assert!(
            !dev_only.contains("vendor/c"),
            "C is reachable from A (production)"
        );
        assert!(
            dev_only.contains("vendor/d"),
            "D is only reachable from B (dev)"
        );
    }

    #[test]
    fn test_classify_dev_packages_shared() {
        // Root: require={A}, require-dev={B}
        // Both A and B depend on C — C is NOT dev-only (reachable from production)
        let resolved = vec![
            ResolvedPackage {
                name: "vendor/a".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
            ResolvedPackage {
                name: "vendor/b".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
            ResolvedPackage {
                name: "vendor/c".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
            },
        ];

        let mut require = BTreeMap::new();
        require.insert("vendor/a".to_string(), "^1.0".to_string());

        let mut require_dev = BTreeMap::new();
        require_dev.insert("vendor/b".to_string(), "^1.0".to_string());

        let mut metadata: HashMap<String, PackagistVersion> = HashMap::new();

        // A requires C
        let mut a_require = BTreeMap::new();
        a_require.insert("vendor/c".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/a".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", a_require),
        );

        // B also requires C
        let mut b_require = BTreeMap::new();
        b_require.insert("vendor/c".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/b".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", b_require),
        );

        // C has no deps
        metadata.insert(
            "vendor/c".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", BTreeMap::new()),
        );

        let dev_only = classify_dev_packages(&resolved, &require, &require_dev, &metadata);

        assert!(!dev_only.contains("vendor/a"), "A is a production package");
        assert!(dev_only.contains("vendor/b"), "B is dev-only");
        assert!(
            !dev_only.contains("vendor/c"),
            "C is shared but reachable from production (A), so it's not dev-only"
        );
    }

    #[test]
    fn test_extract_platform_requirements() {
        let mut requirements = BTreeMap::new();
        requirements.insert("php".to_string(), ">=8.1".to_string());
        requirements.insert("ext-json".to_string(), "*".to_string());
        requirements.insert("ext-mbstring".to_string(), "*".to_string());
        requirements.insert("monolog/monolog".to_string(), "^3.0".to_string());
        requirements.insert("lib-pcre".to_string(), "*".to_string());

        let platform = extract_platform_requirements(&requirements);
        let obj = platform.as_object().unwrap();

        assert!(obj.contains_key("php"), "php should be in platform");
        assert!(
            obj.contains_key("ext-json"),
            "ext-json should be in platform"
        );
        assert!(
            obj.contains_key("ext-mbstring"),
            "ext-mbstring should be in platform"
        );
        assert!(
            obj.contains_key("lib-pcre"),
            "lib-pcre should be in platform"
        );
        assert!(
            !obj.contains_key("monolog/monolog"),
            "monolog/monolog should NOT be in platform"
        );
        assert_eq!(obj["php"], serde_json::Value::String(">=8.1".to_string()));
        assert_eq!(obj["ext-json"], serde_json::Value::String("*".to_string()));
    }

    #[test]
    fn test_extract_platform_requirements_empty() {
        let requirements = BTreeMap::new();
        let platform = extract_platform_requirements(&requirements);
        assert_eq!(platform, serde_json::json!({}));
    }

    #[test]
    fn test_generate_lock_file_minimal() {
        let composer_json_content =
            r#"{"name": "test/project", "require": {"php": ">=8.1"}}"#.to_string();
        let composer_json: RawPackageData = serde_json::from_str(&composer_json_content).unwrap();

        let request = LockFileGenerationRequest {
            resolved_packages: vec![],
            composer_json_content: composer_json_content.clone(),
            composer_json,
            include_dev: true,
            repo_cache: None,
        };

        let lock = generate_lock_file(&request).unwrap();

        assert_eq!(lock.packages.len(), 0);
        assert_eq!(lock.packages_dev.as_ref().unwrap().len(), 0);
        assert_eq!(lock.minimum_stability, "stable");
        assert!(!lock.prefer_stable);
        assert!(!lock.prefer_lowest);
        assert_eq!(lock.plugin_api_version.as_deref(), Some("2.6.0"));

        // Verify content-hash matches
        let expected_hash = LockFile::compute_content_hash(&composer_json_content).unwrap();
        assert_eq!(lock.content_hash, expected_hash);

        // Verify platform requirements extracted
        let platform_obj = lock.platform.as_object().unwrap();
        assert!(platform_obj.contains_key("php"));
        assert_eq!(
            platform_obj["php"],
            serde_json::Value::String(">=8.1".to_string())
        );
    }

    #[test]
    fn test_lock_file_packages_sorted() {
        // Verify that packages are sorted alphabetically when assembled in generate_lock_file
        // We test this by constructing two LockedPackages and sorting them the same way

        let mut packages = vec![
            LockedPackage {
                name: "vendor/zebra".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            },
            LockedPackage {
                name: "vendor/alpha".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            },
        ];

        packages.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(packages[0].name, "vendor/alpha");
        assert_eq!(packages[1].name, "vendor/zebra");
    }

    #[test]
    #[ignore]
    fn test_generate_lock_file_monolog() {
        use crate::resolver::PlatformConfig;
        use crate::resolver::{ResolveRequest, resolve};
        use mozart_core::package::Stability;

        // Resolve monolog/monolog ^3.0
        let resolve_request = ResolveRequest {
            require: vec![("monolog/monolog".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let resolved = resolve(&resolve_request).expect("Resolution should succeed");
        assert!(!resolved.is_empty());

        let composer_json_content =
            r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#.to_string();
        let composer_json: RawPackageData = serde_json::from_str(&composer_json_content).unwrap();

        let gen_request = LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.clone(),
            composer_json,
            include_dev: false,
            repo_cache: None,
        };

        let lock = generate_lock_file(&gen_request).expect("Lock file generation should succeed");

        // Verify monolog is in packages
        assert!(
            lock.packages.iter().any(|p| p.name == "monolog/monolog"),
            "monolog/monolog should be in packages"
        );

        // Verify packages are sorted alphabetically
        let names: Vec<&str> = lock.packages.iter().map(|p| p.name.as_str()).collect();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(
            names, sorted_names,
            "Packages should be sorted alphabetically"
        );

        // Verify content-hash matches
        let expected_hash = LockFile::compute_content_hash(&composer_json_content).unwrap();
        assert_eq!(lock.content_hash, expected_hash);

        // Verify monolog has full metadata
        let monolog = lock
            .packages
            .iter()
            .find(|p| p.name == "monolog/monolog")
            .unwrap();
        assert!(monolog.dist.is_some(), "monolog should have dist info");
        assert!(
            monolog.description.is_some(),
            "monolog should have description"
        );
        assert!(monolog.autoload.is_some(), "monolog should have autoload");

        println!("Generated lock file with {} packages:", lock.packages.len());
        for pkg in &lock.packages {
            println!("  {} {}", pkg.name, pkg.version);
        }
    }
}
