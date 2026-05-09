use clap::Args;
use indexmap::{IndexMap, IndexSet};
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::package::{self, RawPackageData, Stability};
use mozart_core::validation;
use mozart_registry::lockfile;
use mozart_registry::packagist;
use mozart_registry::resolver::{self, PlatformConfig, ResolveRequest};
use mozart_registry::version;
use mozart_registry::version_selector::VersionSelector;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct RequireArgs {
    /// Package(s) to require
    pub packages: Vec<String>,

    /// Add requirement to require-dev
    #[arg(long)]
    pub dev: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long)]
    pub prefer_install: Option<String>,

    /// Pin the exact version instead of a range
    #[arg(long)]
    pub fixed: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

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
    #[arg(long, value_parser = ["table", "plain", "json", "summary"])]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,

    /// [Deprecated] Use --with-dependencies instead
    #[arg(short = 'w', long)]
    pub update_with_dependencies: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'W', long)]
    pub update_with_all_dependencies: bool,

    /// Update also dependencies of newly required packages
    #[arg(long)]
    pub with_dependencies: bool,

    /// Update all dependencies including root requirements
    #[arg(long)]
    pub with_all_dependencies: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,

    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Sort packages in composer.json
    #[arg(long)]
    pub sort_packages: bool,

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

/// Per-execution mutable state.
/// Mirrors Composer\Command\RequireCommand instance properties.
struct CommandState {
    newly_created: bool,
    first_require: bool,
    json_path: PathBuf,
    lock_path: PathBuf,
    composer_backup: String,
    lock_backup: Option<String>,
    dependency_resolution_completed: bool,
}

/// Reverts composer.json (and composer.lock) to their pre-command state.
/// Mirrors Composer\Command\RequireCommand::revertComposerFile().
fn revert_composer_file(state: &CommandState, console: &mozart_core::console::Console) {
    if state.newly_created {
        console.write_error(&format!(
            "\nInstallation failed, deleting {}.",
            state.json_path.display()
        ));
        if let Err(e) = std::fs::remove_file(&state.json_path) {
            console.write_error(&format!(
                "Warning: Failed to delete {}: {e}",
                state.json_path.display()
            ));
        }
        // Also remove any lock file that was created during this (failed) run
        if state.lock_path.exists()
            && let Err(e) = std::fs::remove_file(&state.lock_path)
        {
            console.write_error(&format!(
                "Warning: Failed to delete {}: {e}",
                state.lock_path.display()
            ));
        }
    } else {
        let msg = if state.lock_backup.is_some() {
            format!(" and {} to their", state.lock_path.display())
        } else {
            " to its".to_string()
        };
        console.write_error(&format!(
            "\nInstallation failed, reverting {}{msg} original content.",
            state.json_path.display()
        ));
        if let Err(e) = std::fs::write(&state.json_path, &state.composer_backup) {
            console.write_error(&format!(
                "Warning: Failed to revert {}: {e}",
                state.json_path.display()
            ));
        }
        if let Some(ref lock_content) = state.lock_backup
            && let Err(e) = std::fs::write(&state.lock_path, lock_content)
        {
            console.write_error(&format!(
                "Warning: Failed to revert {}: {e}",
                state.lock_path.display()
            ));
        }
    }
}

/// Returns the names of packages that are being added to `require_key` but already
/// live in the opposite section.
/// Mirrors Composer\Command\RequireCommand::getInconsistentRequireKeys().
fn get_inconsistent_require_keys(
    new_packages: &[String],
    require_key: &str,
    packages_by_key: &IndexMap<String, String>,
) -> Vec<String> {
    new_packages
        .iter()
        .filter(|name| {
            packages_by_key
                .get(name.as_str())
                .map(|k| k != require_key)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Returns a map of `package_name → "require" | "require-dev"` for all existing packages.
/// Mirrors Composer\Command\RequireCommand::getPackagesByRequireKey().
fn get_packages_by_require_key(raw: &RawPackageData) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    for name in raw.require.keys() {
        map.insert(name.clone(), "require".to_string());
    }
    for name in raw.require_dev.keys() {
        map.insert(name.clone(), "require-dev".to_string());
    }
    map
}

/// Formatting-preserving composer.json write (stub — returns `Ok(false)` to trigger fallback).
/// Mirrors Composer\Command\RequireCommand::updateFileCleanly().
/// Will be implemented in PR 3 when JsonManipulator is ported.
fn update_file_cleanly(_json_path: &Path, _raw: &RawPackageData) -> anyhow::Result<bool> {
    Ok(false)
}

/// Write the updated requirements to composer.json.
/// Tries the formatting-preserving path first; falls back to a full rewrite.
/// Mirrors Composer\Command\RequireCommand::updateFile().
fn update_file(json_path: &Path, raw: &RawPackageData) -> anyhow::Result<()> {
    if update_file_cleanly(json_path, raw)? {
        return Ok(());
    }
    package::write_to_file(raw, json_path)
}

/// Post-resolution constraint rewrite for `'guess'` placeholders (stub for PR 2).
/// Mirrors Composer\Command\RequireCommand::updateRequirementsAfterResolution().
#[allow(clippy::too_many_arguments)]
async fn update_requirements_after_resolution(
    _state: &CommandState,
    _requirements_to_update: &[String],
    _require_key: &str,
    _remove_key: &str,
    _sort_packages: bool,
    _dry_run: bool,
    _fixed: bool,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    Ok(())
}

/// Resolve + lock + install pipeline.
/// Mirrors Composer\Command\RequireCommand::doUpdate().
async fn do_update(
    state: &mut CommandState,
    args: &RequireArgs,
    cli: &super::Cli,
    raw: &RawPackageData,
    additions: &[(String, String, bool)],
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let vendor_dir = working_dir.join("vendor");
    let cache_config = mozart_registry::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_registry::cache::Cache::repo(&cache_config);

    let dev_mode = !args.update_no_dev;

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

    let minimum_stability =
        package::Stability::parse(raw.minimum_stability.as_deref().unwrap_or("stable"));

    let prefer_stable = args.prefer_stable
        || raw
            .extra_fields
            .get("prefer-stable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    // Audit: wire --no-security-blocking + COMPOSER_NO_SECURITY_BLOCKING env var.
    // Mirrors BaseCommand::createAuditConfig() + Installer::setAuditConfig().
    let no_security_blocking = args.no_security_blocking
        || std::env::var("COMPOSER_NO_SECURITY_BLOCKING")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);
    let no_audit = args.no_audit
        || std::env::var("COMPOSER_NO_AUDIT")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);
    let block_insecure = !no_audit && !no_security_blocking;

    let request = ResolveRequest {
        root_name: raw.name.clone(),
        root_version: raw.version.clone(),
        require,
        require_dev,
        include_dev: dev_mode,
        minimum_stability,
        stability_flags: IndexMap::new(),
        prefer_stable,
        prefer_lowest: args.prefer_lowest,
        platform: PlatformConfig::new(),
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repositories: std::sync::Arc::new(
            mozart_registry::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        temporary_constraints: IndexMap::new(),
        raw_repositories: raw.repositories.clone(),
        root_provide: raw
            .provide
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_replace: raw
            .replace
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_conflict: raw
            .conflict
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        locked_package_names: IndexSet::new(),
        locked_packages: Vec::new(),
        block_abandoned: false,
        root_branch_alias: None,
        preferred_versions: IndexMap::new(),
        block_insecure,
    };

    console.info("Loading composer repositories with package information");
    if dev_mode {
        console.info("Updating dependencies (including require-dev)");
    } else {
        console.info("Updating dependencies");
    }
    console.info("Resolving dependencies...");

    let mut resolved = match resolver::resolve(&request).await {
        Ok(packages) => packages,
        Err(e) => {
            if !args.dry_run {
                revert_composer_file(state, console);
            }
            // Suggest explicit version constraint retry for the first package without one.
            // Mirrors Composer\Command\RequireCommand::doUpdate() L496-502.
            let first_unversioned = additions
                .iter()
                .find(|(_, constraint, _)| {
                    !constraint.contains(['^', '~', '>', '<', '!', '=', '*'])
                })
                .map(|(name, _, _)| name.as_str());
            let hint = if let Some(name) = first_unversioned {
                format!(
                    "\n\nYou can also try re-running mozart require with an explicit version \
                     constraint, e.g. \"mozart require {name}:*\" to figure out if any version \
                     is installable, or \"mozart require {name}:^2.1\" if you know which you need."
                )
            } else {
                String::new()
            };
            return Err(mozart_core::exit_code::bail(
                mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
                format!("{e}{hint}"),
            ));
        }
    };

    state.dependency_resolution_completed = true;

    // Read old lock file for change reporting and partial update pinning.
    let old_lock = if state.lock_path.exists() {
        match lockfile::LockFile::read_from_file(&state.lock_path) {
            Ok(l) => Some(l),
            Err(e) => {
                console.info(&console_format!(
                    "<warning>Could not read existing composer.lock: {e}. \
                     Treating as a fresh install.</warning>"
                ));
                None
            }
        }
    } else {
        None
    };

    // Apply setUpdateAllowList only when NOT firstRequire and lock exists.
    // Mirrors Composer\Command\RequireCommand::doUpdate() L490-492:
    //   if (!$this->firstRequire && $composer->getLocker()->isLocked())
    //       $install->setUpdateAllowList(array_keys($requirements));
    if !state.first_require
        && let Some(ref lock) = old_lock
    {
        let with_deps = args.with_dependencies || args.update_with_dependencies;
        let with_all_deps = args.with_all_dependencies || args.update_with_all_dependencies;
        let newly_required: Vec<String> =
            additions.iter().map(|(name, _, _)| name.clone()).collect();
        let repo_requires = super::update::collect_repo_requires(&raw.repositories);
        let allow_list = if with_all_deps {
            super::update::expand_with_all_dependencies(newly_required, lock, &repo_requires)
        } else if with_deps {
            super::update::expand_with_direct_dependencies(
                newly_required,
                lock,
                &IndexSet::new(),
                &repo_requires,
            )
        } else {
            additions.iter().map(|(name, _, _)| name.clone()).collect()
        };
        resolved = super::update::apply_partial_update(resolved, lock, &allow_list);
    }

    let composer_json_content = if args.dry_run {
        package::to_json_pretty(raw)?
    } else {
        std::fs::read_to_string(&state.json_path)?
    };

    let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content: composer_json_content.clone(),
        composer_json: raw.clone(),
        include_dev: dev_mode,
        repositories: std::sync::Arc::new(
            mozart_registry::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        previous_lock: old_lock.clone(),
        lock_pinned_names: IndexSet::new(),
    })
    .await?;

    // Compute and print change report.
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
        .filter(|c| matches!(c.kind, super::update::ChangeKind::Uninstall { .. }))
        .collect();

    console.info(&format!(
        "Package operations: {} install{}, {} update{}, {} removal{}",
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
                    console.info(&format!("  - Would remove {} ({old_version})", change.name));
                } else {
                    console.info(&format!("  - Removing {} ({old_version})", change.name));
                }
            }
            super::update::ChangeKind::Install { new_version } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - Would install {} ({new_version})",
                        change.name
                    ));
                } else {
                    console.info(&format!("  - Installing {} ({new_version})", change.name));
                }
            }
            super::update::ChangeKind::Update {
                old_version,
                new_version,
            } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - Would update {} ({old_version} => {new_version})",
                        change.name
                    ));
                } else {
                    console.info(&format!(
                        "  - Updating {} ({old_version} => {new_version})",
                        change.name
                    ));
                }
            }
        }
    }

    if !args.dry_run {
        console.info("Writing lock file");
        new_lock.write_to_file(&state.lock_path)?;
    }

    if !args.no_install && !args.dry_run {
        let prefer_source = args.prefer_source
            || args
                .prefer_install
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("source"))
                .unwrap_or(false);
        if prefer_source {
            console.info(&console_format!(
                "<warning>Warning: Source installs are not yet supported. \
                 Falling back to dist.</warning>"
            ));
        }

        let composer_config = raw.extra_fields.get("config");
        let config_optimize = composer_config
            .and_then(|c| c.get("optimize-autoloader"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let config_classmap = composer_config
            .and_then(|c| c.get("classmap-authoritative"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let config_apcu = composer_config
            .and_then(|c| c.get("apcu-autoloader"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let files_cache = mozart_registry::cache::Cache::files(
            &mozart_registry::cache::build_cache_config(cli.no_cache),
        );
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
                optimize_autoloader: args.optimize_autoloader || config_optimize,
                classmap_authoritative: args.classmap_authoritative || config_classmap,
                apcu_autoloader: args.apcu_autoloader
                    || args.apcu_autoloader_prefix.is_some()
                    || config_apcu,
                apcu_autoloader_prefix: args.apcu_autoloader_prefix.clone(),
                download_only: false,
                prefer_source: args.prefer_source,
            },
            console,
            &mut executor,
        )
        .await?;
    }

    Ok(())
}

/// Run the interactive package search+pick loop.
///
/// Returns a list of `"vendor/package:constraint"` strings that the user confirmed,
/// or an empty vec if the user typed nothing / pressed Ctrl-D immediately.
async fn interactive_search_packages(
    already_required: &indexmap::IndexSet<String>,
    preferred_stability: Stability,
    fixed: bool,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) -> anyhow::Result<Vec<String>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        anyhow::bail!(
            "Not enough arguments (missing: \"packages\") and stdin is not a TTY. \
             Pass package name(s) directly or run interactively."
        );
    }

    let mut selected: Vec<String> = Vec::new();

    loop {
        eprint!("Search for a package: ");
        let _ = std::io::stderr().flush();

        let query = {
            let stdin_locked = stdin.lock();
            let mut lines = stdin_locked.lines();
            match lines.next() {
                Some(Ok(line)) => line.trim().to_string(),
                _ => break,
            }
        };

        if query.is_empty() {
            break;
        }

        let (results, total) = match packagist::search_packages(&query, None).await {
            Ok(r) => r,
            Err(e) => {
                console.info(&console_format!(
                    "<warning>Search failed: {e}. Try again.</warning>"
                ));
                continue;
            }
        };

        let filtered: Vec<&packagist::SearchResult> = results
            .iter()
            .filter(|r| !already_required.contains(&r.name.to_lowercase()))
            .take(15)
            .collect();

        if filtered.is_empty() {
            console.info(&console_format!(
                "<warning>No new packages found for \"{query}\" (total: {total}).</warning>"
            ));
            continue;
        }

        console.info(&format!(
            "\nFound {} package{} for \"{}\":",
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" },
            query
        ));

        let name_width = filtered.iter().map(|r| r.name.len()).max().unwrap_or(0);
        for (idx, result) in filtered.iter().enumerate() {
            let desc = if result.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", result.description)
            };
            console.info(&format!(
                "  [{idx}] {:<width$}{desc}",
                result.name,
                idx = idx + 1,
                width = name_width,
            ));
        }
        console.info("  [0] Search again / enter full package name");
        console.info("");

        eprint!("Enter package # or name (leave empty to finish): ");
        let _ = std::io::stderr().flush();

        let choice = {
            let stdin_locked = stdin.lock();
            let mut lines = stdin_locked.lines();
            match lines.next() {
                Some(Ok(line)) => line.trim().to_string(),
                _ => break,
            }
        };

        if choice.is_empty() {
            break;
        }

        let package_name: String = if let Ok(num) = choice.parse::<usize>() {
            if num == 0 {
                continue;
            } else if num <= filtered.len() {
                filtered[num - 1].name.to_lowercase()
            } else {
                console.info(&console_format!(
                    "<warning>Invalid selection: {num}</warning>"
                ));
                continue;
            }
        } else {
            choice.to_lowercase()
        };

        let (pkg_name, constraint) = if package_name.contains(':') {
            match validation::parse_require_string(&package_name) {
                Ok((n, v)) => (n.to_lowercase(), v),
                Err(e) => {
                    console.info(&console_format!("<warning>Invalid: {e}</warning>"));
                    continue;
                }
            }
        } else {
            if !validation::validate_package_name(&package_name) {
                console.info(&console_format!(
                    "<warning>Invalid package name: \"{package_name}\"</warning>"
                ));
                continue;
            }

            console.info(&console_format!(
                "<info>Using version constraint for {package_name} from Packagist...</info>"
            ));

            match packagist::fetch_package_versions(&package_name, repo_cache).await {
                Ok(versions) => {
                    match version::find_best_candidate(&versions, preferred_stability) {
                        Some(best) => {
                            let stability = version::stability_of(&best.version_normalized);
                            let c = if fixed {
                                best.version.clone()
                            } else {
                                version::find_recommended_require_version(
                                    &best.version,
                                    &best.version_normalized,
                                    stability,
                                )
                            };
                            console.info(&console_format!(
                                "<info>Using version {c} for {package_name}</info>"
                            ));
                            (package_name, c)
                        }
                        None => {
                            console.info(&console_format!(
                                "<warning>Could not find a version of \"{package_name}\" \
                                 matching your minimum-stability. Try specifying it \
                                 explicitly.</warning>"
                            ));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    console.info(&console_format!(
                        "<warning>Could not fetch versions for \"{package_name}\": \
                         {e}</warning>"
                    ));
                    continue;
                }
            }
        };

        selected.push(format!("{pkg_name}:{constraint}"));

        eprint!("Search for another package? [y/N] ");
        let _ = std::io::stderr().flush();

        let again = {
            let stdin_locked = stdin.lock();
            let mut lines = stdin_locked.lines();
            match lines.next() {
                Some(Ok(line)) => line.trim().to_lowercase(),
                _ => break,
            }
        };

        if again != "y" && again != "yes" {
            break;
        }
    }

    Ok(selected)
}

pub async fn execute(
    args: &RequireArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let cache_config = mozart_registry::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_registry::cache::Cache::repo(&cache_config);

    // --- Deprecated flag warnings ---
    // Mirrors Composer\Command\RequireCommand::execute() L134-136.
    if args.no_suggest {
        console.write_error(&console_format!(
            "<warning>You are using the deprecated option \"--no-suggest\". \
             It has no effect and will break in Composer 3.</warning>"
        ));
    }
    if args.update_with_dependencies {
        console.write_error(&console_format!(
            "<warning>The -w / --update-with-dependencies flag is deprecated. \
             Use --with-dependencies instead.</warning>"
        ));
    }
    if args.update_with_all_dependencies {
        console.write_error(&console_format!(
            "<warning>The -W / --update-with-all-dependencies flag is deprecated. \
             Use --with-all-dependencies instead.</warning>"
        ));
    }

    // --- Collect package arguments (interactive if none given) ---
    let cli_packages: Vec<String> = if args.packages.is_empty() {
        if cli.no_interaction {
            anyhow::bail!("Not enough arguments (missing: \"packages\").");
        }
        let working_dir = cli.working_dir()?;
        let composer_path = working_dir.join("composer.json");

        // Read current dependencies to filter from search results (best-effort).
        let (already_required, preferred_stability) = if composer_path.exists() {
            let raw_check = package::read_from_file(&composer_path)?;
            let mut already: IndexSet<String> = IndexSet::new();
            for k in raw_check.require.keys() {
                already.insert(k.to_lowercase());
            }
            for k in raw_check.require_dev.keys() {
                already.insert(k.to_lowercase());
            }
            let stab = raw_check
                .minimum_stability
                .as_deref()
                .map(Stability::parse)
                .unwrap_or(Stability::Stable);
            (already, stab)
        } else {
            (IndexSet::new(), Stability::Stable)
        };

        let found = interactive_search_packages(
            &already_required,
            preferred_stability,
            args.fixed,
            &repo_cache,
            console,
        )
        .await?;

        if found.is_empty() {
            return Ok(());
        }
        found
    } else {
        args.packages.clone()
    };

    let working_dir = cli.working_dir()?;
    let composer_path = working_dir.join("composer.json");

    // --- Bootstrap composer.json ---
    // Mirrors Composer\Command\RequireCommand::execute() L138-152.
    let newly_created = !composer_path.exists();
    if newly_created {
        if let Err(e) = std::fs::write(&composer_path, "{\n}\n") {
            anyhow::bail!("{} could not be created: {e}", composer_path.display());
        }
    } else if std::fs::metadata(&composer_path)
        .map(|m| m.len() == 0)
        .unwrap_or(false)
    {
        std::fs::write(&composer_path, "{\n}\n")?;
    }

    // Backup original content (including the bootstrap content for new files).
    let composer_backup = std::fs::read_to_string(&composer_path)?;
    let lock_path = working_dir.join("composer.lock");
    let lock_backup = if lock_path.exists() {
        Some(std::fs::read_to_string(&lock_path)?)
    } else {
        None
    };

    // Read and parse composer.json.
    let mut raw = package::read_from_file(&composer_path)?;

    // --- firstRequire: computed from the original file, before applying changes ---
    // Mirrors Composer\Command\RequireCommand::execute() L315-321.
    let first_require = newly_created || (raw.require.is_empty() && raw.require_dev.is_empty());

    let mut state = CommandState {
        newly_created,
        first_require,
        json_path: composer_path.clone(),
        lock_path: lock_path.clone(),
        composer_backup,
        lock_backup,
        dependency_resolution_completed: false,
    };

    // --- --fixed gate ---
    // Mirrors Composer\Command\RequireCommand::execute() L173-189.
    if args.fixed {
        let package_type = raw
            .package_type
            .as_deref()
            .filter(|t| !t.is_empty())
            .unwrap_or("library");
        if package_type != "project" && !args.dev {
            console.write_error(&console_format!(
                "<error>The \"--fixed\" option is only allowed for packages with a \
                 \"project\" type or for dev dependencies to prevent possible \
                 misuses.</error>"
            ));
            if raw.package_type.is_none() {
                console.write_error(&console_format!(
                    "<error>If your package is not a library, you can explicitly specify \
                     the \"type\" by using \"mozart config type project\".</error>"
                ));
            }
            return Err(mozart_core::exit_code::bail(
                mozart_core::exit_code::GENERAL_ERROR,
                String::new(),
            ));
        }
    }

    // --- preferred_stability ---
    let preferred_stability = raw
        .minimum_stability
        .as_deref()
        .map(Stability::parse)
        .unwrap_or(Stability::Stable);

    let require_key = if args.dev { "require-dev" } else { "require" };
    let remove_key = if args.dev { "require" } else { "require-dev" };

    // --- Per-arg constraint resolution via VersionSelector ---
    // Mirrors Composer\Command\PackageDiscoveryTrait::determineRequirements().
    let version_selector = VersionSelector::new(preferred_stability, repo_cache.clone());
    let mut additions: Vec<(String, String, bool)> = Vec::new();

    for pkg_arg in &cli_packages {
        let (name, constraint) = match validation::parse_require_string(pkg_arg) {
            Ok((n, v)) => (n.to_lowercase(), v),
            Err(_) => {
                let name = pkg_arg.trim().to_lowercase();
                if !validation::validate_package_name(&name) {
                    anyhow::bail!("Invalid package name: \"{name}\"");
                }

                console_writeln!(
                    console,
                    "<info>Using version constraint for {name} from Packagist...</info>"
                );

                let best = version_selector
                    .find_best_candidate(&name)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Could not find a version of package \"{name}\" matching your \
                             minimum-stability ({preferred_stability:?}). Try requiring it \
                             with an explicit version constraint."
                        )
                    })?;

                let constraint =
                    version_selector.find_recommended_require_version_string(&best, args.fixed);

                console_writeln!(
                    console,
                    "<info>Using version {constraint} for {name}</info>",
                );

                (name, constraint)
            }
        };

        additions.push((name, constraint, args.dev));
    }

    // --- Self-require detection ---
    // Mirrors Composer\Command\RequireCommand::execute() L278-282.
    let root_name = raw.name.to_lowercase();
    for (name, _, _) in &additions {
        if name.to_lowercase() == root_name {
            anyhow::bail!(
                "Root package '{}' cannot require itself in its composer.json",
                raw.name
            );
        }
    }

    // --- Inconsistent require-key detection + warning ---
    // Mirrors Composer\Command\RequireCommand::execute() L289-311.
    let packages_by_key = get_packages_by_require_key(&raw);
    let new_package_names: Vec<String> = additions.iter().map(|(n, _, _)| n.clone()).collect();
    let inconsistent =
        get_inconsistent_require_keys(&new_package_names, require_key, &packages_by_key);
    for pkg in &inconsistent {
        let (with_without, target_key) = if args.dev {
            ("with", require_key)
        } else {
            ("without", require_key)
        };
        console.write_error(&console_format!(
            "<warning>{pkg} is currently present in the {remove_key} key and you ran the \
             command {with_without} the --dev flag, which will move it to the \
             {target_key} key.</warning>"
        ));
    }
    // Remove from the opposite section before inserting into the target.
    for pkg in &inconsistent {
        if args.dev {
            raw.require.remove(pkg.as_str());
        } else {
            raw.require_dev.remove(pkg.as_str());
        }
    }

    // --- Apply changes ---
    for (name, constraint, is_dev) in &additions {
        let section_name = if *is_dev { "require-dev" } else { "require" };
        let target = if *is_dev {
            &mut raw.require_dev
        } else {
            &mut raw.require
        };

        if let Some(existing) = target.get(name) {
            console_writeln!(
                console,
                "<comment>Updating {name} from {existing} to {constraint} in {section_name}</comment>",
            );
        } else {
            console_writeln!(
                console,
                "<info>Adding {name} ({constraint}) to {section_name}</info>",
            );
        }

        target.insert(name.clone(), constraint.clone());
    }

    // --- sort-packages ---
    let config_sort_packages = raw
        .extra_fields
        .get("config")
        .and_then(|c| c.get("sort-packages"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sort_packages = args.sort_packages || config_sort_packages;

    if sort_packages {
        let sorted_require: std::collections::BTreeMap<_, _> = raw.require.clone();
        raw.require = sorted_require;
        let sorted_dev: std::collections::BTreeMap<_, _> = raw.require_dev.clone();
        raw.require_dev = sorted_dev;
    }

    // --- Write composer.json (unless --dry-run) ---
    // Mirrors Composer\Command\RequireCommand::execute() L323-325.
    if args.dry_run {
        console_writeln!(
            console,
            "<comment>Dry run: composer.json not modified.</comment>",
        );
    } else {
        update_file(&composer_path, &raw)?;
    }

    // Print "has been created|updated".
    // Mirrors Composer\Command\RequireCommand::execute() L327.
    console.info(&console_format!(
        "<info>{} has been {}</info>",
        composer_path.display(),
        if newly_created { "created" } else { "updated" }
    ));

    // --- --no-update: skip resolution ---
    if args.no_update {
        console_writeln!(
            console,
            "<comment>Not updating dependencies, only modifying composer.json.</comment>"
        );
        return Ok(());
    }

    // --- Resolution + lock + install ---
    let update_result = do_update(&mut state, args, cli, &raw, &additions, console).await;

    // Mirrors Composer's `finally` block: cleanup newly-created file on dry-run.
    if args.dry_run && state.newly_created {
        let _ = std::fs::remove_file(&state.json_path);
    }

    update_result?;

    // --- Post-resolution constraint rewrite for 'guess' placeholders (stub, PR 2) ---
    update_requirements_after_resolution(
        &state,
        &[],
        require_key,
        remove_key,
        sort_packages,
        args.dry_run,
        args.fixed,
        console,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// Verify that --sort-packages sorts both require and require-dev maps.
    #[test]
    fn test_sort_packages_sorts_both_sections() {
        use mozart_core::package::RawPackageData;

        let mut raw = RawPackageData::new("test/project".to_string());
        raw.require
            .insert("z/package".to_string(), "^1.0".to_string());
        raw.require
            .insert("a/package".to_string(), "^2.0".to_string());
        raw.require
            .insert("m/package".to_string(), "^3.0".to_string());
        raw.require_dev
            .insert("z/dev".to_string(), "^1.0".to_string());
        raw.require_dev
            .insert("a/dev".to_string(), "^2.0".to_string());

        let sorted_require: BTreeMap<String, String> = raw.require.clone();
        raw.require = sorted_require;
        let sorted_dev: BTreeMap<String, String> = raw.require_dev.clone();
        raw.require_dev = sorted_dev;

        let require_keys: Vec<_> = raw.require.keys().collect();
        assert_eq!(require_keys, vec!["a/package", "m/package", "z/package"]);

        let dev_keys: Vec<_> = raw.require_dev.keys().collect();
        assert_eq!(dev_keys, vec!["a/dev", "z/dev"]);
    }

    /// Verify that compute_update_changes produces correct Install entries for new packages.
    #[test]
    fn test_require_change_report_new_packages() {
        let new_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);

        let changes = super::super::update::compute_update_changes(None, &new_lock, false);
        assert_eq!(changes.len(), 2);
        for change in &changes {
            assert!(
                matches!(
                    change.kind,
                    super::super::update::ChangeKind::Install { .. }
                ),
                "Expected Install, got {:?} for {}",
                change.kind,
                change.name
            );
        }
    }

    /// Verify the dry-run path does not write lock file.
    #[test]
    fn test_no_update_skips_lock_generation() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");
        assert!(!lock_path.exists());
    }

    #[test]
    fn test_require_dry_run_modifies_nothing() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");
        let vendor_dir = dir.path().join("vendor");

        let original_content = r#"{"name": "test/project", "require": {}}"#;
        std::fs::write(&composer_path, original_content).unwrap();

        assert_eq!(
            std::fs::read_to_string(&composer_path).unwrap(),
            original_content
        );
        assert!(
            !lock_path.exists(),
            "Lock file should not be created by dry run"
        );
        assert!(
            !vendor_dir.exists(),
            "Vendor dir should not be created by dry run"
        );
    }

    /// Verify firstRequire is true when require and require-dev are both empty.
    #[test]
    fn test_first_require_empty_sections() {
        use mozart_core::package::RawPackageData;

        let raw = RawPackageData::new("test/project".to_string());
        let first_require = raw.require.is_empty() && raw.require_dev.is_empty();
        assert!(
            first_require,
            "firstRequire should be true when both sections are empty"
        );
    }

    /// Verify firstRequire is false when require is non-empty.
    #[test]
    fn test_first_require_non_empty_require() {
        use mozart_core::package::RawPackageData;

        let mut raw = RawPackageData::new("test/project".to_string());
        raw.require
            .insert("some/pkg".to_string(), "^1.0".to_string());
        let first_require = raw.require.is_empty() && raw.require_dev.is_empty();
        assert!(
            !first_require,
            "firstRequire should be false when require is non-empty"
        );
    }

    /// Verify get_packages_by_require_key returns correct section for each package.
    #[test]
    fn test_get_packages_by_require_key() {
        use mozart_core::package::RawPackageData;

        let mut raw = RawPackageData::new("test/project".to_string());
        raw.require
            .insert("vendor/a".to_string(), "^1.0".to_string());
        raw.require_dev
            .insert("vendor/b".to_string(), "^2.0".to_string());

        let map = get_packages_by_require_key(&raw);
        assert_eq!(map.get("vendor/a"), Some(&"require".to_string()));
        assert_eq!(map.get("vendor/b"), Some(&"require-dev".to_string()));
        assert_eq!(map.get("vendor/c"), None);
    }

    /// Verify get_inconsistent_require_keys returns packages in the opposite section.
    #[test]
    fn test_get_inconsistent_require_keys() {
        let mut packages_by_key = IndexMap::new();
        packages_by_key.insert("vendor/a".to_string(), "require".to_string());
        packages_by_key.insert("vendor/b".to_string(), "require-dev".to_string());

        // Adding vendor/a to require-dev while it's in require → inconsistent
        let new_pkgs = vec!["vendor/a".to_string(), "vendor/c".to_string()];
        let inconsistent =
            get_inconsistent_require_keys(&new_pkgs, "require-dev", &packages_by_key);
        assert_eq!(inconsistent, vec!["vendor/a"]);

        // Adding vendor/b to require while it's in require-dev → inconsistent
        let new_pkgs2 = vec!["vendor/b".to_string()];
        let inconsistent2 = get_inconsistent_require_keys(&new_pkgs2, "require", &packages_by_key);
        assert_eq!(inconsistent2, vec!["vendor/b"]);
    }

    #[tokio::test]
    #[ignore]
    async fn test_require_full_e2e() {
        use indexmap::IndexSet;
        use mozart_core::package::RawPackageData;
        use mozart_registry::lockfile::{LockFileGenerationRequest, generate_lock_file};

        let composer_json_content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        let composer_json: RawPackageData = serde_json::from_str(composer_json_content).unwrap();

        let request = ResolveRequest {
            root_name: String::new(),
            root_version: None,
            require: vec![("psr/log".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: Stability::Stable,
            stability_flags: IndexMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
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
            preferred_versions: indexmap::IndexMap::new(),
            block_insecure: false,
        };

        let resolved = resolver::resolve(&request)
            .await
            .expect("Resolution should succeed");
        assert!(!resolved.is_empty());
        assert!(resolved.iter().any(|p| p.name == "psr/log"));

        let lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.to_string(),
            composer_json,
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
        .expect("Lock file generation should succeed");

        assert!(!lock.content_hash.is_empty());
        assert!(!lock.packages.is_empty());
        assert!(lock.packages.iter().any(|p| p.name == "psr/log"));
    }

    #[tokio::test]
    #[ignore]
    async fn test_require_no_install_writes_lock_only() {
        use indexmap::IndexSet;
        use mozart_core::package::RawPackageData;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_path = dir.path().join("composer.json");
        let lock_path = dir.path().join("composer.lock");
        let vendor_dir = dir.path().join("vendor");

        let content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        std::fs::write(&composer_path, content).unwrap();

        let raw: RawPackageData = serde_json::from_str(content).unwrap();

        let request = ResolveRequest {
            root_name: String::new(),
            root_version: None,
            require: vec![("psr/log".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: Stability::Stable,
            stability_flags: IndexMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
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
            preferred_versions: indexmap::IndexMap::new(),
            block_insecure: false,
        };

        let resolved = resolver::resolve(&request)
            .await
            .expect("Resolution should succeed");
        let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: content.to_string(),
            composer_json: raw,
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
        .expect("Lock file generation should succeed");

        new_lock.write_to_file(&lock_path).unwrap();

        assert!(lock_path.exists(), "Lock file should be written");
        assert!(
            !vendor_dir.exists(),
            "Vendor dir should NOT exist with --no-install"
        );
    }
}
