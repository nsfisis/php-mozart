use clap::Args;
use indexmap::IndexSet;
use mozart_core::console;
use mozart_core::console_format;
use mozart_registry::installed;
use mozart_registry::installer_executor::{
    Action, ExecuteContext, FilesystemExecutor, InstallerExecutor, PackageOperation,
    compute_operations, compute_stale_installed_aliases, format_full_pretty_version,
    format_full_pretty_version_for_installed, format_update_pretty_versions,
    locked_to_installed_entry, previously_installed_alias_versions,
};
use mozart_registry::lockfile;
use std::collections::BTreeMap;
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
    root: &mozart_core::package::RawPackageData,
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
    root: &mozart_core::package::RawPackageData,
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
    let mut providers: BTreeMap<String, Vec<String>> = BTreeMap::new();

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
    root: &mozart_core::package::RawPackageData,
    dev_mode: bool,
) -> Vec<String> {
    use mozart_semver::{Version, VersionConstraint};

    let Some(root_version) = root.version.as_deref() else {
        return Vec::new();
    };
    if root.name.is_empty() || root_version.is_empty() {
        return Vec::new();
    }
    let root_name_lower = root.name.to_lowercase();
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
    root: &mozart_core::package::RawPackageData,
    lock: &lockfile::LockFile,
    dev_mode: bool,
) -> BTreeMap<String, String> {
    let mut combined: BTreeMap<String, String> = BTreeMap::new();

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

    for (name, constraint) in &root.require {
        let lower = name.to_lowercase();
        if mozart_core::platform::is_platform_package(&lower) {
            combined.insert(lower, constraint.clone());
        }
    }
    if dev_mode {
        for (name, constraint) in &root.require_dev {
            let lower = name.to_lowercase();
            if mozart_core::platform::is_platform_package(&lower) {
                combined.insert(lower, constraint.clone());
            }
        }
    }

    combined
}

fn check_platform_requirements_against(
    combined: &BTreeMap<String, String>,
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
    console: &console::Console,
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
                    console.info(&console_format!(
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
    console: &mozart_core::console::Console,
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
        console.info("Installing dependencies from lock file (including require-dev)");
    } else {
        console.info("Installing dependencies from lock file");
    }
    console.info("Verifying lock file contents can be installed on current platform.");

    // Step 2: Warn about platform requirements
    warn_platform_requirements(
        &packages_to_install,
        config.ignore_platform_reqs,
        &config.ignore_platform_req,
        console,
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
        console.info("Nothing to install, update or remove");
    } else {
        console.info(&console_format!(
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
            console.info(&console_format!("  - Would remove <info>{}</info>", name));
        }
        for (pkg, action) in &ops {
            match action {
                Action::Skip => {}
                Action::Install => {
                    console.info(&console_format!(
                        "  - Would install <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                }
                Action::Update => {
                    console.info(&console_format!(
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
            console.info(&console_format!("  - Removing <info>{}</info>", name));
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
                    console.info(&console_format!(
                        "  - Installing <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                    Some(PackageOperation::Install { package: pkg })
                }
                Action::Update => {
                    console.info(&console_format!(
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
            console.info("Generating autoload files");

            if config.classmap_authoritative {
                console.info(&console_format!(
                    "<info>Classmap-authoritative mode: autoloader will only look up classes in the classmap.</info>"
                ));
            } else if config.optimize_autoloader {
                console.info(&console_format!(
                    "<info>Optimize autoloader: classmap scanning is not yet fully supported. PSR-4/PSR-0 autoloading will still be used.</info>"
                ));
            }

            let suffix = lock.content_hash.clone();

            let _result =
                mozart_autoload::autoload::generate(&mozart_autoload::autoload::AutoloadConfig {
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
                    platform_check: mozart_autoload::autoload::PlatformCheckMode::Full,
                    ignore_platform_reqs: config.ignore_platform_reqs,
                })?;
        }
    }

    Ok(())
}

/// CLI entry point. Builds production [`mozart_registry::repository::RepositorySet`]
/// (Packagist) and [`FilesystemExecutor`] from `cli`, then dispatches to [`run`].
pub async fn execute(
    args: &InstallArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let cache_config = mozart_registry::cache::build_cache_config(cli.no_cache);
    let repositories =
        std::sync::Arc::new(mozart_registry::repository::RepositorySet::with_packagist(
            mozart_registry::cache::Cache::repo(&cache_config),
        ));
    let mut executor = FilesystemExecutor::new(mozart_registry::cache::Cache::files(&cache_config));
    let working_dir = cli.working_dir()?;
    run(
        &working_dir,
        None,
        args,
        console,
        repositories,
        &mut executor,
    )
    .await
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
    console: &mozart_core::console::Console,
    repositories: std::sync::Arc<mozart_registry::repository::RepositorySet>,
    executor: &mut dyn InstallerExecutor,
) -> anyhow::Result<()> {
    // Step 2: Validate arguments — order matches Composer's InstallCommand::execute (80–101):
    // 1. deprecation warnings, 2. reject packages, 3. reject --no-install,
    // 4. Mozart-only prefer-install mutual-exclusion.
    if args.dev {
        console.info(&console_format!(
            "<warning>You are using the deprecated option \"--dev\". It has no effect and will break in Composer 3.</warning>"
        ));
    }

    if args.no_suggest {
        console.info(&console_format!(
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
        console.info(&console_format!(
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
            console,
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
            console.info(&console_format!(
                "<warning>Warning: The lock file is not up to date with the latest changes in composer.json. You may be getting outdated dependencies. It is recommended that you run `mozart update`.</warning>"
            ));
        }

        let root_pkg = mozart_core::package::read_from_file(&composer_json_path)?;
        root_pkg.validate_root_does_not_self_require()?;
        let missing = lock.get_missing_requirement_info(&root_pkg, dev_mode);
        if !missing.is_empty() {
            for line in &missing {
                console.info(line);
            }
            // Mirrors `Composer\Installer::doInstall()` lines 749-756: when
            // `config.allow-missing-requirements` is true, print the warnings
            // but proceed with what the lock already covers instead of
            // bailing with ERROR_LOCK_FILE_INVALID.
            let allow_missing = root_pkg
                .extra_fields
                .get("config")
                .and_then(|v| v.get("allow-missing-requirements"))
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
            console.info(
                "Your lock file does not contain a compatible set of packages. Please run composer update.",
            );
            console.info("");
            for (i, msg) in lock_problems.iter().enumerate() {
                console.info(&format!("  Problem {}", i + 1));
                console.info(&format!("    {msg}"));
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
        console,
        executor,
    )
    .await
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
            homepage: None,
            support: None,
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
        let locked = [
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
        let locked = [make_locked_package("psr/log", "3.0.0")];
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
        let locked = [make_locked_package("psr/log", "3.0.1")];
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
        let locked = [make_locked_package("psr/log", "3.0.0")];
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
        let locked = [
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
        let locked = [make_locked_package("Monolog/Monolog", "3.8.0")];
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

    #[test]
    fn test_locked_to_installed_entry_propagates_extra_fields() {
        // Composer's installed.json carries package flags like `abandoned` and
        // `default-branch` that LockedPackage stores in extra_fields. Make sure
        // they survive the conversion so we don't strip them on rewrite.
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");

        let mut pkg = make_locked_package("a/a", "1.0.0");
        pkg.extra_fields.insert(
            "abandoned".to_string(),
            serde_json::Value::String("replacement".to_string()),
        );
        pkg.extra_fields
            .insert("default-branch".to_string(), serde_json::Value::Bool(true));

        let entry = locked_to_installed_entry(&pkg, &vendor_dir);

        assert_eq!(
            entry.extra_fields.get("abandoned"),
            Some(&serde_json::Value::String("replacement".to_string()))
        );
        assert_eq!(
            entry.extra_fields.get("default-branch"),
            Some(&serde_json::Value::Bool(true))
        );
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
    // Platform requirement check tests
    // -----------------------------------------------------------------------

    fn root_with_require(
        require: &[(&str, &str)],
        require_dev: &[(&str, &str)],
    ) -> mozart_core::package::RawPackageData {
        let mut root = mozart_core::package::RawPackageData::new("__root__".to_string());
        for (k, v) in require {
            root.require.insert((*k).to_string(), (*v).to_string());
        }
        for (k, v) in require_dev {
            root.require_dev.insert((*k).to_string(), (*v).to_string());
        }
        root
    }

    fn lock_with_platform(
        platform: serde_json::Value,
        platform_dev: serde_json::Value,
    ) -> lockfile::LockFile {
        let mut lock = minimal_lock(vec![]);
        lock.platform = platform;
        lock.platform_dev = platform_dev;
        lock
    }

    fn pp(name: &str, version: &str) -> mozart_core::platform::PlatformPackage {
        mozart_core::platform::PlatformPackage {
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn combine_platform_requirements_root_overrides_lock() {
        let lock = lock_with_platform(
            serde_json::json!({"php": "^7.4", "ext-foo": "^5"}),
            serde_json::json!({}),
        );
        let root = root_with_require(&[("ext-foo", "^10")], &[]);
        let combined = combine_platform_requirements(&root, &lock, true);

        // Root composer.json wins for ext-foo, lock contributes plain php.
        assert_eq!(combined.get("ext-foo").map(String::as_str), Some("^10"));
        assert_eq!(combined.get("php").map(String::as_str), Some("^7.4"));
    }

    #[test]
    fn combine_platform_requirements_skips_non_platform_requires() {
        let lock = lock_with_platform(serde_json::json!({}), serde_json::json!({}));
        let root = root_with_require(&[("vendor/pkg", "^1.0"), ("php", "^8.0")], &[]);
        let combined = combine_platform_requirements(&root, &lock, true);

        assert_eq!(combined.len(), 1);
        assert_eq!(combined.get("php").map(String::as_str), Some("^8.0"));
    }

    #[test]
    fn combine_platform_requirements_includes_dev_only_when_dev_mode() {
        let lock = lock_with_platform(
            serde_json::json!({}),
            serde_json::json!({"ext-only-dev": "^1"}),
        );
        let root = root_with_require(&[], &[("ext-from-dev-require", "^1")]);

        let with_dev = combine_platform_requirements(&root, &lock, true);
        assert!(with_dev.contains_key("ext-only-dev"));
        assert!(with_dev.contains_key("ext-from-dev-require"));

        let no_dev = combine_platform_requirements(&root, &lock, false);
        assert!(!no_dev.contains_key("ext-only-dev"));
        assert!(!no_dev.contains_key("ext-from-dev-require"));
    }

    #[test]
    fn check_platform_requirements_reports_missing_extension() {
        let combined: BTreeMap<String, String> = [("ext-foo".to_string(), "^10".to_string())]
            .into_iter()
            .collect();
        let platform = vec![pp("php", "8.2.0")];
        let problems = check_platform_requirements_against(&combined, &platform, false, &[]);

        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0],
            "- Root composer.json requires PHP extension ext-foo ^10 but it is missing from your system. Install or enable PHP's foo extension."
        );
    }

    #[test]
    fn check_platform_requirements_reports_unsatisfied_php() {
        let combined: BTreeMap<String, String> = [("php".to_string(), "^20".to_string())]
            .into_iter()
            .collect();
        let platform = vec![pp("php", "8.2.0")];
        let problems = check_platform_requirements_against(&combined, &platform, false, &[]);

        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0],
            "- Root composer.json requires php ^20 but your php version (8.2.0) does not satisfy that requirement."
        );
    }

    #[test]
    fn check_platform_requirements_satisfied_returns_empty() {
        let combined: BTreeMap<String, String> = [("php".to_string(), "^8.0".to_string())]
            .into_iter()
            .collect();
        let platform = vec![pp("php", "8.2.0")];
        let problems = check_platform_requirements_against(&combined, &platform, false, &[]);

        assert!(problems.is_empty());
    }

    #[test]
    fn check_platform_requirements_ignore_platform_reqs_short_circuits() {
        let combined: BTreeMap<String, String> = [("ext-foo".to_string(), "^10".to_string())]
            .into_iter()
            .collect();
        let platform: Vec<mozart_core::platform::PlatformPackage> = vec![];
        let problems = check_platform_requirements_against(&combined, &platform, true, &[]);

        assert!(problems.is_empty());
    }

    #[test]
    fn check_platform_requirements_specific_ignore_filters_named_packages() {
        let combined: BTreeMap<String, String> = [
            ("ext-foo".to_string(), "^10".to_string()),
            ("ext-bar".to_string(), "^10".to_string()),
        ]
        .into_iter()
        .collect();
        let platform = vec![pp("php", "8.2.0")];
        let problems = check_platform_requirements_against(
            &combined,
            &platform,
            false,
            &["ext-foo".to_string()],
        );

        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("ext-bar"));
    }

    #[test]
    fn verify_lock_platform_problems_returns_empty_when_no_reqs() {
        // No platform reqs anywhere → returns empty without invoking detect_platform.
        let lock = lock_with_platform(serde_json::json!({}), serde_json::json!({}));
        let root = root_with_require(&[("vendor/pkg", "^1.0")], &[]);
        let problems = verify_lock_platform_problems(&root, &lock, true, false, &[]);

        assert!(problems.is_empty());
    }
}
