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

use crate::composer::Composer;
use mozart_core::composer::{
    AutoloadGenerator, InstallationManager, InstallationSource, LocalPackage, LocalRepository,
    Locker, PackageReference, RepositoryManager,
};
use mozart_core::config::{Config, resolve_references};
use mozart_core::console::IoInterface;
use mozart_core::downloader::{DownloadManager, GitDownloader, HgDownloader, SvnDownloader};
use mozart_core::factory::create_config;
use mozart_core::package::archiver::ArchiveManager;
use mozart_core::package::{RootPackageData, read_from_file};
use mozart_core::repository::cache::Cache;
use mozart_core::util::Filesystem;
use mozart_core::vcs::process::ProcessExecutor;

/// Creates a Composer instance.
///
/// ref: \Composer\Factory\createComposer()
pub fn create_composer(
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    project_dir: std::path::PathBuf,
    composer_json: &std::path::Path,
) -> anyhow::Result<Composer> {
    let content = std::fs::read_to_string(composer_json)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;
    let mut config = create_config()?;
    if let Some(cfg_obj) = value.get("config").and_then(|v| v.as_object()) {
        let overrides: std::collections::BTreeMap<String, serde_json::Value> = cfg_obj
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        config.merge(&overrides)?;
    }
    resolve_references(&mut config);

    let package = RootPackageData::from_raw(read_from_file(composer_json)?);

    // Mirrors `Factory::createComposer`'s `vendorDir` handling. The
    // value out of `Config::get('vendor-dir')` already had `{$...}`
    // placeholders substituted, but it may still be relative — resolve
    // it against the project root so install paths are absolute.
    let vendor_dir = if std::path::Path::new(&config.vendor_dir).is_absolute() {
        std::path::PathBuf::from(&config.vendor_dir)
    } else {
        project_dir.join(&config.vendor_dir)
    };

    let (local_packages, dev_mode) = read_local_packages(&vendor_dir)?;
    let repository_manager =
        RepositoryManager::new(LocalRepository::with_dev_mode(local_packages, dev_mode));
    let installation_manager = InstallationManager::new(vendor_dir.clone());
    let dm = std::sync::Arc::new(tokio::sync::Mutex::new(create_download_manager(
        io, &config,
    )));
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
    let am = std::sync::Arc::new(tokio::sync::Mutex::new(create_archive_manager(dm.clone())));

    Ok(Composer::new(
        project_dir,
        config,
        package,
        repository_manager,
        installation_manager,
        dm,
        autoload_generator,
        locker,
        am,
    ))
}

pub fn create_download_manager(
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    config: &Config,
) -> DownloadManager {
    let cache = if config.cache_files_ttl > 0 {
        Some(Cache::new(
            std::path::PathBuf::from(config.cache_files_dir.clone()),
            config.cache_read_only,
        ))
    } else {
        None
    };
    let cache = cache.unwrap();

    let mut dm = DownloadManager::new(io, false, Filesystem::new(), cache);
    if let serde_json::Value::String(preferred) = &config.preferred_install {
        match preferred.as_str() {
            "dist" => dm.set_prefer_dist(true),
            "source" => dm.set_prefer_source(true),
            _ => (),
        }
    }

    if let serde_json::Value::Object(preferred) = &config.preferred_install {
        _ = preferred;
        unimplemented!()
    }

    dm.set_downloader(
        "git".to_owned(),
        Box::new(GitDownloader::new(
            ProcessExecutor::new(),
            std::path::PathBuf::from(config.cache_files_dir.clone()),
        )),
    );
    dm.set_downloader(
        "svn".to_owned(),
        Box::new(SvnDownloader::new(ProcessExecutor::new())),
    );
    dm.set_downloader(
        "hg".to_owned(),
        Box::new(HgDownloader::new(ProcessExecutor::new())),
    );

    dm
}

pub fn create_archive_manager(
    download_manager: std::sync::Arc<tokio::sync::Mutex<DownloadManager>>,
) -> ArchiveManager {
    ArchiveManager::new(download_manager)
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
fn read_local_packages(
    vendor_dir: &std::path::Path,
) -> anyhow::Result<(Vec<LocalPackage>, Option<bool>)> {
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
            .and_then(InstallationSource::parse);
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

fn read_package_reference(value: Option<&serde_json::Value>) -> Option<PackageReference> {
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
    Some(PackageReference {
        kind,
        url,
        reference,
        shasum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_core::console::Console;
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn io() -> Arc<Mutex<Box<dyn IoInterface>>> {
        Arc::new(Mutex::new(
            Box::new(Console::new(0, true, false, true, true)) as Box<dyn IoInterface>,
        ))
    }

    #[test]
    fn install_path_is_vendor_dir_plus_pretty_name() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("composer.json"), r#"{"name": "acme/app"}"#);
        write(
            &dir.path().join("vendor/composer/installed.json"),
            r#"{"packages": [{"name": "Vendor/Pkg", "version": "1.0.0"}]}"#,
        );

        let composer = Composer::require(io(), dir.path()).unwrap();
        let pkg = composer
            .repository_manager()
            .local_repository()
            .get_canonical_packages()
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

        let composer = Composer::require(io(), dir.path()).unwrap();
        let pkg = composer
            .repository_manager()
            .local_repository()
            .get_canonical_packages()
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

        let composer = Composer::require(io(), dir.path()).unwrap();
        let count = composer
            .repository_manager()
            .local_repository()
            .get_canonical_packages()
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

        let composer = Composer::require(io(), dir.path()).unwrap();
        let names: Vec<&str> = composer
            .repository_manager()
            .local_repository()
            .get_canonical_packages()
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

        use mozart_core::package::PackageInterface;
        let composer = Composer::require(io(), dir.path()).unwrap();
        assert_eq!(composer.package().name(), "acme/app");
        assert_eq!(
            composer
                .package()
                .requires()
                .get("vendor/pkg")
                .map(|l| l.constraint.as_str()),
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

        let composer = Composer::require(io(), dir.path()).unwrap();
        let pkg = composer
            .repository_manager()
            .local_repository()
            .get_canonical_packages()
            .next()
            .unwrap();

        let install_path = composer
            .installation_manager()
            .get_install_path(pkg)
            .unwrap();
        assert_eq!(install_path, dir.path().join("deps/vendor/pkg"));
    }
}
