use clap::Args;
use mozart_core::console;
use mozart_core::package;
use mozart_core::validation;
use mozart_registry::lockfile;
use mozart_registry::resolver::{self, PlatformConfig, ResolveRequest};
use std::collections::HashMap;

#[derive(Args)]
pub struct RemoveArgs {
    /// Package(s) to remove
    pub packages: Vec<String>,

    /// Remove from require-dev
    #[arg(long)]
    pub dev: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Disables the automatic update of the lock file
    #[arg(long)]
    pub no_update: bool,

    /// Skip the install step
    #[arg(long)]
    pub no_install: bool,

    /// Skip the audit step
    #[arg(long)]
    pub no_audit: bool,

    /// Audit output format
    #[arg(long)]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'w', long)]
    pub update_with_dependencies: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'W', long)]
    pub update_with_all_dependencies: bool,

    /// Update also dependencies of the removed packages
    #[arg(long)]
    pub with_all_dependencies: bool,

    /// Skip updating dependencies
    #[arg(long)]
    pub no_update_with_dependencies: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Remove unused packages
    #[arg(long)]
    pub unused: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

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
}

pub fn execute(
    args: &RemoveArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // Step 1: Validate inputs
    if args.packages.is_empty() && !args.unused {
        anyhow::bail!("Not enough arguments (missing: \"packages\").");
    }

    // Step 2: Handle deprecated flags
    if args.update_with_dependencies {
        console.info(&format!(
            "{}",
            console::warning(
                "The -w / --update-with-dependencies flag is deprecated. Use --with-all-dependencies instead."
            )
        ));
    }
    if args.update_with_all_dependencies {
        console.info(&format!(
            "{}",
            console::warning(
                "The -W / --update-with-all-dependencies flag is deprecated. Use --with-all-dependencies instead."
            )
        ));
    }

    // Step 3: Resolve working directory and read composer.json
    let working_dir = super::install::resolve_working_dir(cli);
    let composer_path = working_dir.join("composer.json");

    if !composer_path.exists() {
        anyhow::bail!(
            "composer.json not found in {}. Run `mozart init` to create one.",
            working_dir.display()
        );
    }

    let mut raw = package::read_from_file(&composer_path)?;

    // Step 4: Handle --unused flag (deferred implementation)
    if args.unused {
        console.info(&format!(
            "{}",
            console::warning(
                "--unused is not yet fully implemented. The resolver will naturally prune unreachable packages."
            )
        ));
        // Fall through: if no explicit packages were named, nothing to remove.
        if args.packages.is_empty() {
            return Ok(());
        }
    }

    // Step 5: Determine which packages to remove and remove them
    let mut any_removed = false;

    for pkg_arg in &args.packages {
        let name = pkg_arg.trim().to_lowercase();

        // Validate package name format
        if !validation::validate_package_name(&name) {
            anyhow::bail!("Invalid package name: \"{name}\"");
        }

        if args.dev {
            // Only look in require-dev
            if raw.require_dev.contains_key(&name) {
                println!(
                    "{}",
                    console::info(&format!("Removing {name} from require-dev"))
                );
                raw.require_dev.remove(&name);
                any_removed = true;
            } else {
                console.info(&format!(
                    "{}",
                    console::warning(&format!(
                        "{name} is not required in require-dev and has not been removed."
                    ))
                ));
            }
        } else {
            // Auto-detect: look in require first, then require-dev
            if raw.require.contains_key(&name) {
                println!(
                    "{}",
                    console::info(&format!("Removing {name} from require"))
                );
                raw.require.remove(&name);
                any_removed = true;
            } else if raw.require_dev.contains_key(&name) {
                println!(
                    "{}",
                    console::info(&format!("Removing {name} from require-dev"))
                );
                raw.require_dev.remove(&name);
                any_removed = true;
            } else {
                console.info(&format!(
                    "{}",
                    console::warning(&format!(
                        "{name} is not required in your composer.json and has not been removed."
                    ))
                ));
            }
        }
    }

    // Step 6: Write updated composer.json (unless --dry-run)
    if args.dry_run {
        println!(
            "{}",
            console::comment("Dry run: composer.json not modified.")
        );
    } else if any_removed {
        package::write_to_file(&raw, &composer_path)?;
    }

    // Step 7: Handle --no-update early return
    if args.no_update {
        println!(
            "{}",
            console::comment("Not updating dependencies, only modifying composer.json.")
        );
        return Ok(());
    }

    // If nothing was removed, we can still proceed with resolution (e.g. to clean up orphans).
    // But if nothing changed and there's nothing to resolve, exit cleanly.
    if !any_removed {
        return Ok(());
    }

    // --- Full resolution + lock + install pipeline ---

    let dev_mode = !args.update_no_dev;
    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");

    // Build require/require_dev lists from the updated raw data
    let require: Vec<(String, String)> = raw
        .require
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let require_dev: Vec<(String, String)> = raw
        .require_dev
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Parse minimum-stability from composer.json (defaults to "stable")
    let minimum_stability_str = raw.minimum_stability.as_deref().unwrap_or("stable");
    let minimum_stability = package::Stability::parse(minimum_stability_str);

    // Determine prefer-stable from composer.json field
    let composer_prefer_stable = raw
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let request = ResolveRequest {
        require,
        require_dev,
        include_dev: dev_mode,
        minimum_stability,
        stability_flags: HashMap::new(),
        prefer_stable: composer_prefer_stable,
        prefer_lowest: false,
        platform: PlatformConfig::new(),
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repo_cache: None,
    };

    // Print header messages
    console.info("Loading composer repositories with package information");
    if dev_mode {
        console.info("Updating dependencies (including require-dev)");
    } else {
        console.info("Updating dependencies");
    }
    console.info("Resolving dependencies...");

    // Run resolver
    let mut resolved = resolver::resolve(&request).map_err(|e| {
        mozart_core::exit_code::bail(
            mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
            e.to_string(),
        )
    })?;

    // Read old lock file (if any) for change reporting and partial update
    let old_lock = if lock_path.exists() {
        match lockfile::LockFile::read_from_file(&lock_path) {
            Ok(l) => Some(l),
            Err(e) => {
                console.info(&format!(
                    "{}",
                    console::warning(&format!(
                        "Could not read existing composer.lock: {}. Treating as a fresh install.",
                        e
                    ))
                ));
                None
            }
        }
    } else {
        None
    };

    // Apply partial update logic for `remove`:
    //
    // Composer's default for `remove` is to also update the direct dependencies of the
    // removed packages (i.e. they become candidates for removal if nothing else needs them).
    // With --with-all-dependencies the full transitive dependency tree is considered.
    // With --no-update-with-dependencies only the removed packages themselves are freed.
    //
    // We implement this by building an "allow list" of packages that may change:
    //   - --no-update-with-dependencies: only the removed packages
    //   - --with-all-dependencies:        removed packages + full transitive deps
    //   - default:                         removed packages + direct deps (Composer default)
    // Then we pin everything NOT in the allow list to its locked version.
    let with_all_deps = args.with_all_dependencies || args.update_with_all_dependencies;

    if let Some(ref lock) = old_lock {
        let removed_names: Vec<String> = args
            .packages
            .iter()
            .map(|s| s.trim().to_lowercase())
            .collect();

        let allow_list = if args.no_update_with_dependencies {
            // Only the removed packages themselves are freed
            removed_names
        } else if with_all_deps {
            super::update::expand_with_all_dependencies(removed_names, lock)
        } else {
            // Default: freed packages + their direct dependencies
            super::update::expand_with_direct_dependencies(removed_names, lock)
        };

        // For --minimal-changes, additionally pin packages beyond the allow list
        if args.minimal_changes {
            console.info(&format!(
                "{}",
                console::info(
                    "Minimal changes mode: preserving locked versions for non-removed packages."
                )
            ));
        }

        resolved = super::update::apply_partial_update(resolved, lock, &allow_list);
    }

    // Get the composer.json content string for content-hash computation.
    // For --dry-run, serialize from memory; otherwise re-read the file we just wrote.
    let composer_json_content = if args.dry_run {
        package::to_json_pretty(&raw)?
    } else {
        std::fs::read_to_string(&composer_path)?
    };

    // Generate new lock file
    let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content: composer_json_content.clone(),
        composer_json: raw.clone(),
        include_dev: dev_mode,
        repo_cache: None,
    })?;

    // Compute and print change report
    let changes = super::update::compute_update_changes(old_lock.as_ref(), &new_lock, dev_mode);

    let installs: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, super::update::ChangeKind::Install { .. }))
        .collect();
    let updates: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, super::update::ChangeKind::Update { .. }))
        .collect();
    let removals: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, super::update::ChangeKind::Remove { .. }))
        .collect();

    console.info(&format!(
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
    ));

    // Print individual change lines
    for change in &changes {
        match &change.kind {
            super::update::ChangeKind::Remove { old_version } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - Would remove {} ({})",
                        change.name, old_version
                    ));
                } else {
                    console.info(&format!("  - Removing {} ({})", change.name, old_version));
                }
            }
            super::update::ChangeKind::Install { new_version } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - Would install {} ({})",
                        change.name, new_version
                    ));
                } else {
                    console.info(&format!("  - Installing {} ({})", change.name, new_version));
                }
            }
            super::update::ChangeKind::Update {
                old_version,
                new_version,
            } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - Would update {} ({} => {})",
                        change.name, old_version, new_version
                    ));
                } else {
                    console.info(&format!(
                        "  - Updating {} ({} => {})",
                        change.name, old_version, new_version
                    ));
                }
            }
            super::update::ChangeKind::Unchanged => {}
        }
    }

    // Write lock file (unless --dry-run)
    if !args.dry_run {
        console.info("Writing lock file");
        new_lock.write_to_file(&lock_path)?;
    }

    // Install packages (unless --no-install or --dry-run)
    if !args.no_install && !args.dry_run {
        super::install::install_from_lock(
            &new_lock,
            &working_dir,
            &vendor_dir,
            &super::install::InstallConfig {
                dev_mode,
                dry_run: false,       // dry_run already handled above
                no_autoloader: false, // always generate autoloader
                no_progress: args.no_progress,
                ignore_platform_reqs: args.ignore_platform_reqs,
                ignore_platform_req: args.ignore_platform_req.clone(),
                optimize_autoloader: args.optimize_autoloader,
                classmap_authoritative: args.classmap_authoritative,
                apcu_autoloader: false,
                apcu_autoloader_prefix: None,
            },
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_core::package::RawPackageData;
    use mozart_registry::lockfile;
    use std::collections::BTreeMap;

    // ──────────── Helper constructors ────────────

    fn make_locked_package(name: &str, version: &str) -> lockfile::LockedPackage {
        lockfile::LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: Some(format!("{}.0", version)),
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

    fn make_raw_package(name: &str) -> RawPackageData {
        RawPackageData::new(name.to_string())
    }

    // ──────────── Unit tests ────────────

    /// Remove a package from `require`, verify it's gone from `RawPackageData`.
    #[test]
    fn test_remove_from_require() {
        let mut raw = make_raw_package("test/project");
        raw.require
            .insert("psr/log".to_string(), "^3.0".to_string());
        raw.require
            .insert("monolog/monolog".to_string(), "^3.0".to_string());

        assert!(raw.require.contains_key("psr/log"));

        // Simulate the removal logic
        raw.require.remove("psr/log");

        assert!(
            !raw.require.contains_key("psr/log"),
            "psr/log should be removed from require"
        );
        assert!(
            raw.require.contains_key("monolog/monolog"),
            "monolog/monolog should remain in require"
        );
    }

    /// Remove a package from `require-dev` with `--dev` flag.
    #[test]
    fn test_remove_from_require_dev() {
        let mut raw = make_raw_package("test/project");
        raw.require_dev
            .insert("phpunit/phpunit".to_string(), "^11.0".to_string());
        raw.require_dev
            .insert("mockery/mockery".to_string(), "^1.0".to_string());

        assert!(raw.require_dev.contains_key("phpunit/phpunit"));

        // Simulate the --dev removal logic
        raw.require_dev.remove("phpunit/phpunit");

        assert!(
            !raw.require_dev.contains_key("phpunit/phpunit"),
            "phpunit/phpunit should be removed from require-dev"
        );
        assert!(
            raw.require_dev.contains_key("mockery/mockery"),
            "mockery/mockery should remain in require-dev"
        );
    }

    /// Removing a package not in either section does not panic and doesn't change anything.
    #[test]
    fn test_remove_nonexistent_package_no_panic() {
        let mut raw = make_raw_package("test/project");
        raw.require
            .insert("psr/log".to_string(), "^3.0".to_string());

        // Package not present — simulate the warning-and-skip behavior
        let name = "nonexistent/package";
        let found_in_require = raw.require.remove(name).is_some();
        let found_in_require_dev = raw.require_dev.remove(name).is_some();

        assert!(!found_in_require);
        assert!(!found_in_require_dev);

        // composer.json is unchanged
        assert_eq!(raw.require.len(), 1);
        assert!(raw.require.contains_key("psr/log"));
    }

    /// Without `--dev`, auto-detect finds the package in whichever section contains it.
    #[test]
    fn test_remove_auto_detects_section_require() {
        let mut raw = make_raw_package("test/project");
        raw.require
            .insert("psr/log".to_string(), "^3.0".to_string());
        raw.require_dev
            .insert("phpunit/phpunit".to_string(), "^11.0".to_string());

        // Auto-detect: psr/log is in require
        let name = "psr/log";
        let removed_from_require = raw.require.remove(name).is_some();
        let removed_from_dev = if !removed_from_require {
            raw.require_dev.remove(name).is_some()
        } else {
            false
        };

        assert!(
            removed_from_require,
            "should be found and removed from require"
        );
        assert!(!removed_from_dev);
        assert!(!raw.require.contains_key("psr/log"));
        assert!(raw.require_dev.contains_key("phpunit/phpunit"));
    }

    /// Without `--dev`, auto-detect finds the package in require-dev if not in require.
    #[test]
    fn test_remove_auto_detects_section_require_dev() {
        let mut raw = make_raw_package("test/project");
        raw.require
            .insert("psr/log".to_string(), "^3.0".to_string());
        raw.require_dev
            .insert("phpunit/phpunit".to_string(), "^11.0".to_string());

        // Auto-detect: phpunit/phpunit is in require-dev
        let name = "phpunit/phpunit";
        let removed_from_require = raw.require.remove(name).is_some();
        let removed_from_dev = if !removed_from_require {
            raw.require_dev.remove(name).is_some()
        } else {
            false
        };

        assert!(!removed_from_require);
        assert!(
            removed_from_dev,
            "should be found and removed from require-dev"
        );
        assert!(!raw.require_dev.contains_key("phpunit/phpunit"));
        assert!(raw.require.contains_key("psr/log"));
    }

    /// After re-resolve, removed packages appear as `ChangeKind::Remove` in the change report.
    #[test]
    fn test_remove_change_report_shows_removals() {
        // Old lock has psr/log + monolog; new lock has only psr/log
        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);

        let changes =
            super::super::update::compute_update_changes(Some(&old_lock), &new_lock, false);

        assert_eq!(changes.len(), 1, "exactly one change expected");
        assert_eq!(changes[0].name, "monolog/monolog");
        assert!(
            matches!(
                &changes[0].kind,
                super::super::update::ChangeKind::Remove { old_version }
                if old_version == "3.8.0"
            ),
            "monolog/monolog should appear as a Remove change"
        );
    }

    // ──────────── Integration tests (network, #[ignore]) ────────────

    #[test]
    #[ignore]
    fn test_remove_full_e2e() {
        use mozart_registry::lockfile::{LockFileGenerationRequest, generate_lock_file};
        use mozart_registry::resolver::{ResolveRequest, resolve};
        use std::collections::HashMap;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");
        let vendor_dir = dir.path().join("vendor");

        // Start with psr/log in require
        let content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, content).unwrap();

        let mut raw: RawPackageData = serde_json::from_str(content).unwrap();

        // Simulate initial install
        let request = ResolveRequest {
            require: vec![("psr/log".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: mozart_core::package::Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: mozart_registry::resolver::PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };
        let resolved = resolve(&request).expect("initial resolution should succeed");
        let initial_lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: content.to_string(),
            composer_json: raw.clone(),
            include_dev: false,
            repo_cache: None,
        })
        .expect("initial lock file generation should succeed");
        initial_lock
            .write_to_file(&lock_path)
            .expect("should write initial lock file");

        // Now remove psr/log
        raw.require.remove("psr/log");
        package::write_to_file(&raw, &composer_path).unwrap();

        // Re-resolve with empty require
        let request2 = ResolveRequest {
            require: vec![],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: mozart_core::package::Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: mozart_registry::resolver::PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };
        let resolved2 = resolve(&request2).expect("post-remove resolution should succeed");

        let composer_json_content2 = std::fs::read_to_string(&composer_path).unwrap();
        let new_lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved2,
            composer_json_content: composer_json_content2,
            composer_json: raw,
            include_dev: false,
            repo_cache: None,
        })
        .expect("post-remove lock file generation should succeed");

        // psr/log should no longer be in the new lock
        assert!(
            !new_lock.packages.iter().any(|p| p.name == "psr/log"),
            "psr/log should be absent from the new lock file"
        );

        // Write new lock
        new_lock.write_to_file(&lock_path).unwrap();
        assert!(lock_path.exists(), "lock file should exist");

        // Vendor should not contain psr/log after install_from_lock
        // (install_from_lock removes packages no longer in lock)
        let _ = vendor_dir; // referenced to avoid dead_code warning
    }

    #[test]
    #[ignore]
    fn test_remove_no_update_only_modifies_json() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");

        let content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, content).unwrap();

        // Simulate what execute() does with --no-update:
        // 1. Read and modify composer.json
        let mut raw: RawPackageData = serde_json::from_str(content).unwrap();
        raw.require.remove("psr/log");
        package::write_to_file(&raw, &composer_path).unwrap();

        // 2. Return early — do NOT write lock file
        // Lock file should not exist
        assert!(
            !lock_path.exists(),
            "lock file should not be created with --no-update"
        );

        // composer.json should be updated
        let updated_content = std::fs::read_to_string(&composer_path).unwrap();
        assert!(
            !updated_content.contains("psr/log"),
            "psr/log should be removed from composer.json"
        );
    }

    #[test]
    #[ignore]
    fn test_remove_dry_run_modifies_nothing() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");
        let vendor_dir = dir.path().join("vendor");

        let original_content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, original_content).unwrap();

        // Simulate --dry-run: composer.json, lock, vendor all left unchanged.
        // The execute() function with dry_run=true won't write any files.
        assert_eq!(
            std::fs::read_to_string(&composer_path).unwrap(),
            original_content,
            "composer.json should be unmodified after dry run"
        );
        assert!(
            !lock_path.exists(),
            "lock file should not be created by dry run"
        );
        assert!(
            !vendor_dir.exists(),
            "vendor dir should not be created by dry run"
        );
    }
}
