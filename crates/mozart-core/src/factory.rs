//! Factory helpers for constructing Composer state.
//!
//! Ports the static factory methods from `Composer\Factory`. Today we
//! cover [`create_config`] (effective global [`Config`]) and
//! [`create_composer`] (the project-level [`Composer`] root, built from
//! `composer.json` plus the on-disk `vendor/composer/installed.json`).
//!
//! Auth loading, htaccess creation, and the plugin/event-dispatcher
//! wiring are intentionally omitted as they are out of scope for the
//! current port.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::composer::{
    AutoloadGenerator, Composer, InstallationManager, LocalPackage, LocalRepository, Locker,
    RepositoryManager, composer_home,
};
use crate::config::{Config, resolve_references};
use crate::package::read_from_file;

/// Rust port of `Factory::getCacheDir()`.
///
/// Priority:
/// 1. `$COMPOSER_CACHE_DIR` env var
/// 2. Windows: `%LOCALAPPDATA%/Composer`
/// 3. macOS:   `$HOME/Library/Caches/composer`
/// 4. Linux/other: `$XDG_CACHE_HOME/composer` (or `$HOME/.cache/composer`)
fn get_cache_dir(home: &std::path::Path) -> PathBuf {
    if let Ok(val) = std::env::var("COMPOSER_CACHE_DIR")
        && !val.is_empty()
    {
        return PathBuf::from(val);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA")
            && !local.is_empty()
        {
            return PathBuf::from(local).join("Composer");
        }
        return home.join("cache");
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(h) = std::env::var("HOME")
            && !h.is_empty()
        {
            return PathBuf::from(h)
                .join("Library")
                .join("Caches")
                .join("composer");
        }
        return home.join("cache");
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let cache_base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".cache"))
                    .unwrap_or_else(|_| home.join("cache"))
            });
        cache_base.join("composer")
    }
}

/// Rust port of `Factory::getDataDir()`.
///
/// Priority:
/// 1. `$COMPOSER_HOME` is set → use `home` (same path) as data dir
/// 2. Windows: `home`
/// 3. Linux/macOS: `$XDG_DATA_HOME/composer` (or `$HOME/.local/share/composer`)
fn get_data_dir(home: &std::path::Path) -> PathBuf {
    if std::env::var("COMPOSER_HOME").is_ok_and(|v| !v.is_empty()) {
        return home.to_path_buf();
    }

    #[cfg(target_os = "windows")]
    {
        return home.to_path_buf();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let data_base = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
                    .unwrap_or_else(|_| PathBuf::from("/tmp"))
            });
        data_base.join("composer")
    }
}

/// Rust port of `Factory::createConfig()`.
///
/// Builds the effective global [`Config`] by:
/// 1. Starting from `Config::default()`
/// 2. Setting `home`, `cache-dir`, and `data-dir` based on platform conventions
/// 3. Loading and merging `$COMPOSER_HOME/config.json` if it exists
///
/// Auth loading (`auth.json`, `COMPOSER_AUTH`) and htaccess-protect directory
/// creation are intentionally omitted.
///
/// **Callers must call [`crate::config::resolve_references`] after any
/// additional project-level merges.** This function does not call it
/// internally so that callers can overlay project config first.
pub fn create_config() -> anyhow::Result<Config> {
    let home = composer_home();
    let cache_dir = get_cache_dir(&home);
    let data_dir = get_data_dir(&home);

    let mut config = Config::default();

    // Inject home/cache-dir/data-dir as the platform-computed baseline.
    // `home` and `data-dir` have no dedicated fields on Config and land in `extra`.
    let mut defaults: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    defaults.insert(
        "home".to_string(),
        serde_json::json!(home.to_string_lossy().as_ref()),
    );
    defaults.insert(
        "cache-dir".to_string(),
        serde_json::json!(cache_dir.to_string_lossy().as_ref()),
    );
    defaults.insert(
        "data-dir".to_string(),
        serde_json::json!(data_dir.to_string_lossy().as_ref()),
    );
    config.merge(&defaults)?;

    // Load $COMPOSER_HOME/config.json global config
    let global_config_path = home.join("config.json");
    if global_config_path.exists() {
        let content = std::fs::read_to_string(&global_config_path)?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            anyhow::anyhow!("Failed to parse {}: {e}", global_config_path.display())
        })?;
        if let Some(obj) = json.get("config").and_then(|v| v.as_object()) {
            let overrides: BTreeMap<String, serde_json::Value> =
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            config.merge(&overrides)?;
        }
    }

    Ok(config)
}

/// Rust port of `Factory::createComposer()`.
///
/// Builds the project-level [`Composer`]:
/// 1. Read `composer.json` from `composer_json` and load it into both
///    the merged [`Config`] (overlaying [`create_config`]) and the
///    untyped [`crate::package::RawPackageData`].
/// 2. Resolve all `{$home}` / `{$vendor-dir}` placeholders via
///    [`resolve_references`].
/// 3. Resolve `vendor-dir` against `project_dir` if it is relative, so
///    the installation manager hands back absolute paths
///    (`Factory::createComposer` does the same via
///    `Filesystem::isAbsolutePath`).
/// 4. Wire up the [`InstallationManager`] and a [`RepositoryManager`]
///    whose local repository is populated from
///    `vendor/composer/installed.json` — the same role
///    `Factory::addLocalRepository` plays in PHP.
/// 5. Construct a fresh [`AutoloadGenerator`] with PHP defaults
///    (`new AutoloadGenerator($eventDispatcher, $io)` in PHP, minus the
///    not-yet-ported event dispatcher and IO dependencies).
/// 6. Construct a [`Locker`] pointed at `composer.lock` next to the
///    composer.json — same as `Factory::createComposer`'s
///    `new Locker($io, new JsonFile($lockFile, …), $im, $contents)`,
///    minus the IO/installation-manager/contents dependencies that
///    only matter once we port `setLockData`.
///
/// The plugin manager, download manager, and event dispatcher that
/// `Factory::createComposer` also wires up are not yet ported.
pub fn create_composer(project_dir: PathBuf, composer_json: &Path) -> anyhow::Result<Composer> {
    let content = std::fs::read_to_string(composer_json)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;
    let mut config = create_config()?;
    if let Some(cfg_obj) = value.get("config").and_then(|v| v.as_object()) {
        let overrides: BTreeMap<String, serde_json::Value> = cfg_obj
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        config.merge(&overrides)?;
    }
    resolve_references(&mut config);

    let package = read_from_file(composer_json)?;

    // Mirrors `Factory::createComposer`'s `vendorDir` handling. The
    // value out of `Config::get('vendor-dir')` already had `{$...}`
    // placeholders substituted, but it may still be relative — resolve
    // it against the project root so install paths are absolute.
    let vendor_dir = if Path::new(&config.vendor_dir).is_absolute() {
        PathBuf::from(&config.vendor_dir)
    } else {
        project_dir.join(&config.vendor_dir)
    };

    let (local_packages, dev_mode) = read_local_packages(&vendor_dir)?;
    let repository_manager =
        RepositoryManager::new(LocalRepository::with_dev_mode(local_packages, dev_mode));
    let installation_manager = InstallationManager::new(vendor_dir);
    let autoload_generator = AutoloadGenerator::new();

    // Mirrors `Factory::createComposer`'s lock-file path: the lockfile
    // sits next to composer.json, with `.json` swapped for `.lock`.
    let lock_file_path = composer_json
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| project_dir.clone())
        .join(
            composer_json
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.strip_suffix(".json").unwrap_or(n))
                .map(|stem| format!("{stem}.lock"))
                .unwrap_or_else(|| "composer.lock".to_string()),
        );
    let locker = Locker::new(lock_file_path);

    Ok(Composer::new(
        project_dir,
        config,
        package,
        repository_manager,
        installation_manager,
        autoload_generator,
        locker,
    ))
}

/// Read `vendor/composer/installed.json` into the minimal shape the
/// installation manager needs. Mirrors the relevant slice of
/// `Composer\Repository\FilesystemRepository::initialize`: accept both
/// the v2 object form (`{packages: [...]}`) and the legacy v1 array
/// form. Returns an empty list when the file is missing — the same
/// semantics as `FilesystemRepository::isFresh`.
///
/// We deliberately avoid pulling the full `InstalledPackages` reader from
/// `mozart-registry` here to keep `mozart-core` at the bottom of the
/// dependency graph; the parsing that's actually load-bearing for the
/// install-path computation is just the package name + optional
/// `target-dir`.
fn read_local_packages(vendor_dir: &Path) -> anyhow::Result<(Vec<LocalPackage>, Option<bool>)> {
    let path = vendor_dir.join("composer/installed.json");
    if !path.exists() {
        return Ok((Vec::new(), None));
    }
    let content = std::fs::read_to_string(&path)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;

    let (entries, dev_mode): (&[serde_json::Value], Option<bool>) = match &value {
        serde_json::Value::Object(obj) => {
            let entries = match obj.get("packages") {
                Some(serde_json::Value::Array(arr)) => arr.as_slice(),
                _ => return Ok((Vec::new(), obj.get("dev").and_then(|v| v.as_bool()))),
            };
            (entries, obj.get("dev").and_then(|v| v.as_bool()))
        }
        serde_json::Value::Array(arr) => (arr.as_slice(), None),
        _ => return Ok((Vec::new(), None)),
    };

    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let pretty_name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pretty_version = entry
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let target_dir = entry
            .get("target-dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let package_type = entry
            .get("type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let installation_source = entry
            .get("installation-source")
            .and_then(|v| v.as_str())
            .and_then(crate::composer::InstallationSource::parse);
        let source = read_package_reference(entry.get("source"));
        let dist = read_package_reference(entry.get("dist"));
        let extra = entry
            .get("extra")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        out.push(LocalPackage::new(
            pretty_name,
            pretty_version,
            target_dir,
            package_type,
            installation_source,
            source,
            dist,
            extra,
        ));
    }
    Ok((out, dev_mode))
}

fn read_package_reference(
    value: Option<&serde_json::Value>,
) -> Option<crate::composer::PackageReference> {
    let v = value?;
    let kind = v.get("type").and_then(|x| x.as_str())?.to_string();
    let url = v
        .get("url")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let reference = v
        .get("reference")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let shasum = v
        .get("shasum")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(crate::composer::PackageReference {
        kind,
        url,
        reference,
        shasum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_config_cache_dir_has_no_placeholder() {
        let config = create_config().unwrap();
        assert!(
            !config.cache_dir.contains("{$home}"),
            "cache_dir should not contain placeholder, got: {}",
            config.cache_dir
        );
        assert!(!config.cache_dir.is_empty());
    }

    #[test]
    fn test_create_config_home_accessible_via_get() {
        let config = create_config().unwrap();
        let home_val = config.get("home");
        assert!(home_val.is_some(), "config.get('home') should return Some");
        assert!(
            home_val
                .unwrap()
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "home should be a non-empty string"
        );
    }

    #[test]
    fn test_create_config_data_dir_accessible_via_get() {
        let config = create_config().unwrap();
        assert!(config.get("data-dir").is_some());
    }

    #[test]
    fn test_get_cache_dir_ends_with_composer() {
        let home = std::path::PathBuf::from("/tmp/test-home");
        let result = get_cache_dir(&home);
        assert!(
            result.to_string_lossy().contains("composer"),
            "cache dir should contain 'composer', got: {}",
            result.display()
        );
    }

    #[test]
    fn test_get_data_dir_ends_with_composer_when_no_composer_home() {
        // Only valid when COMPOSER_HOME is not set in the test environment.
        if std::env::var("COMPOSER_HOME").is_ok_and(|v| !v.is_empty()) {
            return;
        }
        let home = std::path::PathBuf::from("/tmp/test-home");
        let result = get_data_dir(&home);
        assert!(
            result.to_string_lossy().contains("composer"),
            "data dir should contain 'composer', got: {}",
            result.display()
        );
    }

    mod create_composer {
        use super::*;
        use std::fs;
        use tempfile::tempdir;

        fn write(path: &Path, content: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        #[test]
        fn install_path_is_vendor_dir_plus_pretty_name() {
            let dir = tempdir().unwrap();
            write(&dir.path().join("composer.json"), r#"{"name": "acme/app"}"#);
            write(
                &dir.path().join("vendor/composer/installed.json"),
                r#"{"packages": [{"name": "Vendor/Pkg", "version": "1.0.0"}]}"#,
            );

            let composer = Composer::require(dir.path()).unwrap();
            let pkg = composer
                .repository_manager()
                .local_repository()
                .canonical_packages()
                .next()
                .unwrap();

            let install_path = composer
                .installation_manager()
                .get_install_path(pkg)
                .unwrap();

            // Mirrors `LibraryInstaller::getInstallPath`:
            // `vendorDir + '/' + prettyName`. `pretty-name` is preserved
            // case (Composer/Repository/FilesystemRepository keeps the original).
            assert_eq!(install_path, dir.path().join("vendor").join("Vendor/Pkg"));
        }

        #[test]
        fn install_path_appends_target_dir() {
            let dir = tempdir().unwrap();
            write(&dir.path().join("composer.json"), r#"{"name": "acme/app"}"#);
            write(
                &dir.path().join("vendor/composer/installed.json"),
                r#"{"packages": [{"name": "vendor/pkg", "target-dir": "src/lib"}]}"#,
            );

            let composer = Composer::require(dir.path()).unwrap();
            let pkg = composer
                .repository_manager()
                .local_repository()
                .canonical_packages()
                .next()
                .unwrap();

            let install_path = composer
                .installation_manager()
                .get_install_path(pkg)
                .unwrap();

            assert_eq!(install_path, dir.path().join("vendor/vendor/pkg/src/lib"));
        }

        #[test]
        fn local_repository_is_empty_when_installed_json_missing() {
            let dir = tempdir().unwrap();
            write(&dir.path().join("composer.json"), r#"{"name": "acme/app"}"#);

            let composer = Composer::require(dir.path()).unwrap();
            let count = composer
                .repository_manager()
                .local_repository()
                .canonical_packages()
                .count();
            assert_eq!(count, 0);
        }

        #[test]
        fn local_repository_accepts_v1_array_form() {
            // Older Composer 1.x / fixture format: bare array of packages.
            // FilesystemRepository::initialize accepts this; our minimal
            // reader must too.
            let dir = tempdir().unwrap();
            write(&dir.path().join("composer.json"), r#"{"name": "acme/app"}"#);
            write(
                &dir.path().join("vendor/composer/installed.json"),
                r#"[{"name": "a/a"}, {"name": "b/b"}]"#,
            );

            let composer = Composer::require(dir.path()).unwrap();
            let names: Vec<&str> = composer
                .repository_manager()
                .local_repository()
                .canonical_packages()
                .map(|p| p.pretty_name())
                .collect();
            assert_eq!(names, vec!["a/a", "b/b"]);
        }

        #[test]
        fn package_returns_root_composer_json() {
            let dir = tempdir().unwrap();
            write(
                &dir.path().join("composer.json"),
                r#"{"name": "acme/app", "require": {"vendor/pkg": "^1.0"}}"#,
            );

            let composer = Composer::require(dir.path()).unwrap();
            assert_eq!(composer.package().name, "acme/app");
            assert_eq!(
                composer
                    .package()
                    .require
                    .get("vendor/pkg")
                    .map(String::as_str),
                Some("^1.0"),
            );
        }

        #[test]
        fn install_path_uses_configured_vendor_dir() {
            let dir = tempdir().unwrap();
            write(
                &dir.path().join("composer.json"),
                r#"{"name": "acme/app", "config": {"vendor-dir": "deps"}}"#,
            );
            write(
                &dir.path().join("deps/composer/installed.json"),
                r#"{"packages": [{"name": "vendor/pkg"}]}"#,
            );

            let composer = Composer::require(dir.path()).unwrap();
            let pkg = composer
                .repository_manager()
                .local_repository()
                .canonical_packages()
                .next()
                .unwrap();

            let install_path = composer
                .installation_manager()
                .get_install_path(pkg)
                .unwrap();
            assert_eq!(install_path, dir.path().join("deps/vendor/pkg"));
        }
    }
}
