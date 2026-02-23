//! Bridge between `mozart-vcs` and `mozart-registry`.
//!
//! Scans VCS repositories defined in composer.json and converts
//! discovered package versions into pool inputs for the SAT resolver.

use std::collections::{BTreeMap, HashMap};

use mozart_core::package::{RawRepository, Stability};
use mozart_sat_resolver::{PoolPackageInput, make_pool_links};
use mozart_vcs::driver::DriverConfig;
use mozart_vcs::repository::{VcsPackageVersion, VcsRepository};

use crate::packagist::PackagistVersion;
use crate::resolver::{parse_normalized, version_stability};

/// Scan all VCS-type repositories and collect package versions.
///
/// Non-VCS repos (e.g. "composer", "package") are silently skipped.
pub fn scan_vcs_repositories(repositories: &[RawRepository]) -> Vec<VcsPackageVersion> {
    let config = DriverConfig::default();
    let mut all_versions = Vec::new();

    for repo in repositories {
        let repo_type = repo.repo_type.as_str();
        match repo_type {
            "vcs" | "git" | "svn" | "hg" | "github" | "gitlab" | "bitbucket" | "forgejo" => {}
            _ => continue,
        }

        let forced_type = match repo_type {
            "vcs" => None,
            other => Some(other),
        };

        let vcs_repo = VcsRepository::new(repo.url.clone(), forced_type, config.clone());

        match vcs_repo.scan() {
            Ok(versions) => {
                all_versions.extend(versions);
            }
            Err(e) => {
                eprintln!("Warning: Failed to scan VCS repository {}: {}", repo.url, e,);
            }
        }
    }

    all_versions
}

/// Convert a VCS package version to SAT pool inputs.
pub fn vcs_to_pool_inputs(
    vpkg: &VcsPackageVersion,
    minimum_stability: Stability,
    stability_flags: &HashMap<String, Stability>,
) -> Vec<PoolPackageInput> {
    let mut results = Vec::new();

    // Extract dependency links from composer.json
    let require = extract_dep_map(&vpkg.composer_json, "require");
    let replace = extract_dep_map(&vpkg.composer_json, "replace");
    let provide = extract_dep_map(&vpkg.composer_json, "provide");
    let conflict = extract_dep_map(&vpkg.composer_json, "conflict");

    let input = PoolPackageInput {
        name: vpkg.name.clone(),
        version: vpkg.version_normalized.clone(),
        pretty_version: vpkg.version.clone(),
        requires: make_pool_links(
            &vpkg.name,
            &require
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
        ),
        replaces: make_pool_links(
            &vpkg.name,
            &replace
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
        ),
        provides: make_pool_links(
            &vpkg.name,
            &provide
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
        ),
        conflicts: make_pool_links(
            &vpkg.name,
            &conflict
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
        ),
        is_fixed: false,
    };

    // Apply stability filtering
    if let Some(v) = parse_normalized(&vpkg.version_normalized) {
        if passes_vcs_stability_filter(&vpkg.name, &v, minimum_stability, stability_flags) {
            results.push(input);
        }
    } else {
        // Dev version: always include (dev stability)
        let pkg_flag = stability_flags.get(&vpkg.name.to_lowercase());
        let allowed = pkg_flag.copied().unwrap_or(minimum_stability);
        if allowed >= Stability::Dev {
            results.push(input);
        }
    }

    results
}

/// Convert a `VcsPackageVersion` into a `PackagistVersion` for lockfile generation.
pub fn vcs_to_packagist_version(vpkg: &VcsPackageVersion) -> PackagistVersion {
    PackagistVersion {
        version: vpkg.version.clone(),
        version_normalized: vpkg.version_normalized.clone(),
        require: extract_dep_map(&vpkg.composer_json, "require"),
        replace: extract_dep_map(&vpkg.composer_json, "replace"),
        provide: extract_dep_map(&vpkg.composer_json, "provide"),
        conflict: extract_dep_map(&vpkg.composer_json, "conflict"),
        dist: vpkg.dist.as_ref().map(|d| crate::packagist::PackagistDist {
            dist_type: d.dist_type.clone(),
            url: d.url.clone(),
            reference: Some(d.reference.clone()),
            shasum: d.shasum.clone(),
        }),
        source: Some(crate::packagist::PackagistSource {
            source_type: vpkg.source.source_type.clone(),
            url: vpkg.source.url.clone(),
            reference: Some(vpkg.source.reference.clone()),
        }),
        require_dev: extract_dep_map(&vpkg.composer_json, "require-dev"),
        suggest: vpkg
            .composer_json
            .get("suggest")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        package_type: vpkg
            .composer_json
            .get("type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        autoload: vpkg.composer_json.get("autoload").cloned(),
        autoload_dev: vpkg.composer_json.get("autoload-dev").cloned(),
        license: vpkg
            .composer_json
            .get("license")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        description: vpkg
            .composer_json
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        homepage: vpkg
            .composer_json
            .get("homepage")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        keywords: vpkg
            .composer_json
            .get("keywords")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        authors: vpkg
            .composer_json
            .get("authors")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        support: vpkg.composer_json.get("support").cloned(),
        funding: vpkg
            .composer_json
            .get("funding")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        time: vpkg.time.clone(),
        extra: vpkg.composer_json.get("extra").cloned(),
        notification_url: None,
    }
}

/// Extract a dependency map from composer.json JSON.
fn extract_dep_map(json: &serde_json::Value, key: &str) -> BTreeMap<String, String> {
    json.get(key)
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Stability filter for VCS packages (mirrors resolver logic).
fn passes_vcs_stability_filter(
    package_name: &str,
    version: &mozart_semver::Version,
    minimum_stability: Stability,
    stability_flags: &HashMap<String, Stability>,
) -> bool {
    let stability = version_stability(version);
    let pkg_flag = stability_flags.get(&package_name.to_lowercase());
    let allowed = pkg_flag.copied().unwrap_or(minimum_stability);
    stability <= allowed
}
