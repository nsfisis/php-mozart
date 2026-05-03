//! Support for `type: path` repositories.
//!
//! Mirrors `Composer\Repository\PathRepository`: a path repo points at a
//! local directory containing a `composer.json`, and the resolver loads the
//! package from that file directly. Mozart does not yet support glob URLs or
//! the `versions` / `reference: none` options — only the bare
//! `{ type: path, url: ... }` form the installer fixtures exercise.
//!
//! Resolution model: a path repo is expanded into a synthetic
//! `type: package` [`RawRepository`] whose payload is the loaded composer.json
//! plus a `dist` block. After this expansion the rest of the registry treats
//! the package the same as any inline `type: package` entry — that is the
//! whole point of doing the work here rather than threading a new repo type
//! through the resolver / lockfile.
//!
//! `dist.reference` matches Composer's `hash('sha1', $json . serialize($options))`
//! where `$options` carries the auto-detected `relative` flag (true when the
//! original URL was not absolute). The same SHA-1 ends up in the lockfile, so
//! consumers comparing references against Composer-produced lockfiles see
//! byte-identical values.

use std::path::{Path, PathBuf};

use mozart_core::package::RawRepository;
use mozart_php_serialize::{Value as PhpValue, serialize as php_serialize};
use sha1::{Digest, Sha1};

/// Translate path repos in `repositories` into synthetic `type: package`
/// entries. Non-path entries are returned unchanged in original order.
///
/// `base_dir` is the directory used to resolve relative `url` values
/// (Composer's PHP code resolves these against the process cwd; in production
/// that equals the project root, in tests it equals the fixtures anchor).
///
/// Failures (missing directory, unreadable composer.json, missing
/// `name`/`version`) drop the offending entry silently — the rest of the
/// repository list still applies. This mirrors Composer's lenient
/// PathRepository, which logs a warning and moves on rather than aborting the
/// whole resolve.
pub fn expand_path_repositories(
    repositories: &[RawRepository],
    base_dir: &Path,
) -> Vec<RawRepository> {
    let mut out = Vec::with_capacity(repositories.len());
    for repo in repositories {
        if repo.repo_type != "path" {
            out.push(repo.clone());
            continue;
        }
        let Some(url) = repo.url.as_deref() else {
            continue;
        };
        let Some(synthetic) = load_path_package(url, base_dir) else {
            continue;
        };
        out.push(synthetic);
    }
    out
}

/// Read one path repo's `composer.json` and synthesize the inline-package
/// form. Returns `None` for any I/O or parse failure (Composer behaves the
/// same — `PathRepository::initialize` skips entries whose `composer.json`
/// is missing).
fn load_path_package(url: &str, base_dir: &Path) -> Option<RawRepository> {
    let resolved = resolve_path(url, base_dir);
    let composer_json_path = resolved.join("composer.json");
    let json = std::fs::read_to_string(&composer_json_path).ok()?;
    let mut package: serde_json::Value = serde_json::from_str(&json).ok()?;
    let obj = package.as_object_mut()?;

    // `version` is mandatory in the inline-package representation: without it
    // the resolver would skip the package. Composer's PathRepository falls
    // back to `dev-main` when no version is declared and no VCS is present;
    // mirror that so a path repo whose composer.json omits `version` still
    // produces a usable entry.
    if !obj.contains_key("version") {
        obj.insert(
            "version".to_string(),
            serde_json::Value::String("dev-main".to_string()),
        );
    }

    let is_relative = !Path::new(url).is_absolute();
    let reference = compute_path_reference(json.as_bytes(), is_relative);

    obj.insert(
        "dist".to_string(),
        serde_json::json!({
            "type": "path",
            "url": url,
            "reference": reference,
        }),
    );
    // Composer copies `symlink`/`relative` from `options` into
    // `transport-options`. We have no `options` to forward today but emit an
    // empty object so consumers reading the package see the same shape.
    obj.entry("transport-options")
        .or_insert_with(|| serde_json::json!({}));

    Some(RawRepository {
        repo_type: "package".to_string(),
        url: None,
        package: Some(serde_json::Value::Array(vec![package])),
        only: None,
        exclude: None,
        canonical: None,
        security_advisories: None,
    })
}

fn resolve_path(url: &str, base_dir: &Path) -> PathBuf {
    let p = Path::new(url);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

/// Compose the SHA-1 reference Composer uses for path repos:
/// `sha1($json . serialize(['relative' => $isRelative]))`. The `relative`
/// flag is the only option Composer's auto-detection populates when the user
/// supplied no `options` block.
fn compute_path_reference(json_bytes: &[u8], is_relative: bool) -> String {
    let options = PhpValue::Array(vec![(
        PhpValue::String("relative".to_string()),
        PhpValue::Bool(is_relative),
    )]);
    let serialized = php_serialize(&options);
    let mut hasher = Sha1::new();
    hasher.update(json_bytes);
    hasher.update(serialized.as_bytes());
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_known_reference_for_plugin_a_fixture() {
        // Fixture used by partial-update-loads-root-aliases-for-path-repos.test.
        // Expected reference (`b133081...`) is what PHP's
        // `hash('sha1', file_get_contents($composerJson) . serialize(['relative' => true]))`
        // produces for this file — pin it here so reference computation
        // changes can't drift silently from Composer.
        let composer_json_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../composer/tests/Composer/Test/Fixtures/functional/installed-versions/plugin-a/composer.json");
        let bytes = std::fs::read(&composer_json_path).expect("fixture composer.json must exist");
        let reference = compute_path_reference(&bytes, true);
        assert!(
            reference.starts_with("b133081"),
            "unexpected reference: {reference}"
        );
    }

    #[test]
    fn relative_url_resolves_against_base_dir_and_emits_synthetic_package_repo() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("pkg-dir")).unwrap();
        std::fs::write(
            temp.path().join("pkg-dir").join("composer.json"),
            r#"{"name": "vendor/pkg", "version": "1.2.3"}"#,
        )
        .unwrap();

        let input = vec![RawRepository {
            repo_type: "path".to_string(),
            url: Some("pkg-dir".to_string()),
            package: None,
            only: None,
            exclude: None,
            canonical: None,
            security_advisories: None,
        }];
        let expanded = expand_path_repositories(&input, temp.path());
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].repo_type, "package");

        let pkgs = expanded[0]
            .package
            .as_ref()
            .expect("expanded entry must carry a package payload")
            .as_array()
            .expect("payload should be an array");
        assert_eq!(pkgs.len(), 1);
        let pkg = &pkgs[0];
        assert_eq!(pkg["name"], "vendor/pkg");
        assert_eq!(pkg["version"], "1.2.3");
        assert_eq!(pkg["dist"]["type"], "path");
        assert_eq!(pkg["dist"]["url"], "pkg-dir");
        assert!(
            pkg["dist"]["reference"]
                .as_str()
                .map(|s| s.len() == 40)
                .unwrap_or(false),
            "reference should be a 40-char SHA-1"
        );
    }

    #[test]
    fn missing_composer_json_drops_the_entry() {
        let temp = tempfile::tempdir().unwrap();
        let input = vec![RawRepository {
            repo_type: "path".to_string(),
            url: Some("does-not-exist".to_string()),
            package: None,
            only: None,
            exclude: None,
            canonical: None,
            security_advisories: None,
        }];
        let expanded = expand_path_repositories(&input, temp.path());
        assert!(expanded.is_empty());
    }

    #[test]
    fn non_path_repos_pass_through_unchanged() {
        let input = vec![RawRepository {
            repo_type: "vcs".to_string(),
            url: Some("https://example.com/repo.git".to_string()),
            package: None,
            only: None,
            exclude: None,
            canonical: None,
            security_advisories: None,
        }];
        let expanded = expand_path_repositories(&input, Path::new("/tmp"));
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].repo_type, "vcs");
        assert_eq!(
            expanded[0].url.as_deref(),
            Some("https://example.com/repo.git")
        );
    }
}
