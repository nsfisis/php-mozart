use clap::Args;
use mozart_core::console;
use mozart_core::console_format;
use mozart_registry::installed;
use mozart_registry::installer_executor::{
    ExecuteContext, FilesystemExecutor, InstallerExecutor, PackageOperation,
};
use mozart_registry::lockfile;
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
/// - ops: list of (package, action) ordered topologically — every package's
///   lock-internal `require` deps appear before it, so installs run in
///   dependency-first order to match Composer's `Transaction::calculateOperations`.
/// - removals: list of package names that are installed but not locked
pub fn compute_operations<'a>(
    locked: &[&'a lockfile::LockedPackage],
    installed: &installed::InstalledPackages,
) -> (Vec<(&'a lockfile::LockedPackage, Action)>, Vec<String>) {
    // Topo-sort `locked` so each package's deps (within the lock set) come
    // before it. Composer's solver yields operations in this order via the
    // Transaction; Mozart writes the lock alphabetically, so the install
    // loop must re-order before emitting trace lines or invoking the
    // executor.
    let ordered = topological_sort(locked);

    let mut ops: Vec<(&'a lockfile::LockedPackage, Action)> = Vec::new();
    for pkg in ordered {
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

/// Order a slice of locked packages so every package's `require` deps that
/// are present in the same slice come before it. Mirrors
/// `Composer\DependencyResolver\Transaction::calculateOperations` — the
/// stack-based DFS over the result map.
///
/// Three parity points worth keeping in sync with Composer:
///
/// - The result map is uasort-ed with `strcmp($b, $a)` (reverse alphabetical).
///   Mozart's lock is alphabetical, so we pre-sort reverse to match.
/// - `getProvidersInResult` returns every package in the result whose name
///   *or* `provide`/`replace` entry matches a `require` link target. We
///   build a multimap keyed by all three so a `require: x/y` push covers
///   all packages that resolve `x/y`.
/// - The DFS uses an explicit LIFO stack (Composer's `array_pop`). On first
///   visit the package is re-pushed and its requires are pushed afterwards,
///   so dep traversal order is the **reverse** of `getRequires` iteration.
///
/// Cycles fall back to input order (Composer rejects cycles earlier; this
/// branch should not normally fire).
fn topological_sort<'a>(
    packages: &[&'a lockfile::LockedPackage],
) -> Vec<&'a lockfile::LockedPackage> {
    use std::collections::BTreeMap;

    // Reverse-alphabetical sort, mirroring `setResultPackageMaps`.
    let mut sorted: Vec<&'a lockfile::LockedPackage> = packages.to_vec();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.name.to_lowercase()));

    // Multimap: `name -> [packages]`. A package contributes itself under its
    // own name *and* under every `provide`/`replace` entry. The vec values
    // stay in `sorted` (reverse-alphabetical) order, mirroring Composer's
    // `resultPackagesByName` after its uasort.
    let mut resolves: BTreeMap<String, Vec<&'a lockfile::LockedPackage>> = BTreeMap::new();
    for pkg in &sorted {
        let names = std::iter::once(pkg.name.to_lowercase())
            .chain(pkg.provide.keys().map(|s| s.to_lowercase()))
            .chain(pkg.replace.keys().map(|s| s.to_lowercase()));
        for n in names {
            resolves.entry(n).or_default().push(*pkg);
        }
    }

    // Identify root packages: those not pulled in by any other package's
    // requires (counting provides/replaces as a match).
    let mut required_by_others: HashSet<String> = HashSet::new();
    for pkg in &sorted {
        let pkg_lower = pkg.name.to_lowercase();
        for dep in pkg.require.keys() {
            let dep_lower = dep.to_lowercase();
            if let Some(matches) = resolves.get(&dep_lower) {
                for &m in matches {
                    let m_lower = m.name.to_lowercase();
                    if m_lower != pkg_lower {
                        required_by_others.insert(m_lower);
                    }
                }
            }
        }
    }

    let mut stack: Vec<&'a lockfile::LockedPackage> = sorted
        .iter()
        .filter(|p| !required_by_others.contains(&p.name.to_lowercase()))
        .copied()
        .collect();

    let mut visited: HashSet<String> = HashSet::new();
    let mut processed: HashSet<String> = HashSet::new();
    let mut ordered: Vec<&'a lockfile::LockedPackage> = Vec::with_capacity(packages.len());

    while let Some(pkg) = stack.pop() {
        let lower = pkg.name.to_lowercase();
        if processed.contains(&lower) {
            continue;
        }
        if !visited.contains(&lower) {
            visited.insert(lower);
            // Re-push self so it's processed after its requires drain.
            stack.push(pkg);
            for dep in pkg.require.keys() {
                let dep_lower = dep.to_lowercase();
                if let Some(matches) = resolves.get(&dep_lower) {
                    for &m in matches {
                        stack.push(m);
                    }
                }
            }
        } else {
            processed.insert(lower);
            ordered.push(pkg);
        }
    }

    // Cycle / disconnected fallback: append any leftover packages in the
    // input order so the function is total.
    for pkg in packages {
        let lower = pkg.name.to_lowercase();
        if !processed.contains(&lower) {
            processed.insert(lower);
            ordered.push(*pkg);
        }
    }

    ordered
}

/// Convert a LockedPackage to an InstalledPackageEntry.
///
/// `LockedPackage::extra_fields` is forwarded verbatim so flags like
/// `abandoned` and `default-branch` survive the lock → installed.json round
/// trip, matching Composer's `InstalledFilesystemRepository::write()` (which
/// dumps the full package via `ArrayDumper`).
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
        extra_fields: pkg.extra_fields.clone(),
    }
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
fn collect_install_platform_problems(
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
        if is_platform_package(&lower) {
            combined.insert(lower, constraint.clone());
        }
    }
    if dev_mode {
        for (name, constraint) in &root.require_dev {
            let lower = name.to_lowercase();
            if is_platform_package(&lower) {
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

    let ignored: HashSet<String> = ignore_platform_req
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

    let ignored_set: HashSet<String> = ignore_platform_req
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    for pkg in packages {
        for (req_name, req_constraint) in &pkg.require {
            if is_platform_package(req_name) {
                let lower = req_name.to_lowercase();
                if !ignored_set.contains(&lower) {
                    console.info(&format!(
                        "{}",
                        console::warning(&format!(
                            "Platform requirement {req_name} {req_constraint} (required by {}) \
                             has not been verified. Platform detection is not yet fully implemented.",
                            pkg.name
                        ))
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
            let from_version = installed
                .packages
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| p.version.as_str())
                .unwrap_or("");
            executor.uninstall_package(name, from_version, &exec_ctx)?;
        }

        if !removals.is_empty() {
            executor.cleanup_after_uninstalls(&exec_ctx)?;
        }

        for (pkg, action) in &ops {
            let op = match action {
                Action::Skip => continue,
                Action::Install => {
                    console.info(&console_format!(
                        "  - Installing <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                    PackageOperation::Install { package: pkg }
                }
                Action::Update => {
                    console.info(&console_format!(
                        "  - Upgrading <info>{}</info> (<comment>{}</comment>)",
                        pkg.name,
                        pkg.version
                    ));
                    // Pull the previously-installed version from installed.json
                    // so the trace recorder can format
                    // `Upgrading pkg (oldVersion => newVersion)`.
                    let from_version = installed
                        .packages
                        .iter()
                        .find(|p| p.name.eq_ignore_ascii_case(&pkg.name))
                        .map(|p| p.version.as_str())
                        .unwrap_or("");
                    PackageOperation::Update {
                        from_version,
                        package: pkg,
                    }
                }
            };
            executor.install_package(op, &exec_ctx).await?;
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
    let working_dir = resolve_working_dir(cli);
    run(&working_dir, args, console, repositories, &mut executor).await
}

/// Library entry point — pure logic, no `Cli` access.
///
/// In-process tests construct an empty `RepositorySet` (Composer's
/// `'packagist' => false` test config) and a tracing `InstallerExecutor`,
/// then call this function directly to exercise the install flow without
/// spawning the binary.
pub async fn run(
    working_dir: &Path,
    args: &InstallArgs,
    console: &mozart_core::console::Console,
    repositories: std::sync::Arc<mozart_registry::repository::RepositorySet>,
    executor: &mut dyn InstallerExecutor,
) -> anyhow::Result<()> {
    // Step 2: Validate arguments
    if args.prefer_install.is_some() && (args.prefer_source || args.prefer_dist) {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            "The --prefer-install option cannot be used together with --prefer-source or --prefer-dist.",
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

    if args.dev {
        console.info(&console_format!(
            "<warning>The --dev option is deprecated. Dev packages are installed by default.</warning>"
        ));
    }

    if args.no_suggest {
        console.info(&console_format!(
            "<warning>The --no-suggest option is deprecated and has no effect.</warning>"
        ));
    }

    // Step 3: Read composer.lock
    // If no lock file present, fall back to update (matching Composer behavior).
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        console.info(&console_format!(
            "<warning>No composer.lock file present. Updating dependencies to latest instead of installing from lock file.</warning>"
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
        return super::update::run(working_dir, &update_args, console, repositories, executor)
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
            return Err(mozart_core::exit_code::bail_silent(
                mozart_core::exit_code::LOCK_FILE_INVALID,
            ));
        }

        let platform_problems = collect_install_platform_problems(
            &root_pkg,
            &lock,
            dev_mode,
            args.ignore_platform_reqs,
            &args.ignore_platform_req,
        );
        if !platform_problems.is_empty() {
            console.info(
                "Your lock file does not contain a compatible set of packages. Please run composer update.",
            );
            console.info("");
            for (i, msg) in platform_problems.iter().enumerate() {
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
            optimize_autoloader: args.optimize_autoloader,
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
    fn collect_install_platform_problems_returns_empty_when_no_reqs() {
        // No platform reqs anywhere → returns empty without invoking detect_platform.
        let lock = lock_with_platform(serde_json::json!({}), serde_json::json!({}));
        let root = root_with_require(&[("vendor/pkg", "^1.0")], &[]);
        let problems = collect_install_platform_problems(&root, &lock, true, false, &[]);

        assert!(problems.is_empty());
    }
}
