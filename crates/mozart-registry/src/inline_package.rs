//! Support for inline `type: package` repositories.
//!
//! `composer.json` may embed full package metadata under
//! `repositories[].package`, mirroring `Composer\Repository\PackageRepository`.
//! These packages need no network fetch — they go straight into the resolver
//! pool and into the generated lockfile entry verbatim.

use crate::packagist::PackagistVersion;
use indexmap::IndexSet;
use mozart_core::package::RawRepository;

/// One package extracted from a `type: package` repository.
pub struct InlinePackage {
    pub name: String,
    pub version: PackagistVersion,
}

/// Collect every package definition from `type: package` repositories.
///
/// Each repository's `package` field may be a single object or an array of
/// objects. Entries that fail to parse (missing `name`/`version`, etc.) are
/// silently skipped so the rest of the repositories list still applies —
/// matching Composer's lenient PackageRepository constructor.
///
/// Repositories are processed in declaration order. Once any repository
/// authoritatively answers for a package name, lower-priority `type: package`
/// repositories that list the same name are skipped — mirroring Composer's
/// first-repo-wins priority via `RepositorySet::findPackages`.
pub fn collect_inline_packages(repositories: &[RawRepository]) -> Vec<InlinePackage> {
    let mut packages = Vec::new();
    let mut claimed: IndexSet<String> = IndexSet::new();
    for repo in repositories {
        if repo.repo_type != "package" {
            continue;
        }
        let Some(value) = &repo.package else {
            continue;
        };

        let mut from_this_repo: Vec<InlinePackage> = Vec::new();
        match value {
            serde_json::Value::Array(arr) => {
                for entry in arr {
                    if let Some(pkg) = parse_inline_package(entry) {
                        from_this_repo.push(pkg);
                    }
                }
            }
            serde_json::Value::Object(_) => {
                if let Some(pkg) = parse_inline_package(value) {
                    from_this_repo.push(pkg);
                }
            }
            _ => {}
        }

        let mut names_this_repo: IndexSet<String> = IndexSet::new();
        for pkg in from_this_repo {
            if claimed.contains(&pkg.name) {
                continue;
            }
            names_this_repo.insert(pkg.name.clone());
            packages.push(pkg);
        }
        claimed.extend(names_this_repo);
    }
    packages
}

fn parse_inline_package(value: &serde_json::Value) -> Option<InlinePackage> {
    let obj = value.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    let version_str = obj.get("version")?.as_str()?.to_string();

    // PackagistVersion requires `version_normalized`. If the inline definition
    // omits it (the common case), compute it the same way Packagist does:
    // run the version through Mozart's normalizer.
    let mut value_for_parse = value.clone();
    if let serde_json::Value::Object(ref mut map) = value_for_parse
        && !map.contains_key("version_normalized")
    {
        let normalized = mozart_semver::Version::parse(&version_str)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| version_str.clone());
        map.insert(
            "version_normalized".to_string(),
            serde_json::Value::String(normalized),
        );
    }

    let version: PackagistVersion = serde_json::from_value(value_for_parse).ok()?;
    Some(InlinePackage { name, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_repo(value: serde_json::Value) -> RawRepository {
        RawRepository {
            repo_type: "package".to_string(),
            url: None,
            package: Some(value),
        }
    }

    #[test]
    fn collects_single_inline_package_object() {
        let repos = vec![pkg_repo(serde_json::json!({
            "name": "a/a",
            "version": "1.0.0"
        }))];
        let pkgs = collect_inline_packages(&repos);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "a/a");
        assert_eq!(pkgs[0].version.version, "1.0.0");
        assert_eq!(pkgs[0].version.version_normalized, "1.0.0.0");
    }

    #[test]
    fn collects_inline_package_array() {
        let repos = vec![pkg_repo(serde_json::json!([
            {"name": "a/a", "version": "1.0.0"},
            {"name": "b/b", "version": "2.0.0"}
        ]))];
        let pkgs = collect_inline_packages(&repos);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "a/a");
        assert_eq!(pkgs[1].name, "b/b");
    }

    #[test]
    fn ignores_non_package_repos() {
        let repos = vec![RawRepository {
            repo_type: "vcs".to_string(),
            url: Some("https://example.com/foo.git".to_string()),
            package: None,
        }];
        assert!(collect_inline_packages(&repos).is_empty());
    }

    #[test]
    fn skips_entries_missing_name_or_version() {
        let repos = vec![pkg_repo(serde_json::json!([
            {"name": "a/a", "version": "1.0.0"},
            {"name": "missing/version"},
            {"version": "2.0.0"},
            {"name": "b/b", "version": "2.0.0"}
        ]))];
        let pkgs = collect_inline_packages(&repos);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "a/a");
        assert_eq!(pkgs[1].name, "b/b");
    }

    #[test]
    fn preserves_explicit_version_normalized() {
        let repos = vec![pkg_repo(serde_json::json!({
            "name": "a/a",
            "version": "1.0",
            "version_normalized": "1.0.0.0-explicit"
        }))];
        let pkgs = collect_inline_packages(&repos);
        assert_eq!(pkgs[0].version.version_normalized, "1.0.0.0-explicit");
    }

    #[test]
    fn parses_full_metadata_fields() {
        let repos = vec![pkg_repo(serde_json::json!({
            "name": "a/a",
            "version": "1.0.0",
            "type": "library",
            "require": {"b/b": "^2.0"},
            "replace": {"old/x": "1.0"},
            "provide": {"some/iface": "1.0"},
            "conflict": {"bad/pkg": "*"},
            "dist": {"type": "zip", "url": "https://e.com/a.zip"}
        }))];
        let pkgs = collect_inline_packages(&repos);
        assert_eq!(pkgs.len(), 1);
        let v = &pkgs[0].version;
        assert_eq!(v.package_type.as_deref(), Some("library"));
        assert_eq!(v.require.get("b/b").map(String::as_str), Some("^2.0"));
        assert_eq!(v.replace.get("old/x").map(String::as_str), Some("1.0"));
        assert_eq!(v.provide.get("some/iface").map(String::as_str), Some("1.0"));
        assert_eq!(v.conflict.get("bad/pkg").map(String::as_str), Some("*"));
        assert!(v.dist.is_some());
    }
}
