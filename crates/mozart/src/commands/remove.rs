use clap::Args;
use indexmap::{IndexMap, IndexSet};
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::package;
use mozart_core::repository::installed;
use mozart_core::repository::lockfile;
use mozart_core::repository::resolver::{self, PlatformConfig, ResolveRequest};

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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_core::repository::cache::Cache::repo(&cache_config);

    if args.packages.is_empty() && !args.unused {
        anyhow::bail!("Not enough arguments (missing: \"packages\").");
    }

    // Only -w/--update-with-dependencies is deprecated in Composer; -W is an alias, not deprecated
    if args.update_with_dependencies {
        io.lock().unwrap().write_error(&console_format!(
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
            &io,
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
                console_writeln!(io, "<info>Removing {name} from require-dev</info>");
                composer.require_dev.shift_remove(&name);
                packages_removed.push(name);
            } else {
                io.lock().unwrap().info(&console_format!(
                    "<warning>{name} is not required in your composer.json and has not been removed</warning>"
                ));
            }
        } else if composer.require.contains_key(&name) {
            console_writeln!(io, "<info>Removing {name} from require</info>");
            composer.require.shift_remove(&name);
            packages_removed.push(name);
        } else if composer.require_dev.contains_key(&name) {
            console_writeln!(io, "<info>Removing {name} from require-dev</info>");
            composer.require_dev.shift_remove(&name);
            packages_removed.push(name);
        } else {
            io.lock().unwrap().info(&console_format!(
                "<warning>{name} is not required in your composer.json and has not been removed</warning>"
            ));
        }
    }

    if !args.dry_run && !packages_removed.is_empty() {
        package::write_to_file(&composer, &composer_path)?;
    }
    io.lock().unwrap().info("./composer.json has been updated");

    if args.no_update {
        console_writeln!(
            io,
            "<comment>Not updating dependencies, only modifying composer.json.</comment>"
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
                mozart_core::repository::repository::RepositorySet::with_packagist(repo_cache.clone()),
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

        io.lock().unwrap().info(&console_format!(
            "<info>Running composer update {pkg_names}{flags}</info>"
        ));
        io.lock().unwrap().info("Loading composer repositories with package information");
        if dev_mode {
            io.lock().unwrap().info("Updating dependencies (including require-dev)");
        } else {
            io.lock().unwrap().info("Updating dependencies");
        }
        io.lock().unwrap().info("Resolving dependencies...");

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
                    io.lock().unwrap().info(&console_format!(
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
                io.lock().unwrap().info(&console_format!(
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
                mozart_core::repository::repository::RepositorySet::with_packagist(repo_cache.clone()),
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

        io.lock().unwrap().info(&console_format!(
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
                        io.lock().unwrap().info(&format!(
                            "  - Would remove {} ({})",
                            change.name, old_version
                        ));
                    } else {
                        io.lock().unwrap().info(&format!(
                            "  - Removing {} ({})",
                            change.name, old_version
                        ));
                    }
                }
                super::update::ChangeKind::Install { new_version } => {
                    if args.dry_run {
                        io.lock().unwrap().info(&format!(
                            "  - Would install {} ({})",
                            change.name, new_version
                        ));
                    } else {
                        io.lock().unwrap().info(&format!(
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
                        io.lock().unwrap().info(&format!(
                            "  - Would update {} ({} => {})",
                            change.name, old_version, new_version
                        ));
                    } else {
                        io.lock().unwrap().info(&format!(
                            "  - Updating {} ({} => {})",
                            change.name, old_version, new_version
                        ));
                    }
                }
            }
        }

        if !args.dry_run {
            io.lock().unwrap().info("Writing lock file");
            new_lock.write_to_file(&lock_path)?;
        }

        if !args.no_install && !args.dry_run {
            let cache_config = mozart_core::repository::cache::build_cache_config(no_cache);
            let files_cache = mozart_core::repository::cache::Cache::files(&cache_config);
            let mut executor =
                mozart_core::repository::installer_executor::FilesystemExecutor::new(files_cache);
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
                io.clone(),
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
            io.lock()
                .unwrap()
                .error("\nRemoval failed, reverting ./composer.json to its original content.");
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
                io.lock().unwrap().error(&format!(
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
    repo_cache: &mozart_core::repository::cache::Cache,
    no_cache: bool,
    io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
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
            mozart_core::repository::repository::RepositorySet::with_packagist(repo_cache.clone()),
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

    io.lock()
        .unwrap()
        .info("Resolving dependencies to detect unused packages...");

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
        io.lock().unwrap().info(&console_format!(
            "<info>No unused packages to remove</info>"
        ));
        return Ok(());
    }

    for name in &unused {
        io.lock()
            .unwrap()
            .info(&format!("  - Removing unused package: {name}"));
    }
    io.lock()
        .unwrap()
        .info(&format!("Found {} unused package(s).", unused.len()));

    if args.dry_run {
        io.lock().unwrap().info(&console_format!(
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
            mozart_core::repository::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        previous_lock: Some(old_lock.clone()),
        lock_pinned_names: IndexSet::new(),
    })
    .await?;

    io.lock().unwrap().info("Writing lock file");
    new_lock.write_to_file(&lock_path)?;

    if !args.no_install {
        let vendor_dir = working_dir.join("vendor");
        let cache_config = mozart_core::repository::cache::build_cache_config(no_cache);
        let files_cache = mozart_core::repository::cache::Cache::files(&cache_config);
        let mut executor =
            mozart_core::repository::installer_executor::FilesystemExecutor::new(files_cache);
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
            io.clone(),
            &mut executor,
        )
        .await?;
    }

    Ok(())
}
