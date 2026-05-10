use clap::Args;
use indexmap::IndexSet;
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::package::{PackageInterface as _, RootPackage as _, RootPackageData};
use mozart_core::repository::installed;
use mozart_core::repository::installer_executor::{
    Action, ExecuteContext, FilesystemExecutor, InstallerExecutor, PackageOperation,
    compute_operations, compute_stale_installed_aliases, format_full_pretty_version,
    format_full_pretty_version_for_installed, format_update_pretty_versions,
    locked_to_installed_entry, previously_installed_alias_versions,
};
use mozart_core::repository::lockfile;
use std::path::Path;

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
    #[arg(long, value_parser = ["source", "dist", "auto"])]
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
    #[arg(long, value_parser = ["table", "plain", "json", "summary"])]
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
    /// Use APCu to cache found/not-found classes.
    pub apcu_autoloader: bool,
    /// Custom prefix for APCu autoloader cache.
    pub apcu_autoloader_prefix: Option<String>,
    /// Only download packages, skip autoloader generation and installed.json write.
    pub download_only: bool,
    /// Prefer installing from VCS source rather than dist archives.
    pub prefer_source: bool,
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
            apcu_autoloader: false,
            apcu_autoloader_prefix: None,
            download_only: false,
            prefer_source: false,
        }
    }
}

/// Run all lock-verification checks that are temporary stand-ins for
/// `Composer\Solver::solve` against a lock-only request. Returns all problem
/// strings combined so a future "swap for SAT-verify" change is a single
/// function replacement.
fn verify_lock(
    root: &RootPackageData,
    lock: &lockfile::LockFile,
    dev_mode: bool,
    ignore_platform_reqs: bool,
    ignore_platform_req: &[String],
) -> Vec<String> {
    let mut problems = verify_lock_platform_problems(
        root,
        lock,
        dev_mode,
        ignore_platform_reqs,
        ignore_platform_req,
    );
    problems.extend(verify_lock_same_name_problems(lock, dev_mode));
    problems.extend(verify_lock_conflict_problems(lock, dev_mode));
    problems.extend(verify_lock_root_require_problems(lock, root, dev_mode));
    problems
}

/// Verify root + lock platform requirements against the detected platform.
///
/// Mirrors the platform checks Composer performs in `Installer::doInstall()`
/// when installing from an existing lock: every platform requirement that
/// appears in either the root `composer.json` (`require`/`require-dev`) or the
/// lock's `platform`/`platform-dev` field must be satisfied by the current
/// system. composer.json takes precedence over the lock on duplicate keys.
///
/// Returns the list of "Root composer.json requires …" diagnostic lines (one
/// per failing requirement). An empty vec means everything is satisfied.
fn verify_lock_platform_problems(
    root: &RootPackageData,
    lock: &lockfile::LockFile,
    dev_mode: bool,
    ignore_platform_reqs: bool,
    ignore_platform_req: &[String],
) -> Vec<String> {
    let combined = combine_platform_requirements(root, lock, dev_mode);
    if combined.is_empty() {
        return Vec::new();
    }
    let platform = mozart_core::platform::detect_platform();
    check_platform_requirements_against(
        &combined,
        &platform,
        ignore_platform_reqs,
        ignore_platform_req,
    )
}

/// Mirror Composer's `RuleSetGenerator::addConflictRules` SAME_NAME loop on
/// the locked package set. `getNames(false)` returns each package's
/// canonical name plus the names it claims via `replace`; when two distinct
/// locked packages claim the same name, only one of them can be installed.
/// During Composer's lock-verify solve every locked package is `fix`-locked,
/// so two providers of the same name make the rule unsatisfiable and the
/// solver throws `SolverProblemsException` → exit 2.
///
/// `provide` is intentionally excluded — `getNames(false)` excludes it, and
/// virtual `provide` targets allow multiple co-installed providers.
fn verify_lock_same_name_problems(lock: &lockfile::LockFile, dev_mode: bool) -> Vec<String> {
    let mut providers: indexmap::IndexMap<_, Vec<_>> = indexmap::IndexMap::new();

    let mut all_pkgs: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();
    if dev_mode {
        all_pkgs.extend(lock.packages_dev.iter().flatten());
    }

    for p in all_pkgs {
        let canonical = p.name.to_lowercase();
        providers
            .entry(canonical.clone())
            .or_default()
            .push(p.name.clone());
        for replace_target in p.replace.keys() {
            let target_lower = replace_target.to_lowercase();
            if target_lower == canonical {
                continue;
            }
            providers
                .entry(target_lower)
                .or_default()
                .push(p.name.clone());
        }
    }

    let mut problems = Vec::new();
    for (name, owners) in &providers {
        if owners.len() > 1 {
            problems.push(format!(
                "- Conflict between locked packages on name {}: {}",
                name,
                owners.join(", ")
            ));
        }
    }
    problems
}

/// Detect locked-package requires whose target is the current root but
/// whose version constraint no longer matches the root's declared version.
/// Mirrors the slice of Composer's `Installer::doInstall` SAT verify that
/// surfaces messages like
/// `"b/requirer 1.0.0 requires root/pkg ^1 -> found root/pkg[2.x-dev] but
/// it does not match the constraint"`: when the user bumps the root's
/// `version` (or its branch alias) past the range a locked dependent
/// expects, the lock can't be installed as-is and the resolver-equivalent
/// must bail with exit-code 2 before any package operations run.
fn verify_lock_root_require_problems(
    lock: &lockfile::LockFile,
    root: &RootPackageData,
    dev_mode: bool,
) -> Vec<String> {
    use mozart_semver::{Version, VersionConstraint};

    let root_version = root.pretty_version();
    if root_version == "1.0.0+no-version-set" || root.name().is_empty() || root_version.is_empty() {
        return Vec::new();
    }
    let root_name_lower = root.name().to_string();
    let Ok(parsed_root_version) = Version::parse(root_version) else {
        return Vec::new();
    };

    let mut all_pkgs: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();
    if dev_mode {
        all_pkgs.extend(lock.packages_dev.iter().flatten());
    }

    let mut problems = Vec::new();
    for &p in &all_pkgs {
        for (target, constraint_str) in &p.require {
            if target.to_lowercase() != root_name_lower {
                continue;
            }
            let Ok(constraint) = VersionConstraint::parse(constraint_str) else {
                continue;
            };
            if constraint.matches(&parsed_root_version) {
                continue;
            }
            problems.push(format!(
                "- {pkg_name} is locked to version {pkg_version} and an update of this package was not requested.\n    - {pkg_name} {pkg_version} requires {target} {constraint_str} -> found {target}[{root_version}] but it does not match the constraint.",
                pkg_name = p.name,
                pkg_version = p.version,
            ));
        }
    }
    problems
}

/// Detect declared `conflict` clashes between two packages already in the
/// lock. Mirrors what Composer's `Installer::doInstall` SAT verify catches
/// when one locked package conflicts with another locked package's version
/// (including its branch-alias and the lock's top-level `aliases` block):
/// the SAT solver fails with `SolverProblemsException`, exit-code 2,
/// and the user is told to run `composer update`.
///
/// We don't yet run a full SAT verify on `install`; this targeted check
/// covers the lock-file-only conflict case the SAT solver would have
/// caught. Each (declarer, target) pair where the target's effective
/// version satisfies the declarer's `conflict` constraint is reported.
fn verify_lock_conflict_problems(lock: &lockfile::LockFile, dev_mode: bool) -> Vec<String> {
    use mozart_semver::{Version, VersionConstraint};

    let mut all_pkgs: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();
    if dev_mode {
        all_pkgs.extend(lock.packages_dev.iter().flatten());
    }

    // Collect every (name → version_string) pair the lock advertises so a
    // conflict against a name can be matched against any version that name
    // resolves to. Sources, in order: a package's own `(name, version)`,
    // its `extra.branch-alias` mapping, the lock's top-level `aliases`
    // block, and each `replace` target with its declared constraint as a
    // best-effort version (Composer's solver would re-run constraint
    // intersection here; we treat exact replace constraints as concrete
    // versions to keep this check string-based).
    let mut name_versions: Vec<(String, String, &lockfile::LockedPackage)> = Vec::new();
    for &p in &all_pkgs {
        let lower_name = p.name.to_lowercase();
        name_versions.push((lower_name.clone(), p.version.clone(), p));
        if let Some(branch_alias) = p
            .extra_fields
            .get("extra")
            .and_then(|e| e.get("branch-alias"))
            .and_then(|m| m.as_object())
            && let Some(alias_target) = branch_alias.get(&p.version).and_then(|v| v.as_str())
        {
            name_versions.push((lower_name.clone(), alias_target.to_string(), p));
        }
        for (target, constraint) in &p.replace {
            name_versions.push((target.to_lowercase(), constraint.clone(), p));
        }
    }
    for la in &lock.aliases {
        // The lock's top-level aliases block exposes a package under the
        // alias's pretty version pointing at a base (`package`, `version`).
        // Find that base in `all_pkgs` so the alias inherits its conflict
        // declarations transparently.
        if let Some(base) = all_pkgs
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&la.package) && p.version == la.version)
        {
            name_versions.push((la.package.to_lowercase(), la.alias.clone(), base));
        }
    }

    let mut problems = Vec::new();
    for &p in &all_pkgs {
        for (target, conflict_constraint) in &p.conflict {
            let target_lower = target.to_lowercase();
            let Ok(constraint) = VersionConstraint::parse(conflict_constraint) else {
                continue;
            };
            for (name, ver, source) in &name_versions {
                if name != &target_lower {
                    continue;
                }
                if std::ptr::eq(*source as *const _, p as *const _) {
                    continue;
                }
                let Ok(parsed_ver) = Version::parse(ver) else {
                    continue;
                };
                if constraint.matches(&parsed_ver) {
                    problems.push(format!(
                        "- {} {} conflicts with {} {}.",
                        p.name, p.version, source.name, ver
                    ));
                    break;
                }
            }
        }
    }
    problems
}

/// Merge platform requirements from the lock's `platform`/`platform-dev`
/// fields and the root composer.json's `require`/`require-dev`. Root
/// composer.json overrides the lock on duplicate keys (matching Composer's
/// "composer.json as source of truth" rule for shared platform reqs).
fn combine_platform_requirements(
    root: &RootPackageData,
    lock: &lockfile::LockFile,
    dev_mode: bool,
) -> indexmap::IndexMap<String, String> {
    let mut combined = indexmap::IndexMap::new();

    if let Some(obj) = lock.platform.as_object() {
        for (name, val) in obj {
            if let Some(s) = val.as_str() {
                combined.insert(name.to_lowercase(), s.to_string());
            }
        }
    }
    if dev_mode && let Some(obj) = lock.platform_dev.as_object() {
        for (name, val) in obj {
            if let Some(s) = val.as_str() {
                combined.insert(name.to_lowercase(), s.to_string());
            }
        }
    }

    for (name, link) in root.requires() {
        let lower = name.to_lowercase();
        if mozart_core::platform::is_platform_package(&lower) {
            combined.insert(lower, link.constraint.clone());
        }
    }
    if dev_mode {
        for (name, link) in root.dev_requires() {
            let lower = name.to_lowercase();
            if mozart_core::platform::is_platform_package(&lower) {
                combined.insert(lower, link.constraint.clone());
            }
        }
    }

    combined
}

fn check_platform_requirements_against(
    combined: &indexmap::IndexMap<String, String>,
    platform: &[mozart_core::platform::PlatformPackage],
    ignore_platform_reqs: bool,
    ignore_platform_req: &[String],
) -> Vec<String> {
    if ignore_platform_reqs {
        return Vec::new();
    }

    let ignored: IndexSet<String> = ignore_platform_req
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    let mut messages = Vec::new();
    for (name, constraint_str) in combined {
        if ignored.contains(name) {
            continue;
        }

        let constraint = match mozart_semver::VersionConstraint::parse(constraint_str) {
            Ok(c) => c,
            Err(_) => continue,
        };

        match platform.iter().find(|p| p.name == *name) {
            None => {
                if let Some(ext_name) = name.strip_prefix("ext-") {
                    messages.push(format!(
                        "- Root composer.json requires PHP extension {name} {constraint_str} but it is missing from your system. Install or enable PHP's {ext_name} extension."
                    ));
                } else {
                    messages.push(format!(
                        "- Root composer.json requires {name} {constraint_str} but it is not present on your system."
                    ));
                }
            }
            Some(detected) => {
                if let Ok(version) = mozart_semver::Version::parse(&detected.version)
                    && !constraint.matches(&version)
                {
                    messages.push(format!(
                        "- Root composer.json requires {name} {constraint_str} but your {name} version ({}) does not satisfy that requirement.",
                        detected.version
                    ));
                }
            }
        }
    }

    messages
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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) {
    if ignore_platform_reqs {
        return;
    }

    let ignored_set: IndexSet<String> = ignore_platform_req
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    for pkg in packages {
        for (req_name, req_constraint) in &pkg.require {
            if mozart_core::platform::is_platform_package(req_name) {
                let lower = req_name.to_lowercase();
                if !ignored_set.contains(&lower) {
                    io.lock().unwrap().info(&console_format!(
                        "<warning>Platform requirement {req_name} {req_constraint} (required by {}) \
                         has not been verified. Platform detection is not yet fully implemented.</warning>",
                        pkg.name
                    ));
                }
            }
        }
    }
}

pub async fn install_from_lock(
    lock: &lockfile::LockFile,
    working_dir: &Path,
    vendor_dir: &Path,
    config: &InstallConfig,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    executor: &mut dyn InstallerExecutor,
) -> anyhow::Result<()> {
    let dev_mode = config.dev_mode;

    // Step 1: Determine which packages to install
    let mut packages_to_install: Vec<&lockfile::LockedPackage> = lock.packages.iter().collect();

    if dev_mode && let Some(ref dev_pkgs) = lock.packages_dev {
        packages_to_install.extend(dev_pkgs.iter());
    }

    // Print install mode header
    if dev_mode {
        io.lock()
            .unwrap()
            .info("Installing dependencies from lock file (including require-dev)");
    } else {
        io.lock()
            .unwrap()
            .info("Installing dependencies from lock file");
    }
    io.lock()
        .unwrap()
        .info("Verifying lock file contents can be installed on current platform.");

    // Step 2: Warn about platform requirements
    warn_platform_requirements(
        &packages_to_install,
        config.ignore_platform_reqs,
        &config.ignore_platform_req,
        io.clone(),
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
        io.lock()
            .unwrap()
            .info("Nothing to install, update or remove");
    } else {
        io.lock().unwrap().info(&console_format!(
            "<info>Package operations: {} install{}, {} update{}, {} removal{}</info>",
            installs.len(),
            if installs.len() == 1 { "" } else { "s" },
            updates.len(),
            if updates.len() == 1 { "" } else { "s" },
            removals.len(),
            if removals.len() == 1 { "" } else { "s" },
        ));
    }

    // Step 6: Execute operations (unless dry_run). Removals run first to
    // mirror Composer's `Transaction::moveUninstallsToFront`.
    if config.dry_run {
        for name in &removals {
            io.lock()
                .unwrap()
                .info(&console_format!("  - Would remove <info>{}</info>", name));
        }
        for (pkg, action) in &ops {
            match action {
                Action::Skip => {}
                Action::Install => {
                    io.lock().unwrap().info(&console_format!(
                        "  - Would install <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                }
                Action::Update => {
                    io.lock().unwrap().info(&console_format!(
                        "  - Would upgrade <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                }
            }
        }
    } else {
        let exec_ctx = ExecuteContext {
            vendor_dir: vendor_dir.to_path_buf(),
            no_progress: config.no_progress,
            prefer_source: config.prefer_source,
        };

        for name in &removals {
            io.lock()
                .unwrap()
                .info(&console_format!("  - Removing <info>{}</info>", name));
            // Mirrors Composer's `UninstallOperation::show`, which renders
            // the package's `getFullPrettyVersion()` — for dev packages
            // backed by git/hg that includes the (truncated) source ref.
            let from_entry = installed
                .packages
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name));
            let from_full = from_entry
                .map(format_full_pretty_version_for_installed)
                .unwrap_or_default();
            executor.uninstall_package(name, &from_full, &exec_ctx)?;
        }

        // Mirror Composer's `Transaction::moveUninstallsToFront` +
        // `MarkAliasUninstalledOperation` emission: any alias declared on a
        // currently-installed package (via `extra.branch-alias`) that is no
        // longer present in the new lock's `aliases[]` block needs a trace
        // line so consumers see the alias was retired alongside its target.
        // Detection runs before installs/updates since Composer hoists alias
        // uninstalls to the front of the operations list.
        let stale_aliases = compute_stale_installed_aliases(&installed, lock);
        for stale in &stale_aliases {
            executor
                .install_package(
                    PackageOperation::MarkAliasUninstalled {
                        name: &stale.name,
                        alias_full: &stale.alias_full,
                        target_full: &stale.target_full,
                    },
                    &exec_ctx,
                )
                .await?;
        }

        if !removals.is_empty() {
            executor.cleanup_after_uninstalls(&exec_ctx)?;
        }

        for (pkg, action) in &ops {
            // Owned scratch buffers the Update branch borrows for
            // `PackageOperation::Update::{from_full_pretty,to_full_pretty}`.
            // Declared at loop scope so the borrows outlive the await call.
            let from_full_pretty_buf;
            let to_full_pretty_buf;
            let op: Option<PackageOperation<'_>> = match action {
                // Skip still falls through to the alias-mark block below:
                // Composer's `Transaction::calculateOperations` emits a
                // MarkAliasInstalled even when the target package itself is
                // already present, as long as the alias hasn't been recorded
                // in `installed.json` yet (`presentAliasMap` miss). This
                // matters for `update --lock` from a lock that introduced a
                // new root alias on a previously-installed package.
                Action::Skip => None,
                Action::Install => {
                    io.lock().unwrap().info(&console_format!(
                        "  - Installing <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                    Some(PackageOperation::Install { package: pkg })
                }
                Action::Update => {
                    io.lock().unwrap().info(&console_format!(
                        "  - Upgrading <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                    // Pull the previously-installed entry from installed.json
                    // so the trace recorder can format
                    // `Upgrading pkg (oldVersion => newVersion)`. The plain
                    // version drives the upgrade/downgrade direction; the
                    // full-pretty pair is rendered through
                    // `format_update_pretty_versions` so Composer's
                    // SOURCE_REF / DIST_REF mode switch (used when both
                    // sides would otherwise render identically) lands on
                    // both halves.
                    let from_entry = installed
                        .packages
                        .iter()
                        .find(|p| p.name.eq_ignore_ascii_case(&pkg.name));
                    let from_version = from_entry.map(|p| p.version.as_str()).unwrap_or("");
                    if let Some(entry) = from_entry {
                        let (from, to) = format_update_pretty_versions(entry, pkg);
                        from_full_pretty_buf = from;
                        to_full_pretty_buf = to;
                    } else {
                        from_full_pretty_buf = String::new();
                        to_full_pretty_buf = format_full_pretty_version(pkg);
                    }
                    Some(PackageOperation::Update {
                        from_version,
                        from_full_pretty: &from_full_pretty_buf,
                        to_full_pretty: &to_full_pretty_buf,
                        package: pkg,
                    })
                }
            };
            if let Some(op) = op {
                executor.install_package(op, &exec_ctx).await?;
            }

            // After the target install/update, emit MarkAliasInstalled for any
            // aliases whose `package`+`version` (the target's pretty version)
            // match. Mirrors Composer's `Transaction::calculateOperations` DFS
            // which pushes alias targets first and emits MarkAliasInstalled
            // when the alias itself is processed.
            //
            // Two sources of alias entries: the `aliases[]` block in
            // `composer.lock` (which the resolver populates with both root
            // aliases and branch-aliases) and the package's own
            // `extra.branch-alias` (recovered through
            // `Locker::getLockedRepository`'s ArrayLoader expansion when the
            // lock was hand-written without a matching `aliases[]` entry).
            // The two sources can name the same alias version, so dedupe by
            // `alias_normalized` to avoid emitting the trace line twice.
            //
            // Also skip aliases that were already in installed.json under the
            // same name+normalized version: Composer's
            // `Transaction::calculateOperations` only emits a
            // MarkAliasInstalledOperation when the alias is *not* already in
            // `presentAliasMap`. An update that keeps the same alias version
            // (e.g. `dev-main` ref bump on a `default-branch` package) does
            // not retrigger the alias mark.
            let already_installed_aliases =
                previously_installed_alias_versions(&installed, &pkg.name);
            let mut emitted_alias_versions: Vec<String> = Vec::new();
            for alias in &lock.aliases {
                if alias.package.eq_ignore_ascii_case(&pkg.name) && alias.version == pkg.version {
                    if already_installed_aliases.contains(&alias.alias_normalized) {
                        emitted_alias_versions.push(alias.alias_normalized.clone());
                        continue;
                    }
                    executor
                        .install_package(
                            PackageOperation::MarkAliasInstalled { alias, target: pkg },
                            &exec_ctx,
                        )
                        .await?;
                    emitted_alias_versions.push(alias.alias_normalized.clone());
                }
            }
            let branch_aliases = lockfile::locked_package_branch_aliases(pkg);
            for alias in &branch_aliases {
                if emitted_alias_versions.contains(&alias.alias_normalized) {
                    continue;
                }
                if already_installed_aliases.contains(&alias.alias_normalized) {
                    emitted_alias_versions.push(alias.alias_normalized.clone());
                    continue;
                }
                executor
                    .install_package(
                        PackageOperation::MarkAliasInstalled { alias, target: pkg },
                        &exec_ctx,
                    )
                    .await?;
                emitted_alias_versions.push(alias.alias_normalized.clone());
            }
        }

        // Step 8: Write updated vendor/composer/installed.json (unless download_only)
        if !config.download_only {
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
        }

        // Step 9: Generate autoloader (unless no_autoloader or download_only)
        if !config.no_autoloader && !config.download_only {
            io.lock().unwrap().info("Generating autoload files");

            if config.classmap_authoritative {
                io.lock().unwrap().info(&console_format!(
                    "<info>Classmap-authoritative mode: autoloader will only look up classes in the classmap.</info>"
                ));
            } else if config.optimize_autoloader {
                io.lock().unwrap().info(&console_format!(
                    "<info>Optimize autoloader: classmap scanning is not yet fully supported. PSR-4/PSR-0 autoloading will still be used.</info>"
                ));
            }

            let suffix = lock.content_hash.clone();

            let _result =
                mozart_core::autoload::generate(&mozart_core::autoload::AutoloadConfig {
                    project_dir: working_dir.to_path_buf(),
                    vendor_dir: vendor_dir.to_path_buf(),
                    dev_mode,
                    suffix,
                    classmap_authoritative: config.classmap_authoritative,
                    optimize: config.optimize_autoloader,
                    apcu: config.apcu_autoloader,
                    apcu_prefix: config.apcu_autoloader_prefix.clone(),
                    strict_psr: false,
                    strict_ambiguous: false,
                    platform_check: mozart_core::autoload::PlatformCheckMode::Full,
                    ignore_platform_reqs: config.ignore_platform_reqs,
                })?;
        }
    }

    Ok(())
}

/// CLI entry point. Builds production [`mozart_core::repository::repository::RepositorySet`]
/// (Packagist) and [`FilesystemExecutor`] from `cli`, then dispatches to [`run`].
pub async fn execute(
    args: &InstallArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let repositories = std::sync::Arc::new(
        mozart_core::repository::repository::RepositorySet::with_packagist(
            mozart_core::repository::cache::Cache::repo(&cache_config),
        ),
    );
    let mut executor =
        FilesystemExecutor::new(mozart_core::repository::cache::Cache::files(&cache_config));
    let working_dir = cli.working_dir()?;
    run(&working_dir, None, args, io, repositories, &mut executor).await
}

/// Library entry point — pure logic, no `Cli` access.
///
/// In-process tests construct an empty `RepositorySet` (Composer's
/// `'packagist' => false` test config) and a tracing `InstallerExecutor`,
/// then call this function directly to exercise the install flow without
/// spawning the binary.
///
/// `path_repo_base_override` is the in-process test escape hatch for relative
/// `type: path` repo URLs — see [`super::update::run`] for the full rationale.
/// Production callers pass `None` to anchor against `working_dir`.
pub async fn run(
    working_dir: &Path,
    path_repo_base_override: Option<&Path>,
    args: &InstallArgs,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    repositories: std::sync::Arc<mozart_core::repository::repository::RepositorySet>,
    executor: &mut dyn InstallerExecutor,
) -> anyhow::Result<()> {
    // Step 2: Validate arguments — order matches Composer's InstallCommand::execute (80–101):
    // 1. deprecation warnings, 2. reject packages, 3. reject --no-install,
    // 4. Mozart-only prefer-install mutual-exclusion.
    if args.dev {
        io.lock().unwrap().info(&console_format!(
            "<warning>You are using the deprecated option \"--dev\". It has no effect and will break in Composer 3.</warning>"
        ));
    }

    if args.no_suggest {
        io.lock().unwrap().info(&console_format!(
            "<warning>You are using the deprecated option \"--no-suggest\". It has no effect and will break in Composer 3.</warning>"
        ));
    }

    if !args.packages.is_empty() {
        let pkgs = args.packages.join(" ");
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            format!(
                "Invalid argument {pkgs}. Use \"mozart require {pkgs}\" instead to add packages to your composer.json."
            ),
        ));
    }

    if args.no_install {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            "Invalid option \"--no-install\". Use \"mozart update --no-install\" instead if you are trying to update the composer.lock file.",
        ));
    }

    if args.prefer_install.is_some() && (args.prefer_source || args.prefer_dist) {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            "The --prefer-install option cannot be used together with --prefer-source or --prefer-dist.",
        ));
    }

    // Step 3: Read composer.lock
    // If no lock file present, fall back to update (matching Composer behavior).
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        io.lock().unwrap().info(&console_format!(
            "<warning>No composer.lock file present. Updating dependencies to latest instead of installing from lock file. See https://getcomposer.org/install for more information.</warning>"
        ));
        let update_args = super::update::UpdateArgs {
            packages: vec![],
            with: vec![],
            prefer_source: args.prefer_source,
            prefer_dist: args.prefer_dist,
            prefer_install: args.prefer_install.clone(),
            dry_run: args.dry_run,
            dev: args.dev,
            no_dev: args.no_dev,
            lock: false,
            no_install: false,
            no_audit: !args.audit,
            audit_format: args.audit_format.clone(),
            no_security_blocking: args.no_security_blocking,
            no_autoloader: args.no_autoloader,
            no_suggest: args.no_suggest,
            no_progress: args.no_progress,
            with_dependencies: false,
            with_all_dependencies: false,
            optimize_autoloader: args.optimize_autoloader,
            classmap_authoritative: args.classmap_authoritative,
            apcu_autoloader: args.apcu_autoloader,
            apcu_autoloader_prefix: args.apcu_autoloader_prefix.clone(),
            ignore_platform_req: args.ignore_platform_req.clone(),
            ignore_platform_reqs: args.ignore_platform_reqs,
            prefer_stable: false,
            prefer_lowest: false,
            minimal_changes: false,
            patch_only: false,
            interactive: false,
            root_reqs: false,
            bump_after_update: None,
        };
        // Forward the caller's repositories + executor so in-process tests
        // see their mocks honored across the install→update fallback edge.
        return super::update::run(
            working_dir,
            path_repo_base_override,
            &update_args,
            io,
            repositories,
            executor,
        )
        .await;
    }
    let lock = lockfile::LockFile::read_from_file(&lock_path)?;

    // Step 4: Determine dev mode (needed for the lock-vs-composer.json check)
    let dev_mode = !args.no_dev;

    // Step 5: Freshness check + lock-vs-composer.json requirement check
    //
    // Mirrors `Composer\Installer::doInstall()` lines 745-756: if the lock is
    // stale, warn; then verify every root require (and require-dev when in dev
    // mode) is satisfied by the lock contents. If not, exit with
    // ERROR_LOCK_FILE_INVALID (4) before attempting to install.
    let composer_json_path = working_dir.join("composer.json");
    if composer_json_path.exists() {
        let content = std::fs::read_to_string(&composer_json_path)?;
        if !lock.is_fresh(&content) {
            io.lock().unwrap().info(&console_format!(
                "<warning>Warning: The lock file is not up to date with the latest changes in composer.json. You may be getting outdated dependencies. It is recommended that you run `mozart update`.</warning>"
            ));
        }

        let raw_pkg = mozart_core::package::read_from_file(&composer_json_path)?;
        raw_pkg.validate_root_does_not_self_require()?;
        let root_pkg = RootPackageData::from_raw(raw_pkg);
        let missing = lock.get_missing_requirement_info(&root_pkg, dev_mode);
        if !missing.is_empty() {
            for line in &missing {
                io.lock().unwrap().info(line);
            }
            // Mirrors `Composer\Installer::doInstall()` lines 749-756: when
            // `config.allow-missing-requirements` is true, print the warnings
            // but proceed with what the lock already covers instead of
            // bailing with ERROR_LOCK_FILE_INVALID.
            let allow_missing = root_pkg
                .config()
                .get("allow-missing-requirements")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !allow_missing {
                return Err(mozart_core::exit_code::bail_silent(
                    mozart_core::exit_code::LOCK_FILE_INVALID,
                ));
            }
        }

        let lock_problems = verify_lock(
            &root_pkg,
            &lock,
            dev_mode,
            args.ignore_platform_reqs,
            &args.ignore_platform_req,
        );
        if !lock_problems.is_empty() {
            io.lock().unwrap().info(
                "Your lock file does not contain a compatible set of packages. Please run composer update.",
            );
            io.lock().unwrap().info("");
            for (i, msg) in lock_problems.iter().enumerate() {
                io.lock().unwrap().info(&format!("  Problem {}", i + 1));
                io.lock().unwrap().info(&format!("    {msg}"));
            }
            return Err(mozart_core::exit_code::bail_silent(
                mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
            ));
        }
    }

    // Step 6: Determine if prefer-source is enabled
    let prefer_source = args.prefer_source
        || args
            .prefer_install
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("source"))
            .unwrap_or(false);

    let vendor_dir = working_dir.join("vendor");

    // Step 7: Delegate to shared install_from_lock()
    let _ = repositories; // unused — install_from_lock has no resolver phase
    install_from_lock(
        &lock,
        working_dir,
        &vendor_dir,
        &InstallConfig {
            dev_mode,
            dry_run: args.dry_run,
            no_autoloader: args.no_autoloader,
            no_progress: args.no_progress,
            ignore_platform_reqs: args.ignore_platform_reqs,
            ignore_platform_req: args.ignore_platform_req.clone(),
            // Mirror Composer's setter coupling (Installer.php 1247–1272):
            // classmap_authoritative=true forces optimize_autoloader=true.
            optimize_autoloader: args.optimize_autoloader || args.classmap_authoritative,
            classmap_authoritative: args.classmap_authoritative,
            apcu_autoloader: args.apcu_autoloader || args.apcu_autoloader_prefix.is_some(),
            apcu_autoloader_prefix: args.apcu_autoloader_prefix.clone(),
            download_only: args.download_only,
            prefer_source,
        },
        io,
        executor,
    )
    .await
}
