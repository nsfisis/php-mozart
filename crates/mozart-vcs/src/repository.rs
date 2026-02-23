use anyhow::{Result, bail};

use crate::driver::{
    DistReference, DriverConfig, DriverType, SourceReference, create_driver, detect_driver,
};

/// A single package version discovered from a VCS repository.
#[derive(Debug, Clone)]
pub struct VcsPackageVersion {
    /// Package name (from composer.json).
    pub name: String,
    /// Version string (e.g., "1.2.3" for tags, "dev-main" for branches).
    pub version: String,
    /// Normalized version for comparison.
    pub version_normalized: String,
    /// Full composer.json data as JSON.
    pub composer_json: serde_json::Value,
    /// Source reference (VCS checkout info).
    pub source: SourceReference,
    /// Dist reference (archive download, if available).
    pub dist: Option<DistReference>,
    /// Whether this is the default branch version.
    pub is_default_branch: bool,
    /// Release date (ISO 8601).
    pub time: Option<String>,
}

/// Repository that scans a VCS URL for package versions.
///
/// Corresponds to Composer's `Repository\VcsRepository`.
pub struct VcsRepository {
    url: String,
    driver_type: Option<DriverType>,
    config: DriverConfig,
}

impl VcsRepository {
    pub fn new(url: String, repo_type: Option<&str>, config: DriverConfig) -> Self {
        let driver_type = detect_driver(&url, repo_type, &config);
        Self {
            url,
            driver_type,
            config,
        }
    }

    /// Scan the VCS repository for all package versions.
    ///
    /// 1. Detects the driver type and initializes it
    /// 2. Reads composer.json from the root to get the package name
    /// 3. Scans tags → version releases
    /// 4. Scans branches → dev versions
    pub async fn scan(&self) -> Result<Vec<VcsPackageVersion>> {
        let driver_type = self
            .driver_type
            .ok_or_else(|| anyhow::anyhow!("No suitable VCS driver found for URL: {}", self.url))?;

        let mut driver = create_driver(&self.url, driver_type, self.config.clone());
        driver.initialize().await?;

        // Get package name from root composer.json
        let root_id = driver.root_identifier().to_string();
        let root_info = driver.composer_information(&root_id).await?;
        let package_name = match &root_info {
            Some(info) => info["name"]
                .as_str()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "composer.json at root of {} does not contain a 'name' field",
                        self.url,
                    )
                })?
                .to_string(),
            None => bail!(
                "No composer.json found at root of {} (ref: {})",
                self.url,
                root_id,
            ),
        };

        let mut versions = Vec::new();

        // Scan tags
        let tags = driver.tags().await?.clone();
        for (tag_name, tag_hash) in &tags {
            if let Some(version) = self.tag_to_version(tag_name) {
                match driver.composer_information(tag_hash).await {
                    Ok(Some(info)) => {
                        let time = driver.change_date(tag_hash).await.unwrap_or(None);
                        let source = driver.source(tag_hash);
                        let dist = driver.dist(tag_hash).await.unwrap_or(None);

                        // Ensure name matches root package
                        if info["name"].as_str() != Some(&package_name) {
                            continue;
                        }

                        let normalized = self.normalize_version(&version);

                        versions.push(VcsPackageVersion {
                            name: package_name.clone(),
                            version: version.clone(),
                            version_normalized: normalized,
                            composer_json: info,
                            source,
                            dist,
                            is_default_branch: false,
                            time,
                        });
                    }
                    Ok(None) | Err(_) => continue,
                }
            }
        }

        // Scan branches
        let branches = driver.branches().await?.clone();
        let default_branch = driver.root_identifier().to_string();
        for (branch_name, branch_hash) in &branches {
            match driver.composer_information(branch_hash).await {
                Ok(Some(info)) => {
                    if info["name"].as_str() != Some(&package_name) {
                        continue;
                    }

                    let time = driver.change_date(branch_hash).await.unwrap_or(None);
                    let source = driver.source(branch_hash);
                    let dist = driver.dist(branch_hash).await.unwrap_or(None);
                    let is_default = branch_name == &default_branch;

                    let version = self.branch_to_version(branch_name);
                    let normalized = self.normalize_version(&version);

                    // Check for branch-alias
                    let aliased_version = info
                        .get("extra")
                        .and_then(|e| e.get("branch-alias"))
                        .and_then(|ba| ba.get(format!("dev-{branch_name}")))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    versions.push(VcsPackageVersion {
                        name: package_name.clone(),
                        version: aliased_version.unwrap_or(version),
                        version_normalized: normalized,
                        composer_json: info,
                        source,
                        dist,
                        is_default_branch: is_default,
                        time,
                    });
                }
                Ok(None) | Err(_) => continue,
            }
        }

        driver.cleanup().await?;
        Ok(versions)
    }

    /// Convert a tag name to a version string.
    /// Returns `None` if the tag doesn't look like a version.
    fn tag_to_version(&self, tag: &str) -> Option<String> {
        // Strip common prefixes
        let version = tag
            .strip_prefix('v')
            .or_else(|| tag.strip_prefix("V"))
            .or_else(|| tag.strip_prefix("release-"))
            .or_else(|| tag.strip_prefix("release/"))
            .unwrap_or(tag);

        // Basic semver-ish check
        if version.is_empty() {
            return None;
        }
        if version.chars().next()?.is_ascii_digit() {
            Some(version.to_string())
        } else {
            None
        }
    }

    /// Convert a branch name to a dev version string.
    fn branch_to_version(&self, branch: &str) -> String {
        // Numeric branches like "1.x", "2.0" become "1.x-dev", "2.0.x-dev"
        if branch.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            let version = if branch.ends_with(".x") || branch.ends_with(".*") {
                branch.to_string()
            } else {
                format!("{branch}.x")
            };
            format!("{version}-dev")
        } else {
            format!("dev-{branch}")
        }
    }

    /// Normalize a version string.
    fn normalize_version(&self, version: &str) -> String {
        // Use mozart-semver for proper normalization if available,
        // otherwise do a simple normalization
        mozart_semver::Version::parse(version)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| version.to_string())
    }
}
