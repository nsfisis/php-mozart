//! Support for `type: composer` repositories.
//!
//! A Composer repository is a directory (or HTTP endpoint) hosting a
//! `packages.json` file. The legacy format embeds full package metadata
//! directly:
//!
//! ```json
//! {
//!     "packages": {
//!         "a/a": {
//!             "dev-foobar": { "name": "a/a", "version": "dev-foobar", ... }
//!         }
//!     }
//! }
//! ```
//!
//! Mirrors `Composer\Repository\ComposerRepository` for the file:// case
//! used by the test fixtures. Lazy / v2 / provider-includes / metadata-url
//! variants are out of scope here — the in-process installer fixtures only
//! exercise the legacy embedded-packages form.

use crate::packagist::PackagistVersion;
use crate::repository_filter::RepositoryFilter;
use indexmap::IndexSet;
use mozart_core::package::RawRepository;
use std::path::PathBuf;

/// One package version drawn from a `type: composer` repository.
pub struct ComposerRepoPackage {
    pub name: String,
    pub version: PackagistVersion,
}

/// Read every package version from `type: composer` repositories declared in
/// `composer.json`. Only `file://` URLs are supported here — they're what
/// the installer fixtures use after the harness rewrites
/// `file://foobar` → `file:///abs/path/to/fixtures/foobar`.
pub fn collect_composer_packages(repositories: &[RawRepository]) -> Vec<ComposerRepoPackage> {
    let mut out = Vec::new();
    let mut claimed: IndexSet<String> = IndexSet::new();
    for repo in repositories {
        if repo.repo_type != "composer" {
            continue;
        }
        let Some(url) = repo.url.as_deref() else {
            continue;
        };
        let Some(dir) = file_url_to_path(url) else {
            continue;
        };
        let packages_json = dir.join("packages.json");
        let Ok(content) = std::fs::read_to_string(&packages_json) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let Some(packages) = parsed.get("packages").and_then(|v| v.as_object()) else {
            continue;
        };
        let filter = RepositoryFilter::from_repo(repo);
        let mut names_this_repo: IndexSet<String> = IndexSet::new();
        for (name, versions) in packages {
            if !filter.is_allowed(name) {
                continue;
            }
            if claimed.contains(name) {
                continue;
            }
            let Some(versions_obj) = versions.as_object() else {
                continue;
            };
            let mut emitted = false;
            for (_, version_value) in versions_obj {
                if let Ok(pv) = serde_json::from_value::<PackagistVersion>(version_value.clone()) {
                    out.push(ComposerRepoPackage {
                        name: name.clone(),
                        version: pv,
                    });
                    emitted = true;
                }
            }
            if emitted {
                names_this_repo.insert(name.clone());
            }
        }
        if filter.canonical {
            claimed.extend(names_this_repo);
        }
    }
    out
}

/// Turn a `file://` URL into a filesystem path. Accepts both
/// `file:///abs/path` (RFC 8089 form) and `file://abs/path` (Composer's
/// loose form). Returns `None` for non-`file://` URLs.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // RFC 8089: file:///abs/path → empty authority, rest starts with `/`.
    // Composer's harness writes `file:///abs/...` after rewriting, so the
    // typical input here is one leading `/`.
    Some(PathBuf::from(rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_packages_json(dir: &std::path::Path, body: &str) {
        fs::write(dir.join("packages.json"), body).unwrap();
    }

    fn composer_repo(url: String) -> RawRepository {
        RawRepository {
            repo_type: "composer".to_string(),
            url: Some(url),
            package: None,
            only: None,
            exclude: None,
            canonical: None,
            security_advisories: None,
        }
    }

    #[test]
    fn reads_legacy_packages_json() {
        let tmp = TempDir::new().unwrap();
        write_packages_json(
            tmp.path(),
            r#"{
                "packages": {
                    "a/a": {
                        "dev-foobar": {
                            "name": "a/a",
                            "version": "dev-foobar",
                            "version_normalized": "dev-foobar"
                        }
                    }
                }
            }"#,
        );
        let url = format!("file://{}", tmp.path().display());
        let repos = vec![composer_repo(url)];
        let pkgs = collect_composer_packages(&repos);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "a/a");
        assert_eq!(pkgs[0].version.version, "dev-foobar");
    }

    #[test]
    fn ignores_non_composer_types() {
        let repos = vec![RawRepository {
            repo_type: "vcs".to_string(),
            url: Some("https://example.com/foo.git".to_string()),
            package: None,
            only: None,
            exclude: None,
            canonical: None,
            security_advisories: None,
        }];
        assert!(collect_composer_packages(&repos).is_empty());
    }

    #[test]
    fn skips_missing_packages_json() {
        let tmp = TempDir::new().unwrap();
        let url = format!("file://{}", tmp.path().display());
        let repos = vec![composer_repo(url)];
        assert!(collect_composer_packages(&repos).is_empty());
    }
}
