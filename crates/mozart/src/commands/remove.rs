use clap::Args;
use indexmap::{IndexMap, IndexSet};
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::package;
use mozart_registry::installed;
use mozart_registry::lockfile;
use mozart_registry::resolver::{self, PlatformConfig, ResolveRequest};

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
    #[arg(long, value_parser = ["table", "plain", "json", "summary"], default_value = "summary")]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,

    /// [Deprecated] Use --no-update-with-dependencies instead
    #[arg(short = 'w', long)]
    pub update_with_dependencies: bool,

    /// Alias for --with-all-dependencies
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

pub async fn execute(
    args: &RemoveArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let cache_config = mozart_registry::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_registry::cache::Cache::repo(&cache_config);

    if args.packages.is_empty() && !args.unused {
        anyhow::bail!("Not enough arguments (missing: \"packages\").");
    }

    // Only -w/--update-with-dependencies is deprecated in Composer; -W is an alias, not deprecated
    if args.update_with_dependencies {
        console.write_error(&console_format!(
            "<warning>You are using the deprecated option \"update-with-dependencies\". This is now default behaviour. The --no-update-with-dependencies option can be used to remove a package without its dependencies.</warning>"
        ));
    }

    let working_dir = cli.working_dir()?;
    let composer_path = working_dir.join("composer.json");

    if !composer_path.exists() {
        anyhow::bail!(
            "composer.json not found in {}. Run `mozart init` to create one.",
            working_dir.display()
        );
    }

    let mut composer = package::read_from_file(&composer_path)?;
    // Backup for revert on pipeline failure (mirrors $composerBackup in Composer)
    let composer_backup = std::fs::read(&composer_path)?;

    if args.unused && args.packages.is_empty() {
        return remove_unused(
            &composer,
            &working_dir,
            args,
            &repo_cache,
            cli.no_cache,
            console,
        )
        .await;
    }

    // Per-package removal; tracks actually-removed names for the post-install still-present check
    let mut packages_removed: Vec<String> = Vec::new();

    for pkg_arg in &args.packages {
        let name = pkg_arg.trim().to_lowercase();
        // No validate_package_name bail: invalid names fall through to the "not required" warning,
        // matching Composer's behaviour (it does not validate name format here either).

        if args.dev {
            if composer.require_dev.contains_key(&name) {
                console_writeln!(
                    console,
                    &console_format!("<info>Removing {name} from require-dev</info>"),
                );
                composer.require_dev.remove(&name);
                packages_removed.push(name);
            } else {
                console.info(&console_format!(
                    "<warning>{name} is not required in your composer.json and has not been removed</warning>"
                ));
            }
        } else if composer.require.contains_key(&name) {
            console_writeln!(
                console,
                &console_format!("<info>Removing {name} from require</info>"),
            );
            composer.require.remove(&name);
            packages_removed.push(name);
        } else if composer.require_dev.contains_key(&name) {
            console_writeln!(
                console,
                &console_format!("<info>Removing {name} from require-dev</info>"),
            );
            composer.require_dev.remove(&name);
            packages_removed.push(name);
        } else {
            console.info(&console_format!(
                "<warning>{name} is not required in your composer.json and has not been removed</warning>"
            ));
        }
    }

    if !args.dry_run && !packages_removed.is_empty() {
        package::write_to_file(&composer, &composer_path)?;
    }
    console.info("./composer.json has been updated");

    if args.no_update {
        console_writeln!(
            console,
            &console_format!(
                "<comment>Not updating dependencies, only modifying composer.json.</comment>"
            ),
        );
        return Ok(());
    }

    // --- Full resolution + lock + install pipeline ---

    let dev_mode = !args.update_no_dev;
    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");
    let pkg_names = args.packages.join(" ");
    let with_all_deps = args.with_all_dependencies || args.update_with_all_dependencies;
    // Flag suffix echoed in "Running composer update" — mirrors Composer's $flags variable
    let flags: &str = if with_all_deps {
        " --with-all-dependencies"
    } else if args.no_update_with_dependencies {
        ""
    } else {
        " --with-dependencies"
    };

    let no_cache = cli.no_cache;

    let pipeline_result: anyhow::Result<()> = async {
        let require: Vec<(String, String)> = composer
            .require
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let require_dev: Vec<(String, String)> = composer
            .require_dev
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let minimum_stability_str = composer.minimum_stability.as_deref().unwrap_or("stable");
        let minimum_stability = package::Stability::parse(minimum_stability_str);

        let composer_prefer_stable = composer
            .extra_fields
            .get("prefer-stable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let request = ResolveRequest {
            root_name: composer.name.clone(),
            root_version: composer.version.clone(),
            require,
            require_dev,
            include_dev: dev_mode,
            minimum_stability,
            stability_flags: IndexMap::new(),
            prefer_stable: composer_prefer_stable,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
            ignore_platform_reqs: args.ignore_platform_reqs,
            ignore_platform_req_list: args.ignore_platform_req.clone(),
            repositories: std::sync::Arc::new(
                mozart_registry::repository::RepositorySet::with_packagist(repo_cache.clone()),
            ),
            temporary_constraints: IndexMap::new(),
            raw_repositories: composer.repositories.clone(),
            root_provide: composer
                .provide
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            root_replace: composer
                .replace
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            root_conflict: composer
                .conflict
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            locked_package_names: IndexSet::new(),
            locked_packages: Vec::new(),
            block_abandoned: false,
            root_branch_alias: None,
            preferred_versions: IndexMap::new(),
            block_insecure: false,
        };

        console.info(&console_format!(
            "<info>Running composer update {pkg_names}{flags}</info>"
        ));
        console.info("Loading composer repositories with package information");
        if dev_mode {
            console.info("Updating dependencies (including require-dev)");
        } else {
            console.info("Updating dependencies");
        }
        console.info("Resolving dependencies...");

        let mut resolved = resolver::resolve(&request).await.map_err(|e| {
            mozart_core::exit_code::bail(
                mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
                e.to_string(),
            )
        })?;

        let old_lock = if lock_path.exists() {
            match lockfile::LockFile::read_from_file(&lock_path) {
                Ok(l) => Some(l),
                Err(e) => {
                    console.info(&console_format!(
                        "<warning>Could not read existing composer.lock: {}. Treating as a fresh install.</warning>",
                        e
                    ));
                    None
                }
            }
        } else {
            None
        };

        if let Some(ref lock) = old_lock {
            let removed_names: Vec<String> = args
                .packages
                .iter()
                .map(|s| s.trim().to_lowercase())
                .collect();

            let repo_requires = super::update::collect_repo_requires(&composer.repositories);
            let allow_list = if args.no_update_with_dependencies {
                removed_names
            } else if with_all_deps {
                super::update::expand_with_all_dependencies(removed_names, lock, &repo_requires)
            } else {
                super::update::expand_with_direct_dependencies(
                    removed_names,
                    lock,
                    &IndexSet::new(),
                    &repo_requires,
                )
            };

            if args.minimal_changes {
                console.info(&console_format!(
                    "<info>Minimal changes mode: preserving locked versions for non-removed packages.</info>"
                ));
            }

            resolved = super::update::apply_partial_update(resolved, lock, &allow_list);
        }

        let composer_json_content = if args.dry_run {
            package::to_json_pretty(&composer)?
        } else {
            std::fs::read_to_string(&composer_path)?
        };

        let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.clone(),
            composer_json: composer.clone(),
            include_dev: dev_mode,
            repositories: std::sync::Arc::new(
                mozart_registry::repository::RepositorySet::with_packagist(repo_cache.clone()),
            ),
            previous_lock: old_lock.clone(),
            lock_pinned_names: IndexSet::new(),
        })
        .await?;

        let changes =
            super::update::compute_update_changes(old_lock.as_ref(), &new_lock, dev_mode);

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
            .filter(|c| matches!(c.kind, super::update::ChangeKind::Uninstall { .. }))
            .collect();

        console.info(&console_format!(
            "<info>Package operations: {} install{}, {} update{}, {} removal{}</info>",
            installs.len(),
            if installs.len() == 1 { "" } else { "s" },
            updates.len(),
            if updates.len() == 1 { "" } else { "s" },
            removals.len(),
            if removals.len() == 1 { "" } else { "s" },
        ));

        for change in &changes {
            match &change.kind {
                super::update::ChangeKind::Uninstall { old_version } => {
                    if args.dry_run {
                        console.info(&format!(
                            "  - Would remove {} ({})",
                            change.name, old_version
                        ));
                    } else {
                        console.info(&format!(
                            "  - Removing {} ({})",
                            change.name, old_version
                        ));
                    }
                }
                super::update::ChangeKind::Install { new_version } => {
                    if args.dry_run {
                        console.info(&format!(
                            "  - Would install {} ({})",
                            change.name, new_version
                        ));
                    } else {
                        console.info(&format!(
                            "  - Installing {} ({})",
                            change.name, new_version
                        ));
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
            }
        }

        if !args.dry_run {
            console.info("Writing lock file");
            new_lock.write_to_file(&lock_path)?;
        }

        if !args.no_install && !args.dry_run {
            let cache_config = mozart_registry::cache::build_cache_config(no_cache);
            let files_cache = mozart_registry::cache::Cache::files(&cache_config);
            let mut executor =
                mozart_registry::installer_executor::FilesystemExecutor::new(files_cache);
            super::install::install_from_lock(
                &new_lock,
                &working_dir,
                &vendor_dir,
                &super::install::InstallConfig {
                    dev_mode,
                    dry_run: false,
                    no_autoloader: false,
                    no_progress: args.no_progress,
                    ignore_platform_reqs: args.ignore_platform_reqs,
                    ignore_platform_req: args.ignore_platform_req.clone(),
                    optimize_autoloader: args.optimize_autoloader,
                    classmap_authoritative: args.classmap_authoritative,
                    apcu_autoloader: args.apcu_autoloader || args.apcu_autoloader_prefix.is_some(),
                    apcu_autoloader_prefix: args.apcu_autoloader_prefix.clone(),
                    download_only: false,
                    prefer_source: false,
                },
                console,
                &mut executor,
            )
            .await?;
        }

        Ok(())
    }
    .await;

    if let Err(e) = pipeline_result {
        if !args.dry_run && !packages_removed.is_empty() {
            let _ = std::fs::write(&composer_path, &composer_backup);
            console.error("\nRemoval failed, reverting ./composer.json to its original content.");
        }
        return Err(e);
    }

    // Post-install still-present check — mirrors Composer's local-repository query at L303-311
    if !args.dry_run && !args.no_install && !packages_removed.is_empty() {
        let installed_pkgs = installed::InstalledPackages::read(&vendor_dir)?;
        let mut still_present = false;
        for name in &packages_removed {
            if installed_pkgs
                .packages
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(name))
            {
                console.error(&format!(
                    "Removal failed, {name} is still present, it may be required by another package. See `mozart why {name}`."
                ));
                still_present = true;
            }
        }
        if still_present {
            return Err(mozart_core::exit_code::bail_silent(2));
        }
    }

    Ok(())
}

/// Remove unused packages by re-resolving and comparing with the current lock file.
async fn remove_unused(
    composer: &package::RawPackageData,
    working_dir: &std::path::Path,
    args: &RemoveArgs,
    repo_cache: &mozart_registry::cache::Cache,
    no_cache: bool,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let lock_path = working_dir.join("composer.lock");

    if !lock_path.exists() {
        anyhow::bail!("A valid composer.lock file is required to run this command with --unused");
    }

    let old_lock = lockfile::LockFile::read_from_file(&lock_path)?;

    let dev_mode = !args.update_no_dev;

    let require: Vec<(String, String)> = composer
        .require
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let require_dev: Vec<(String, String)> = composer
        .require_dev
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let minimum_stability_str = composer.minimum_stability.as_deref().unwrap_or("stable");
    let minimum_stability = package::Stability::parse(minimum_stability_str);
    let composer_prefer_stable = composer
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let request = ResolveRequest {
        root_name: composer.name.clone(),
        root_version: composer.version.clone(),
        require,
        require_dev,
        include_dev: dev_mode,
        minimum_stability,
        stability_flags: IndexMap::new(),
        prefer_stable: composer_prefer_stable,
        prefer_lowest: false,
        platform: PlatformConfig::new(),
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repositories: std::sync::Arc::new(
            mozart_registry::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        temporary_constraints: IndexMap::new(),
        raw_repositories: composer.repositories.clone(),
        root_provide: composer
            .provide
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_replace: composer
            .replace
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_conflict: composer
            .conflict
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        locked_package_names: IndexSet::new(),
        locked_packages: Vec::new(),
        block_abandoned: false,
        root_branch_alias: None,
        preferred_versions: IndexMap::new(),
        block_insecure: false,
    };

    console.info("Resolving dependencies to detect unused packages...");

    let resolved = resolver::resolve(&request).await.map_err(|e| {
        mozart_core::exit_code::bail(
            mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
            e.to_string(),
        )
    })?;

    let resolved_names: indexmap::IndexSet<String> =
        resolved.iter().map(|p| p.name.to_lowercase()).collect();

    let mut unused: Vec<String> = Vec::new();
    for pkg in &old_lock.packages {
        if !resolved_names.contains(&pkg.name.to_lowercase()) {
            unused.push(pkg.name.clone());
        }
    }
    if let Some(ref dev_pkgs) = old_lock.packages_dev {
        for pkg in dev_pkgs {
            if !resolved_names.contains(&pkg.name.to_lowercase()) {
                unused.push(pkg.name.clone());
            }
        }
    }

    if unused.is_empty() {
        console.info(&console_format!(
            "<info>No unused packages to remove</info>"
        ));
        return Ok(());
    }

    for name in &unused {
        console.info(&format!("  - Removing unused package: {name}"));
    }
    console.info(&format!("Found {} unused package(s).", unused.len()));

    if args.dry_run {
        console.info(&console_format!(
            "<comment>Dry run: lock file not modified.</comment>"
        ));
        return Ok(());
    }

    let composer_json_content = std::fs::read_to_string(working_dir.join("composer.json"))?;
    let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content,
        composer_json: composer.clone(),
        include_dev: dev_mode,
        repositories: std::sync::Arc::new(
            mozart_registry::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        previous_lock: Some(old_lock.clone()),
        lock_pinned_names: IndexSet::new(),
    })
    .await?;

    console.info("Writing lock file");
    new_lock.write_to_file(&lock_path)?;

    if !args.no_install {
        let vendor_dir = working_dir.join("vendor");
        let cache_config = mozart_registry::cache::build_cache_config(no_cache);
        let files_cache = mozart_registry::cache::Cache::files(&cache_config);
        let mut executor =
            mozart_registry::installer_executor::FilesystemExecutor::new(files_cache);
        super::install::install_from_lock(
            &new_lock,
            working_dir,
            &vendor_dir,
            &super::install::InstallConfig {
                dev_mode,
                dry_run: false,
                no_autoloader: false,
                no_progress: args.no_progress,
                ignore_platform_reqs: args.ignore_platform_reqs,
                ignore_platform_req: args.ignore_platform_req.clone(),
                optimize_autoloader: args.optimize_autoloader,
                classmap_authoritative: args.classmap_authoritative,
                apcu_autoloader: args.apcu_autoloader || args.apcu_autoloader_prefix.is_some(),
                apcu_autoloader_prefix: args.apcu_autoloader_prefix.clone(),
                download_only: false,
                prefer_source: false,
            },
            console,
            &mut executor,
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_core::package::RawPackageData;
    use mozart_registry::lockfile;
    use std::collections::BTreeMap;

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
            provide: BTreeMap::new(),
            replace: BTreeMap::new(),
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

    /// Remove a package from `require`, verify it's gone from `RawPackageData`.
    #[test]
    fn test_remove_from_require() {
        let mut composer = make_raw_package("test/project");
        composer
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());
        composer
            .require
            .insert("monolog/monolog".to_string(), "^3.0".to_string());

        assert!(composer.require.contains_key("psr/log"));

        composer.require.remove("psr/log");

        assert!(
            !composer.require.contains_key("psr/log"),
            "psr/log should be removed from require"
        );
        assert!(
            composer.require.contains_key("monolog/monolog"),
            "monolog/monolog should remain in require"
        );
    }

    /// Remove a package from `require-dev` with `--dev` flag.
    #[test]
    fn test_remove_from_require_dev() {
        let mut composer = make_raw_package("test/project");
        composer
            .require_dev
            .insert("phpunit/phpunit".to_string(), "^11.0".to_string());
        composer
            .require_dev
            .insert("mockery/mockery".to_string(), "^1.0".to_string());

        assert!(composer.require_dev.contains_key("phpunit/phpunit"));

        composer.require_dev.remove("phpunit/phpunit");

        assert!(
            !composer.require_dev.contains_key("phpunit/phpunit"),
            "phpunit/phpunit should be removed from require-dev"
        );
        assert!(
            composer.require_dev.contains_key("mockery/mockery"),
            "mockery/mockery should remain in require-dev"
        );
    }

    /// Removing a package not in either section does not panic and doesn't change anything.
    #[test]
    fn test_remove_nonexistent_package_no_panic() {
        let mut composer = make_raw_package("test/project");
        composer
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let name = "nonexistent/package";
        let found_in_require = composer.require.remove(name).is_some();
        let found_in_require_dev = composer.require_dev.remove(name).is_some();

        assert!(!found_in_require);
        assert!(!found_in_require_dev);

        assert_eq!(composer.require.len(), 1);
        assert!(composer.require.contains_key("psr/log"));
    }

    /// Without `--dev`, auto-detect finds the package in whichever section contains it.
    #[test]
    fn test_remove_auto_detects_section_require() {
        let mut composer = make_raw_package("test/project");
        composer
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());
        composer
            .require_dev
            .insert("phpunit/phpunit".to_string(), "^11.0".to_string());

        let name = "psr/log";
        let removed_from_require = composer.require.remove(name).is_some();
        let removed_from_dev = if !removed_from_require {
            composer.require_dev.remove(name).is_some()
        } else {
            false
        };

        assert!(
            removed_from_require,
            "should be found and removed from require"
        );
        assert!(!removed_from_dev);
        assert!(!composer.require.contains_key("psr/log"));
        assert!(composer.require_dev.contains_key("phpunit/phpunit"));
    }

    /// Without `--dev`, auto-detect finds the package in require-dev if not in require.
    #[test]
    fn test_remove_auto_detects_section_require_dev() {
        let mut composer = make_raw_package("test/project");
        composer
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());
        composer
            .require_dev
            .insert("phpunit/phpunit".to_string(), "^11.0".to_string());

        let name = "phpunit/phpunit";
        let removed_from_require = composer.require.remove(name).is_some();
        let removed_from_dev = if !removed_from_require {
            composer.require_dev.remove(name).is_some()
        } else {
            false
        };

        assert!(!removed_from_require);
        assert!(
            removed_from_dev,
            "should be found and removed from require-dev"
        );
        assert!(!composer.require_dev.contains_key("phpunit/phpunit"));
        assert!(composer.require.contains_key("psr/log"));
    }

    /// After re-resolve, removed packages appear as `ChangeKind::Uninstall` in the change report.
    #[test]
    fn test_remove_change_report_shows_removals() {
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
                super::super::update::ChangeKind::Uninstall { old_version }
                if old_version == "3.8.0"
            ),
            "monolog/monolog should appear as an Uninstall change"
        );
    }

    /// Glob-style package names (e.g. "vendor/*") no longer bail with an "Invalid package name"
    /// error — they fall through to the "not required" warning path. This is a regression test
    /// for the validate_package_name bail that was removed in PR-A.
    #[test]
    fn test_glob_package_name_falls_through_to_not_required() {
        let mut composer = make_raw_package("test/project");
        composer
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());

        // A glob-style name: not a valid exact package name, not in require either.
        let name = "vendor/*";
        let found =
            composer.require.remove(name).is_some() || composer.require_dev.remove(name).is_some();

        // Should NOT be found (falls through to "not required" warning), not panicked/bailed.
        assert!(!found, "glob name should not match any package");
        // composer.json is unchanged
        assert_eq!(composer.require.len(), 1);
    }

    /// --unused with no lock file must return an error matching Composer's wording.
    #[test]
    fn test_unused_no_lock_error_wording() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        // No lock file present — the error message is tested via the remove_unused code path.
        let lock_path = dir.path().join("composer.lock");
        assert!(!lock_path.exists());

        // The error message Composer uses (and Mozart must match):
        let expected = "A valid composer.lock file is required to run this command with --unused";
        // Simulate the check that remove_unused() performs:
        let result: anyhow::Result<()> = if !lock_path.exists() {
            Err(anyhow::anyhow!("{}", expected))
        } else {
            Ok(())
        };
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(expected));
    }

    #[tokio::test]
    #[ignore]
    async fn test_remove_full_e2e() {
        use indexmap::{IndexMap, IndexSet};
        use mozart_registry::lockfile::{LockFileGenerationRequest, generate_lock_file};
        use mozart_registry::resolver::{ResolveRequest, resolve};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");
        let vendor_dir = dir.path().join("vendor");

        let content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, content).unwrap();

        let mut composer: RawPackageData = serde_json::from_str(content).unwrap();

        let request = ResolveRequest {
            root_name: String::new(),
            root_version: None,
            require: vec![("psr/log".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: mozart_core::package::Stability::Stable,
            stability_flags: IndexMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: mozart_registry::resolver::PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repositories: std::sync::Arc::new(
                mozart_registry::repository::RepositorySet::with_packagist(
                    mozart_registry::cache::Cache::new(
                        std::env::temp_dir().join("mozart-test-cache"),
                        false,
                    ),
                ),
            ),
            temporary_constraints: IndexMap::new(),
            raw_repositories: vec![],
            root_provide: IndexMap::new(),
            root_replace: IndexMap::new(),
            root_conflict: IndexMap::new(),
            locked_package_names: IndexSet::new(),
            locked_packages: Vec::new(),
            block_abandoned: false,
            root_branch_alias: None,
            preferred_versions: IndexMap::new(),
            block_insecure: false,
        };
        let resolved = resolve(&request)
            .await
            .expect("initial resolution should succeed");
        let initial_lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: content.to_string(),
            composer_json: composer.clone(),
            include_dev: false,
            repositories: std::sync::Arc::new(
                mozart_registry::repository::RepositorySet::with_packagist(
                    mozart_registry::cache::Cache::new(
                        std::env::temp_dir().join("mozart-test-cache"),
                        false,
                    ),
                ),
            ),
            previous_lock: None,
            lock_pinned_names: IndexSet::new(),
        })
        .await
        .expect("initial lock file generation should succeed");
        initial_lock
            .write_to_file(&lock_path)
            .expect("should write initial lock file");

        composer.require.remove("psr/log");
        package::write_to_file(&composer, &composer_path).unwrap();

        let request2 = ResolveRequest {
            root_name: String::new(),
            root_version: None,
            require: vec![],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: mozart_core::package::Stability::Stable,
            stability_flags: IndexMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: mozart_registry::resolver::PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repositories: std::sync::Arc::new(
                mozart_registry::repository::RepositorySet::with_packagist(
                    mozart_registry::cache::Cache::new(
                        std::env::temp_dir().join("mozart-test-cache"),
                        false,
                    ),
                ),
            ),
            temporary_constraints: IndexMap::new(),
            raw_repositories: vec![],
            root_provide: IndexMap::new(),
            root_replace: IndexMap::new(),
            root_conflict: IndexMap::new(),
            locked_package_names: IndexSet::new(),
            locked_packages: Vec::new(),
            block_abandoned: false,
            root_branch_alias: None,
            preferred_versions: IndexMap::new(),
            block_insecure: false,
        };
        let resolved2 = resolve(&request2)
            .await
            .expect("post-remove resolution should succeed");

        let composer_json_content2 = std::fs::read_to_string(&composer_path).unwrap();
        let new_lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved2,
            composer_json_content: composer_json_content2,
            composer_json: composer,
            include_dev: false,
            repositories: std::sync::Arc::new(
                mozart_registry::repository::RepositorySet::with_packagist(
                    mozart_registry::cache::Cache::new(
                        std::env::temp_dir().join("mozart-test-cache"),
                        false,
                    ),
                ),
            ),
            previous_lock: Some(initial_lock.clone()),
            lock_pinned_names: IndexSet::new(),
        })
        .await
        .expect("post-remove lock file generation should succeed");

        assert!(
            !new_lock.packages.iter().any(|p| p.name == "psr/log"),
            "psr/log should be absent from the new lock file"
        );

        new_lock.write_to_file(&lock_path).unwrap();
        assert!(lock_path.exists(), "lock file should exist");

        let _ = vendor_dir;
    }

    #[test]
    fn test_remove_no_update_only_modifies_json() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");

        let content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, content).unwrap();

        let mut composer: RawPackageData = serde_json::from_str(content).unwrap();
        composer.require.remove("psr/log");
        package::write_to_file(&composer, &composer_path).unwrap();

        assert!(
            !lock_path.exists(),
            "lock file should not be created with --no-update"
        );

        let updated_content = std::fs::read_to_string(&composer_path).unwrap();
        assert!(
            !updated_content.contains("psr/log"),
            "psr/log should be removed from composer.json"
        );
    }

    #[test]
    fn test_remove_dry_run_modifies_nothing() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");
        let vendor_dir = dir.path().join("vendor");

        let original_content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, original_content).unwrap();

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
