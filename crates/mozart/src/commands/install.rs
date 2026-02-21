use crate::console;
use crate::downloader;
use crate::installed;
use crate::lockfile;
use clap::Args;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct InstallArgs {
    /// Package(s) to install
    pub packages: Vec<String>,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long)]
    pub prefer_install: Option<String>,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Only download packages, do not install
    #[arg(long)]
    pub download_only: bool,

    /// [Deprecated] Enables installation of require-dev packages
    #[arg(long)]
    pub dev: bool,

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Skip the install step
    #[arg(long)]
    pub no_install: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

    /// Run audit after installation
    #[arg(long)]
    pub audit: bool,

    /// Audit output format
    #[arg(long)]
    pub audit_format: Option<String>,

    /// Optimizes PSR-0 and PSR-4 packages to be loaded with classmaps
    #[arg(short, long)]
    pub optimize_autoloader: bool,

    /// Autoload classes from the classmap only
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,

    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu_autoloader: bool,

    /// Use a custom prefix for the APCu autoloader cache
    #[arg(long)]
    pub apcu_autoloader_prefix: Option<String>,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,
}

/// Configuration for `install_from_lock`, replacing positional boolean parameters.
pub struct InstallConfig {
    /// Install dev dependencies as well as prod dependencies.
    pub dev_mode: bool,
    /// Print what would happen without making changes.
    pub dry_run: bool,
    /// Skip generating autoload files.
    pub no_autoloader: bool,
    /// Suppress download progress bars.
    pub no_progress: bool,
    /// Ignore all platform requirements (php, ext-*, lib-*).
    pub ignore_platform_reqs: bool,
    /// Ignore specific platform requirements by name.
    pub ignore_platform_req: Vec<String>,
    /// Optimize autoloader by generating a classmap.
    pub optimize_autoloader: bool,
    /// Use classmap-only autoloading (implies optimize_autoloader).
    pub classmap_authoritative: bool,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            dev_mode: true,
            dry_run: false,
            no_autoloader: false,
            no_progress: false,
            ignore_platform_reqs: false,
            ignore_platform_req: vec![],
            optimize_autoloader: false,
            classmap_authoritative: false,
        }
    }
}

/// The action to take for a package during install.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    Update,
    Skip,
}

/// An operation to perform during install.
pub struct InstallOp<'a> {
    pub package: &'a lockfile::LockedPackage,
    pub action: Action,
}

/// Resolve the working directory from the CLI option, falling back to cwd.
pub fn resolve_working_dir(cli: &super::Cli) -> PathBuf {
    match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().expect("Failed to determine current directory"),
    }
}

/// Compute install operations by comparing locked packages against installed packages.
///
/// Returns a tuple of (ops, removals) where:
/// - ops: list of (package, action) for each locked package
/// - removals: list of package names that are installed but not locked
pub fn compute_operations<'a>(
    locked: &[&'a lockfile::LockedPackage],
    installed: &installed::InstalledPackages,
) -> (Vec<(&'a lockfile::LockedPackage, Action)>, Vec<String>) {
    let mut ops: Vec<(&'a lockfile::LockedPackage, Action)> = Vec::new();

    for pkg in locked {
        if installed.is_installed(&pkg.name, &pkg.version) {
            ops.push((pkg, Action::Skip));
        } else if installed
            .packages
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&pkg.name))
        {
            ops.push((pkg, Action::Update));
        } else {
            ops.push((pkg, Action::Install));
        }
    }

    // Compute removals: packages in installed but not in locked
    let locked_names: HashSet<String> = locked.iter().map(|p| p.name.to_lowercase()).collect();

    let removals: Vec<String> = installed
        .packages
        .iter()
        .filter(|p| !locked_names.contains(&p.name.to_lowercase()))
        .map(|p| p.name.clone())
        .collect();

    (ops, removals)
}

/// Convert a LockedPackage to an InstalledPackageEntry.
pub fn locked_to_installed_entry(
    pkg: &lockfile::LockedPackage,
    _vendor_dir: &Path,
) -> installed::InstalledPackageEntry {
    // Composer uses a path relative to vendor/composer/installed.json
    let install_path = format!("../{}", pkg.name);

    installed::InstalledPackageEntry {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        version_normalized: pkg.version_normalized.clone(),
        source: pkg
            .source
            .as_ref()
            .map(|s| serde_json::to_value(s).unwrap_or_default()),
        dist: pkg
            .dist
            .as_ref()
            .map(|d| serde_json::to_value(d).unwrap_or_default()),
        package_type: pkg.package_type.clone(),
        install_path: Some(install_path),
        autoload: pkg.autoload.clone(),
        aliases: vec![],
        extra_fields: BTreeMap::new(),
    }
}

/// Clean up empty vendor namespace directories after removals.
pub fn cleanup_empty_vendor_dirs(vendor_dir: &Path) -> anyhow::Result<()> {
    if let Ok(entries) = std::fs::read_dir(vendor_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip "composer" dir and "bin" dir
                if name == "composer" || name == "bin" {
                    continue;
                }
                // If the namespace dir is empty, remove it
                if std::fs::read_dir(&path)?.next().is_none() {
                    std::fs::remove_dir(&path)?;
                }
            }
        }
    }
    Ok(())
}

/// Check whether a package name refers to a platform package.
///
/// Platform packages are: names starting with "php", "ext-", or "lib-".
fn is_platform_package(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "php"
        || lower.starts_with("php-")
        || lower.starts_with("ext-")
        || lower.starts_with("lib-")
}

/// Warn about platform requirements found in locked packages.
///
/// Iterates all locked packages' `require` fields, filters for platform entries,
/// and emits a warning for any that are not in the ignore list (unless
/// `ignore_platform_reqs` is set).
fn warn_platform_requirements(
    packages: &[&lockfile::LockedPackage],
    ignore_platform_reqs: bool,
    ignore_platform_req: &[String],
) {
    if ignore_platform_reqs {
        return;
    }

    let ignored_set: HashSet<String> = ignore_platform_req
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    for pkg in packages {
        for (req_name, req_constraint) in &pkg.require {
            if is_platform_package(req_name) {
                let lower = req_name.to_lowercase();
                if !ignored_set.contains(&lower) {
                    eprintln!(
                        "{}",
                        console::warning(&format!(
                            "Platform requirement {req_name} {req_constraint} (required by {}) \
                             has not been verified. Platform detection is not yet fully implemented.",
                            pkg.name
                        ))
                    );
                }
            }
        }
    }
}

/// Create a download progress tracker for a package.
fn make_progress(show: bool, pkg_name: &str, version: &str) -> downloader::DownloadProgress {
    downloader::DownloadProgress::new(show, format!("{pkg_name} ({version})"))
}

/// Install packages from a lock file into vendor/.
///
/// Used by both the `install` and `update` commands.
///
/// This function:
/// 1. Determines which packages to install (prod + optionally dev)
/// 2. Warns about platform requirements (unless ignored)
/// 3. Reads currently installed packages
/// 4. Computes install/update/skip/removal operations
/// 5. Prints a summary
/// 6. Executes downloads with optional progress bars (unless dry_run)
/// 7. Writes vendor/composer/installed.json
/// 8. Cleans up empty vendor directories
/// 9. Generates the autoloader (unless no_autoloader)
pub fn install_from_lock(
    lock: &lockfile::LockFile,
    working_dir: &Path,
    vendor_dir: &Path,
    config: &InstallConfig,
) -> anyhow::Result<()> {
    let dev_mode = config.dev_mode;

    // Step 1: Determine which packages to install
    let mut packages_to_install: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();

    if dev_mode && let Some(ref dev_pkgs) = lock.packages_dev {
        packages_to_install.extend(dev_pkgs.iter());
    }

    // Print install mode header
    if dev_mode {
        eprintln!("Installing dependencies from lock file (including require-dev)");
    } else {
        eprintln!("Installing dependencies from lock file");
    }
    eprintln!("Verifying lock file contents can be installed on current platform.");

    // Step 2: Warn about platform requirements
    warn_platform_requirements(
        &packages_to_install,
        config.ignore_platform_reqs,
        &config.ignore_platform_req,
    );

    // Step 3: Read currently installed packages
    let installed = installed::InstalledPackages::read(vendor_dir)?;

    // Step 4: Compute install operations
    let (ops, removals) = compute_operations(&packages_to_install, &installed);

    // Step 5: Print operation summary
    let installs: Vec<_> = ops
        .iter()
        .filter(|(_, a)| matches!(a, Action::Install))
        .collect();
    let updates: Vec<_> = ops
        .iter()
        .filter(|(_, a)| matches!(a, Action::Update))
        .collect();

    if installs.is_empty() && updates.is_empty() && removals.is_empty() {
        eprintln!("Nothing to install, update or remove");
    } else {
        eprintln!(
            "{}",
            console::info(&format!(
                "Package operations: {} install{}, {} update{}, {} removal{}",
                installs.len(),
                if installs.len() == 1 { "" } else { "s" },
                updates.len(),
                if updates.len() == 1 { "" } else { "s" },
                removals.len(),
                if removals.len() == 1 { "" } else { "s" },
            ))
        );
    }

    // Step 6: Execute operations (unless dry_run)
    if config.dry_run {
        for (pkg, action) in &ops {
            match action {
                Action::Skip => {}
                Action::Install => {
                    eprintln!("  - Would install {} ({})", pkg.name, pkg.version);
                }
                Action::Update => {
                    eprintln!("  - Would update {} ({})", pkg.name, pkg.version);
                }
            }
        }
        for name in &removals {
            eprintln!("  - Would remove {name}");
        }
    } else {
        for (pkg, action) in &ops {
            match action {
                Action::Skip => continue,
                Action::Install => {
                    eprintln!("  - Installing {} ({})", pkg.name, pkg.version);
                }
                Action::Update => {
                    eprintln!("  - Updating {} ({})", pkg.name, pkg.version);
                }
            }

            let dist = pkg.dist.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Package {} has no dist information — source installs are not yet supported",
                    pkg.name
                )
            })?;

            let mut progress = make_progress(!config.no_progress, &pkg.name, &pkg.version);

            downloader::install_package(
                &dist.url,
                &dist.dist_type,
                dist.shasum.as_deref(),
                vendor_dir,
                &pkg.name,
                Some(&mut progress),
            )?;

            progress.finish();
        }

        // Handle removals
        for name in &removals {
            eprintln!("  - Removing {name}");
            let pkg_dir = vendor_dir.join(name);
            if pkg_dir.exists() {
                std::fs::remove_dir_all(&pkg_dir)?;
            }
        }

        // Step 7: Clean up empty vendor namespace directories
        if !removals.is_empty() {
            cleanup_empty_vendor_dirs(vendor_dir)?;
        }

        // Step 8: Write updated vendor/composer/installed.json
        let mut new_installed = installed::InstalledPackages::new();
        new_installed.dev = dev_mode;

        // Collect dev package names from lock
        if dev_mode && let Some(ref dev_pkgs) = lock.packages_dev {
            new_installed.dev_package_names = dev_pkgs.iter().map(|p| p.name.clone()).collect();
        }

        for pkg in &packages_to_install {
            new_installed.upsert(locked_to_installed_entry(pkg, vendor_dir));
        }

        new_installed.write(vendor_dir)?;

        // Step 9: Generate autoloader (unless no_autoloader)
        if !config.no_autoloader {
            eprintln!("Generating autoload files");

            if config.classmap_authoritative {
                eprintln!(
                    "{}",
                    console::info(
                        "Classmap-authoritative mode: autoloader will only look up classes in the classmap."
                    )
                );
            } else if config.optimize_autoloader {
                eprintln!(
                    "{}",
                    console::info(
                        "Optimize autoloader: classmap scanning is not yet fully supported. \
                         PSR-4/PSR-0 autoloading will still be used."
                    )
                );
            }

            let suffix = lock.content_hash.clone();

            crate::autoload::generate(&crate::autoload::AutoloadConfig {
                project_dir: working_dir.to_path_buf(),
                vendor_dir: vendor_dir.to_path_buf(),
                dev_mode,
                suffix,
                classmap_authoritative: config.classmap_authoritative,
            })?;

            eprintln!("Generated autoload files");
        }
    }

    Ok(())
}

pub fn execute(args: &InstallArgs, cli: &super::Cli) -> anyhow::Result<()> {
    // Step 1: Resolve the working directory
    let working_dir = resolve_working_dir(cli);

    // Step 2: Validate arguments
    if !args.packages.is_empty() {
        let pkgs = args.packages.join(" ");
        eprintln!(
            "{}",
            console::error(&format!(
                "Invalid argument {pkgs}. Use \"mozart require {pkgs}\" instead to add packages to your composer.json."
            ))
        );
        std::process::exit(1);
    }

    if args.no_install {
        eprintln!(
            "{}",
            console::error(
                "Invalid option \"--no-install\". Use \"mozart update --no-install\" instead if you are trying to update the composer.lock file."
            )
        );
        std::process::exit(1);
    }

    if args.dev {
        eprintln!(
            "{}",
            console::warning(
                "The --dev option is deprecated. Dev packages are installed by default."
            )
        );
    }

    if args.no_suggest {
        eprintln!(
            "{}",
            console::warning("The --no-suggest option is deprecated and has no effect.")
        );
    }

    // Step 3: Read composer.lock
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        eprintln!(
            "{}",
            console::warning(
                "No composer.lock file present. Run \"mozart update\" to generate one."
            )
        );
        std::process::exit(1);
    }
    let lock = lockfile::LockFile::read_from_file(&lock_path)?;

    // Step 4: Freshness check
    let composer_json_path = working_dir.join("composer.json");
    if composer_json_path.exists() {
        let content = std::fs::read_to_string(&composer_json_path)?;
        if !lock.is_fresh(&content) {
            eprintln!(
                "{}",
                console::warning(
                    "Warning: The lock file is not up to date with the latest changes in composer.json. You may be getting outdated dependencies. It is recommended that you run `mozart update`."
                )
            );
        }
    }

    // Step 5: Warn about prefer-source (not yet supported)
    let prefer_source = args.prefer_source
        || args
            .prefer_install
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("source"))
            .unwrap_or(false);
    if prefer_source {
        eprintln!(
            "{}",
            console::warning(
                "Warning: Source installs are not yet supported. Falling back to dist."
            )
        );
    }

    // Step 6: Determine dev mode and vendor directory
    let dev_mode = !args.no_dev;
    let vendor_dir = working_dir.join("vendor");

    // Step 7: Delegate to shared install_from_lock()
    install_from_lock(
        &lock,
        &working_dir,
        &vendor_dir,
        &InstallConfig {
            dev_mode,
            dry_run: args.dry_run,
            no_autoloader: args.no_autoloader,
            no_progress: args.no_progress,
            ignore_platform_reqs: args.ignore_platform_reqs,
            ignore_platform_req: args.ignore_platform_req.clone(),
            optimize_autoloader: args.optimize_autoloader,
            classmap_authoritative: args.classmap_authoritative,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn make_locked_package(name: &str, version: &str) -> lockfile::LockedPackage {
        lockfile::LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            conflict: BTreeMap::new(),
            suggest: None,
            package_type: Some("library".to_string()),
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
        }
    }

    fn make_installed_entry(name: &str, version: &str) -> installed::InstalledPackageEntry {
        installed::InstalledPackageEntry {
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

    fn minimal_lock(packages: Vec<lockfile::LockedPackage>) -> lockfile::LockFile {
        lockfile::LockFile {
            readme: lockfile::LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages,
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

    // -----------------------------------------------------------------------
    // compute_operations tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_operations_all_new() {
        let locked = vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let installed = installed::InstalledPackages::new();

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0].1, Action::Install));
        assert!(matches!(ops[1].1, Action::Install));
        assert!(removals.is_empty());
    }

    #[test]
    fn test_compute_operations_all_skipped() {
        let locked = vec![make_locked_package("psr/log", "3.0.0")];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let mut installed = installed::InstalledPackages::new();
        installed.upsert(make_installed_entry("psr/log", "3.0.0"));

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].1, Action::Skip));
        assert!(removals.is_empty());
    }

    #[test]
    fn test_compute_operations_update_needed() {
        let locked = vec![make_locked_package("psr/log", "3.0.1")];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let mut installed = installed::InstalledPackages::new();
        installed.upsert(make_installed_entry("psr/log", "3.0.0"));

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].1, Action::Update));
        assert!(removals.is_empty());
    }

    #[test]
    fn test_compute_operations_removals() {
        let locked = vec![make_locked_package("psr/log", "3.0.0")];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let mut installed = installed::InstalledPackages::new();
        installed.upsert(make_installed_entry("psr/log", "3.0.0"));
        installed.upsert(make_installed_entry("monolog/monolog", "3.8.0"));

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].1, Action::Skip));
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0], "monolog/monolog");
    }

    #[test]
    fn test_compute_operations_mixed() {
        let locked = vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("symfony/console", "7.2.3"),
            make_locked_package("monolog/monolog", "3.8.1"),
        ];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let mut installed = installed::InstalledPackages::new();
        // psr/log already at correct version -> skip
        installed.upsert(make_installed_entry("psr/log", "3.0.0"));
        // monolog at wrong version -> update
        installed.upsert(make_installed_entry("monolog/monolog", "3.8.0"));
        // old-package not in locked -> removal
        installed.upsert(make_installed_entry("old/package", "1.0.0"));
        // symfony/console not installed at all -> install

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert_eq!(ops.len(), 3);

        let psr = ops.iter().find(|(p, _)| p.name == "psr/log").unwrap();
        assert!(matches!(psr.1, Action::Skip));

        let symfony = ops
            .iter()
            .find(|(p, _)| p.name == "symfony/console")
            .unwrap();
        assert!(matches!(symfony.1, Action::Install));

        let monolog = ops
            .iter()
            .find(|(p, _)| p.name == "monolog/monolog")
            .unwrap();
        assert!(matches!(monolog.1, Action::Update));

        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0], "old/package");
    }

    #[test]
    fn test_compute_operations_case_insensitive() {
        let locked = vec![make_locked_package("Monolog/Monolog", "3.8.0")];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let mut installed = installed::InstalledPackages::new();
        installed.upsert(make_installed_entry("monolog/monolog", "3.8.0"));

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].1, Action::Skip));
        assert!(removals.is_empty());
    }

    #[test]
    fn test_compute_operations_empty_lock() {
        let locked: Vec<lockfile::LockedPackage> = vec![];
        let locked_refs: Vec<&lockfile::LockedPackage> = locked.iter().collect();
        let mut installed = installed::InstalledPackages::new();
        installed.upsert(make_installed_entry("old/package", "1.0.0"));

        let (ops, removals) = compute_operations(&locked_refs, &installed);

        assert!(ops.is_empty());
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0], "old/package");
    }

    // -----------------------------------------------------------------------
    // locked_to_installed_entry tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_locked_to_installed_entry_conversion() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");

        let mut pkg = make_locked_package("psr/log", "3.0.2");
        pkg.version_normalized = Some("3.0.2.0".to_string());
        pkg.package_type = Some("library".to_string());
        pkg.autoload = Some(serde_json::json!({"psr-4": {"Psr\\Log\\": "src/"}}));

        let entry = locked_to_installed_entry(&pkg, &vendor_dir);

        assert_eq!(entry.name, "psr/log");
        assert_eq!(entry.version, "3.0.2");
        assert_eq!(entry.version_normalized.as_deref(), Some("3.0.2.0"));
        assert_eq!(entry.package_type.as_deref(), Some("library"));
        assert_eq!(entry.install_path.as_deref(), Some("../psr/log"));
        assert!(entry.autoload.is_some());
        assert!(entry.aliases.is_empty());
        assert!(entry.extra_fields.is_empty());
        assert!(entry.source.is_none());
        assert!(entry.dist.is_none());
    }

    #[test]
    fn test_locked_to_installed_entry_with_dist() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");

        let mut pkg = make_locked_package("monolog/monolog", "3.8.0");
        pkg.dist = Some(lockfile::LockedDist {
            dist_type: "zip".to_string(),
            url: "https://example.com/monolog.zip".to_string(),
            reference: Some("abc123".to_string()),
            shasum: Some("deadbeef".to_string()),
        });

        let entry = locked_to_installed_entry(&pkg, &vendor_dir);

        assert_eq!(entry.name, "monolog/monolog");
        assert_eq!(entry.install_path.as_deref(), Some("../monolog/monolog"));
        assert!(entry.dist.is_some());
    }

    // -----------------------------------------------------------------------
    // installed.json generation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_installed_json_written_from_lock() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");

        // Write a lock file
        let lock_path = dir.path().join("composer.lock");
        let lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.2"),
            make_locked_package("vendor/pkg", "1.2.3"),
        ]);
        lock.write_to_file(&lock_path).unwrap();

        // Simulate what execute() does for the installed.json write step
        let packages_to_install: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();
        let mut new_installed = installed::InstalledPackages::new();
        new_installed.dev = false;
        for pkg in &packages_to_install {
            new_installed.upsert(locked_to_installed_entry(pkg, &vendor_dir));
        }
        new_installed.write(&vendor_dir).unwrap();

        // Verify installed.json
        let loaded = installed::InstalledPackages::read(&vendor_dir).unwrap();
        assert_eq!(loaded.packages.len(), 2);
        assert!(loaded.is_installed("psr/log", "3.0.2"));
        assert!(loaded.is_installed("vendor/pkg", "1.2.3"));
        assert_eq!(
            loaded
                .packages
                .iter()
                .find(|p| p.name == "psr/log")
                .unwrap()
                .install_path
                .as_deref(),
            Some("../psr/log")
        );
    }

    #[test]
    fn test_installed_json_dev_package_names() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");

        let mut lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.2")]);
        lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);

        // Simulate dev mode installed.json generation
        let mut packages_to_install: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();
        if let Some(ref dev_pkgs) = lock.packages_dev {
            packages_to_install.extend(dev_pkgs.iter());
        }

        let mut new_installed = installed::InstalledPackages::new();
        new_installed.dev = true;
        if let Some(ref dev_pkgs) = lock.packages_dev {
            new_installed.dev_package_names = dev_pkgs.iter().map(|p| p.name.clone()).collect();
        }
        for pkg in &packages_to_install {
            new_installed.upsert(locked_to_installed_entry(pkg, &vendor_dir));
        }
        new_installed.write(&vendor_dir).unwrap();

        let loaded = installed::InstalledPackages::read(&vendor_dir).unwrap();
        assert_eq!(loaded.packages.len(), 2);
        assert!(loaded.dev);
        assert_eq!(loaded.dev_package_names, vec!["phpunit/phpunit"]);
    }

    // -----------------------------------------------------------------------
    // cleanup_empty_vendor_dirs tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_cleanup_empty_vendor_dirs_removes_empty() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");
        std::fs::create_dir_all(&vendor_dir).unwrap();

        // Create an empty namespace dir
        let empty_ns = vendor_dir.join("old-vendor");
        std::fs::create_dir_all(&empty_ns).unwrap();

        // Create a non-empty namespace dir
        let nonempty_ns = vendor_dir.join("psr");
        std::fs::create_dir_all(nonempty_ns.join("log")).unwrap();

        // Create the composer dir (should be skipped)
        std::fs::create_dir_all(vendor_dir.join("composer")).unwrap();

        cleanup_empty_vendor_dirs(&vendor_dir).unwrap();

        assert!(!empty_ns.exists(), "empty namespace dir should be removed");
        assert!(
            vendor_dir.join("psr").exists(),
            "non-empty namespace dir should remain"
        );
        assert!(
            vendor_dir.join("composer").exists(),
            "composer dir should be preserved"
        );
    }

    #[test]
    fn test_cleanup_empty_vendor_dirs_skips_bin() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");
        std::fs::create_dir_all(&vendor_dir).unwrap();

        let bin_dir = vendor_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        cleanup_empty_vendor_dirs(&vendor_dir).unwrap();

        assert!(
            bin_dir.exists(),
            "bin dir should be preserved even if empty"
        );
    }
}
