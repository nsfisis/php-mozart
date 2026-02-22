use clap::Args;
use mozart_core::console;
use mozart_core::console_format;
use mozart_core::package::{self, Stability};
use mozart_registry::lockfile;
use mozart_registry::resolver::{self, PlatformConfig, ResolveRequest, ResolvedPackage};
use std::collections::{HashMap, HashSet};

#[derive(Args)]
pub struct UpdateArgs {
    /// Package(s) to update
    pub packages: Vec<String>,

    /// Temporary version constraint overrides
    #[arg(long)]
    pub with: Vec<String>,

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

    /// [Deprecated] Enables installation of require-dev packages
    #[arg(long)]
    pub dev: bool,

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Only updates the lock file hash
    #[arg(long)]
    pub lock: bool,

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

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Update also dependencies of packages in the argument list
    #[arg(short = 'w', long)]
    pub with_dependencies: bool,

    /// Update also all dependencies including root requirements
    #[arg(short = 'W', long)]
    pub with_all_dependencies: bool,

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

    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,

    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Only allow patch version updates
    #[arg(long)]
    pub patch_only: bool,

    /// Interactive package selection
    #[arg(short, long)]
    pub interactive: bool,

    /// Only update packages that are root requirements
    #[arg(long)]
    pub root_reqs: bool,

    /// Bump version constraints after update (dev, no-dev, all)
    #[arg(long)]
    pub bump_after_update: Option<Option<String>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Change tracking types
// ─────────────────────────────────────────────────────────────────────────────

/// The kind of change for a package during update.
#[derive(Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Install {
        new_version: String,
    },
    Update {
        old_version: String,
        new_version: String,
    },
    Remove {
        old_version: String,
    },
    Unchanged,
}

/// A single package change entry computed during update.
#[derive(Debug)]
pub struct UpdateChange {
    pub name: String,
    pub kind: ChangeKind,
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: parse minimum-stability string
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a minimum-stability string from composer.json into a `Stability` enum value.
///
/// Recognizes "stable", "RC", "beta", "alpha", "dev" (case-insensitive).
/// Defaults to `Stability::Stable` for unrecognized values.
fn parse_minimum_stability(s: &str) -> Stability {
    package::Stability::parse(s)
}

/// Check whether a package name refers to a platform package (php, ext-*, lib-*, composer-*).
fn is_platform_package(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "php"
        || lower.starts_with("ext-")
        || lower.starts_with("lib-")
        || lower.starts_with("composer-")
        || lower == "composer"
        || lower == "composer-runtime-api"
        || lower == "composer-plugin-api"
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: compute changes between old and new lock
// ─────────────────────────────────────────────────────────────────────────────

/// Compare old lock vs new lock to determine installs, updates, removals, and unchanged packages.
///
/// Produces one `UpdateChange` per affected package. Packages that are identical in both
/// lock files are omitted (ChangeKind::Unchanged) from the returned vec — callers that
/// want the full list should filter as needed.
pub fn compute_update_changes(
    old_lock: Option<&lockfile::LockFile>,
    new_lock: &lockfile::LockFile,
    dev_mode: bool,
) -> Vec<UpdateChange> {
    // Build map of old lock packages keyed by lowercase name -> version string
    let mut old_map: HashMap<String, String> = HashMap::new();
    if let Some(old) = old_lock {
        for pkg in &old.packages {
            old_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
        }
        if dev_mode && let Some(ref dev_pkgs) = old.packages_dev {
            for pkg in dev_pkgs {
                old_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
            }
        }
    }

    // Build map of new lock packages keyed by lowercase name -> version string
    let mut new_map: HashMap<String, String> = HashMap::new();
    for pkg in &new_lock.packages {
        new_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
    }
    if dev_mode && let Some(ref dev_pkgs) = new_lock.packages_dev {
        for pkg in dev_pkgs {
            new_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
        }
    }

    let mut changes: Vec<UpdateChange> = Vec::new();

    // Check all packages in the new lock
    for (name, new_version) in &new_map {
        let kind = if let Some(old_version) = old_map.get(name) {
            if old_version == new_version {
                ChangeKind::Unchanged
            } else {
                ChangeKind::Update {
                    old_version: old_version.clone(),
                    new_version: new_version.clone(),
                }
            }
        } else {
            ChangeKind::Install {
                new_version: new_version.clone(),
            }
        };

        if !matches!(kind, ChangeKind::Unchanged) {
            changes.push(UpdateChange {
                name: name.clone(),
                kind,
            });
        }
    }

    // Check packages in the old lock that are missing from the new lock (removals)
    for (name, old_version) in &old_map {
        if !new_map.contains_key(name) {
            changes.push(UpdateChange {
                name: name.clone(),
                kind: ChangeKind::Remove {
                    old_version: old_version.clone(),
                },
            });
        }
    }

    changes
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: apply partial update filter
// ─────────────────────────────────────────────────────────────────────────────

/// For a partial update (when specific packages are named on the CLI), swap back
/// the versions of packages that were NOT requested to be updated.
///
/// This implements the simplified approach (plan approach c):
/// - For packages in `update_packages`: use the newly resolved version.
/// - For packages NOT in `update_packages`: if the old lock has them at a different
///   version, keep the old locked version. This prevents unintended upgrades.
///
/// Note: This is a best-effort approach. In some cases the resolver may pick
/// incompatible versions; a future phase can add true pinning via resolver constraints.
pub fn apply_partial_update(
    resolved: Vec<ResolvedPackage>,
    old_lock: &lockfile::LockFile,
    update_packages: &[String],
) -> Vec<ResolvedPackage> {
    // Build a set of normalized package names we want to update
    let update_set: std::collections::HashSet<String> =
        update_packages.iter().map(|s| s.to_lowercase()).collect();

    // Build a map of old locked packages by name -> (version, version_normalized, is_dev)
    let mut old_pkg_map: HashMap<String, &lockfile::LockedPackage> = HashMap::new();
    for pkg in &old_lock.packages {
        old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
    }
    if let Some(ref dev_pkgs) = old_lock.packages_dev {
        for pkg in dev_pkgs {
            old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
        }
    }

    resolved
        .into_iter()
        .map(|mut pkg| {
            let name_lower = pkg.name.to_lowercase();
            // If this package is NOT in the update set and we have an old locked version,
            // swap it back to the old version to prevent unintended changes.
            if !update_set.contains(&name_lower)
                && let Some(old_pkg) = old_pkg_map.get(&name_lower)
            {
                pkg.version = old_pkg.version.clone();
                pkg.version_normalized = old_pkg
                    .version_normalized
                    .clone()
                    .unwrap_or_else(|| old_pkg.version.clone());
                pkg.is_dev = false; // preserve existing; lock file doesn't store this flag directly
            }
            pkg
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Wildcard expansion helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Match a single package name against a glob pattern.
///
/// Only the `*` wildcard is supported (matches any sequence of non-`/` characters
/// within a segment, or any characters when the pattern contains no `/`).
/// Examples:
///   - `symfony/*`      matches `symfony/console`, `symfony/http-kernel`
///   - `monolog/mono*`  matches `monolog/monolog`
///   - `psr/*`          matches `psr/log`, `psr/container`
fn glob_matches(pattern: &str, name: &str) -> bool {
    // Fast path: no wildcard
    if !pattern.contains('*') {
        return pattern.eq_ignore_ascii_case(name);
    }
    // Split both pattern and name on '/' and match segment-by-segment
    let pat_parts: Vec<&str> = pattern.splitn(2, '/').collect();
    let name_parts: Vec<&str> = name.splitn(2, '/').collect();

    // Both must have the same number of segments (vendor/name vs vendor/name)
    if pat_parts.len() != name_parts.len() {
        return false;
    }
    for (pp, np) in pat_parts.iter().zip(name_parts.iter()) {
        if !glob_segment_matches(pp, np) {
            return false;
        }
    }
    true
}

/// Match a single path segment against a pattern segment (no '/' involved).
/// `*` matches any sequence of characters (including empty).
fn glob_segment_matches(pattern: &str, text: &str) -> bool {
    // Simple recursive matcher
    let pat = pattern.to_lowercase();
    let txt = text.to_lowercase();
    glob_segment_matches_inner(pat.as_bytes(), txt.as_bytes())
}

fn glob_segment_matches_inner(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(&b'*'), _) => {
            // '*' can match zero or more characters
            // Try consuming zero chars, or consuming one char from text
            glob_segment_matches_inner(&pattern[1..], text)
                || (!text.is_empty() && glob_segment_matches_inner(pattern, &text[1..]))
        }
        (Some(p), Some(t)) if p == t => glob_segment_matches_inner(&pattern[1..], &text[1..]),
        _ => false,
    }
}

/// Expand a list of package specifiers (which may include wildcards) against
/// all packages in the lock file, returning the resolved concrete package names.
///
/// Non-wildcard specifiers are passed through unchanged (even if not in the lock,
/// so the resolver can report the error naturally).
pub fn expand_wildcards(specifiers: &[String], lock: &lockfile::LockFile) -> Vec<String> {
    // Collect all locked package names (prod + dev)
    let all_names: Vec<String> = lock
        .packages
        .iter()
        .map(|p| p.name.to_lowercase())
        .chain(
            lock.packages_dev
                .iter()
                .flatten()
                .map(|p| p.name.to_lowercase()),
        )
        .collect();

    let mut result: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for spec in specifiers {
        if spec.contains('*') {
            // Expand the wildcard against the lock
            let mut matched = false;
            for name in &all_names {
                if glob_matches(spec, name) && seen.insert(name.clone()) {
                    result.push(name.clone());
                    matched = true;
                }
            }
            if !matched {
                eprintln!(
                    "{}",
                    console::warning(&format!(
                        "No locked packages matched the pattern '{}'. Pattern will be ignored.",
                        spec
                    ))
                );
            }
        } else {
            let lower = spec.to_lowercase();
            if seen.insert(lower.clone()) {
                result.push(lower);
            }
        }
    }

    result
}

// ─────────────────────────────────────────────────────────────────────────────
// Dependency expansion helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a lookup map from package name (lowercase) to its LockedPackage.
fn build_lock_map(lock: &lockfile::LockFile) -> HashMap<String, &lockfile::LockedPackage> {
    let mut map = HashMap::new();
    for pkg in &lock.packages {
        map.insert(pkg.name.to_lowercase(), pkg);
    }
    if let Some(ref dev_pkgs) = lock.packages_dev {
        for pkg in dev_pkgs {
            map.insert(pkg.name.to_lowercase(), pkg);
        }
    }
    map
}

/// Given a set of package names, add their direct `require` dependencies from
/// the lock file to the set.  Returns the augmented set.
pub fn expand_with_direct_dependencies(
    packages: Vec<String>,
    lock: &lockfile::LockFile,
) -> Vec<String> {
    let lock_map = build_lock_map(lock);
    let mut result_set: HashSet<String> = packages.iter().cloned().collect();
    let mut result: Vec<String> = packages;

    for name in result.clone() {
        if let Some(pkg) = lock_map.get(&name) {
            for dep_name in pkg.require.keys() {
                // Skip platform packages (php, ext-*, lib-*)
                if dep_name == "php"
                    || dep_name.starts_with("ext-")
                    || dep_name.starts_with("lib-")
                    || dep_name == "php-64bit"
                    || dep_name == "php-ipv6"
                    || dep_name == "php-zts"
                    || dep_name == "php-debug"
                {
                    continue;
                }
                let lower = dep_name.to_lowercase();
                if result_set.insert(lower.clone()) {
                    result.push(lower);
                }
            }
        }
    }

    result
}

/// Given a set of package names, recursively expand their full transitive
/// `require` dependency tree from the lock file.
pub fn expand_with_all_dependencies(
    packages: Vec<String>,
    lock: &lockfile::LockFile,
) -> Vec<String> {
    let lock_map = build_lock_map(lock);
    let mut result_set: HashSet<String> = packages.iter().cloned().collect();
    let mut queue: Vec<String> = packages.clone();
    let mut result: Vec<String> = packages;

    while let Some(name) = queue.pop() {
        if let Some(pkg) = lock_map.get(&name) {
            for dep_name in pkg.require.keys() {
                // Skip platform packages
                if dep_name == "php"
                    || dep_name.starts_with("ext-")
                    || dep_name.starts_with("lib-")
                    || dep_name == "php-64bit"
                    || dep_name == "php-ipv6"
                    || dep_name == "php-zts"
                    || dep_name == "php-debug"
                {
                    continue;
                }
                let lower = dep_name.to_lowercase();
                if result_set.insert(lower.clone()) {
                    result.push(lower.clone());
                    queue.push(lower);
                }
            }
        }
    }

    result
}

/// Expand the package list applying wildcard matching and optional dependency expansion.
///
/// Returns the final list of package names to update (concrete, lowercase, deduplicated).
pub fn expand_packages(
    specifiers: &[String],
    lock: Option<&lockfile::LockFile>,
    with_dependencies: bool,
    with_all_dependencies: bool,
) -> Vec<String> {
    // First expand wildcards (requires a lock file)
    let mut packages: Vec<String> = if let Some(lock) = lock {
        expand_wildcards(specifiers, lock)
    } else {
        // No lock file: pass through as-is (no wildcards can be resolved)
        specifiers.iter().map(|s| s.to_lowercase()).collect()
    };

    // Then expand dependencies if requested
    if let Some(lock) = lock {
        if with_all_dependencies {
            packages = expand_with_all_dependencies(packages, lock);
        } else if with_dependencies {
            packages = expand_with_direct_dependencies(packages, lock);
        }
    }

    packages
}

// ─────────────────────────────────────────────────────────────────────────────
// Interactive selection helper
// ─────────────────────────────────────────────────────────────────────────────

/// Interactively prompt the user to select which packages to update.
///
/// For each package in `packages`, prints a y/n prompt and collects the
/// user's response.  Returns only the packages the user confirmed.
///
/// When stdin is not a TTY (e.g. in CI or piped input), emits a warning and
/// returns the full package list unchanged.
pub fn interactive_select_packages(packages: Vec<String>) -> Vec<String> {
    use std::io::{self, BufRead, IsTerminal, Write};

    let stdin = io::stdin();
    if !stdin.is_terminal() {
        eprintln!(
            "{}",
            console::warning(
                "Interactive mode requires a TTY. Running non-interactively with all packages."
            )
        );
        return packages;
    }

    eprintln!("Select packages to update (y/n for each):");

    let mut selected = Vec::new();
    let stdin_locked = stdin.lock();
    let mut lines = stdin_locked.lines();

    for pkg in &packages {
        loop {
            eprint!("  Update {}? [y/n] ", pkg);
            let _ = io::stderr().flush();

            match lines.next() {
                Some(Ok(line)) => {
                    let answer = line.trim().to_lowercase();
                    match answer.as_str() {
                        "y" | "yes" => {
                            selected.push(pkg.clone());
                            break;
                        }
                        "n" | "no" => {
                            break;
                        }
                        _ => {
                            eprintln!("  Please answer y or n.");
                        }
                    }
                }
                _ => {
                    // EOF or error: treat as "no"
                    break;
                }
            }
        }
    }

    selected
}

// ─────────────────────────────────────────────────────────────────────────────
// Minimal-changes helper
// ─────────────────────────────────────────────────────────────────────────────

/// For `--minimal-changes` mode: when no specific packages are named, pin all
/// packages to their current locked version UNLESS the current locked version
/// no longer satisfies the root constraint.  This prevents pulling in newer
/// versions of packages that don't need updating.
///
/// When specific packages ARE named, `apply_partial_update` already handles
/// pinning non-requested packages, so this function is a no-op in that case.
///
/// Implementation: We add the locked version back for every package that is NOT
/// newly required (i.e., already exists in the lock with a version that is still
/// satisfiable). In practice this is expressed by running `apply_partial_update`
/// with an empty update set — which pins *everything* — and then releasing the
/// pins for packages whose constraints have changed or that are new.
///
/// For the initial implementation we take a simpler approach: we call
/// `apply_partial_update` with an empty update list so that all packages are
/// pinned to their old locked versions.  The resolver will still produce a valid
/// solution; we then override with locked versions for packages not explicitly
/// listed.
pub fn apply_minimal_changes(
    resolved: Vec<ResolvedPackage>,
    old_lock: &lockfile::LockFile,
) -> Vec<ResolvedPackage> {
    // Pin every package to its old locked version (full pin, no updates)
    apply_partial_update(resolved, old_lock, &[])
}

/// Filter resolved packages to only allow patch-level version changes.
///
/// For each resolved package, if the old lock has a version with the same
/// major.minor, the upgrade is allowed. Otherwise the package is pinned
/// back to its old locked version.
pub fn apply_patch_only(
    resolved: Vec<ResolvedPackage>,
    old_lock: &lockfile::LockFile,
) -> Vec<ResolvedPackage> {
    let mut old_pkg_map: HashMap<String, &lockfile::LockedPackage> = HashMap::new();
    for pkg in &old_lock.packages {
        old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
    }
    if let Some(ref dev_pkgs) = old_lock.packages_dev {
        for pkg in dev_pkgs {
            old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
        }
    }

    resolved
        .into_iter()
        .map(|mut pkg| {
            let name_lower = pkg.name.to_lowercase();
            if let Some(old_pkg) = old_pkg_map.get(&name_lower) {
                let old_norm = old_pkg
                    .version_normalized
                    .as_deref()
                    .unwrap_or(&old_pkg.version);
                let new_norm = &pkg.version_normalized;

                // Compare major.minor: if they differ, pin to old version
                let old_mm = major_minor(old_norm);
                let new_mm = major_minor(new_norm);
                if old_mm != new_mm {
                    pkg.version = old_pkg.version.clone();
                    pkg.version_normalized = old_norm.to_string();
                }
            }
            pkg
        })
        .collect()
}

/// Extract (major, minor) from a normalized version string.
fn major_minor(version: &str) -> (u64, u64) {
    let parts: Vec<&str> = version.split('.').collect();
    let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

// ─────────────────────────────────────────────────────────────────────────────
// Main execute function
// ─────────────────────────────────────────────────────────────────────────────

pub async fn execute(
    args: &UpdateArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // Step 1: Resolve the working directory
    let working_dir = super::install::resolve_working_dir(cli);

    // Step 2: Handle deprecated flags
    if args.dev {
        console.info(&console_format!(
            "<warning>The --dev option is deprecated. Dev packages are updated by default.</warning>"
        ));
    }
    if args.no_suggest {
        console.info(&console_format!(
            "<warning>The --no-suggest option is deprecated and has no effect.</warning>"
        ));
    }

    // --root-reqs: if no packages specified, auto-populate with root requirements
    if args.root_reqs && args.packages.is_empty() {
        console.info("Using root requirements as the update list (--root-reqs).");
    }

    // Step 3: Read composer.json
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            format!(
                "Composer could not find a composer.json file in {}",
                working_dir.display()
            ),
        ));
    }
    let composer_json = package::read_from_file(&composer_json_path)?;
    let composer_json_content = std::fs::read_to_string(&composer_json_path)?;

    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");

    // Step 4: Handle --lock mode (early return)
    if args.lock {
        return handle_lock_mode(&lock_path, &composer_json_content, args.dry_run, console);
    }

    let dev_mode = !args.no_dev;

    // Step 5: Build the resolve request from composer.json
    // Filter out platform packages from require list for the resolver (they're handled separately)
    let require: Vec<(String, String)> = composer_json
        .require
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let require_dev: Vec<(String, String)> = composer_json
        .require_dev
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Parse minimum-stability from composer.json (defaults to "stable")
    let minimum_stability_str = composer_json
        .minimum_stability
        .as_deref()
        .unwrap_or("stable");
    let minimum_stability = parse_minimum_stability(minimum_stability_str);

    // Determine prefer-stable: CLI flag OR composer.json field
    let composer_prefer_stable = composer_json
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let prefer_stable = args.prefer_stable || composer_prefer_stable;

    let request = ResolveRequest {
        root_name: composer_json.name.clone(),
        require,
        require_dev,
        include_dev: dev_mode,
        minimum_stability,
        stability_flags: HashMap::new(),
        prefer_stable,
        prefer_lowest: args.prefer_lowest,
        platform: PlatformConfig::new(),
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repo_cache: None,
    };

    // Step 6: Print header and run resolver
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
            return Err(mozart_core::exit_code::bail(
                mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
                e.to_string(),
            ));
        }
    };

    // Step 7: Read old lock file (for change reporting and partial update)
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

    // Step 8: Expand package list (wildcards + dependency expansion) and handle
    //         interactive selection, then apply partial update logic.
    //
    // Note: wildcard expansion and dependency traversal both require a lock file.
    // If --minimal-changes is requested without specific packages, we pin all packages.
    // --root-reqs: treat root requirements as the package list
    let effective_packages: Vec<String> = if args.root_reqs && args.packages.is_empty() {
        let mut root_pkgs: Vec<String> = composer_json
            .require
            .keys()
            .filter(|k| !is_platform_package(k))
            .map(|k| k.to_lowercase())
            .collect();
        if dev_mode {
            root_pkgs.extend(
                composer_json
                    .require_dev
                    .keys()
                    .filter(|k| !is_platform_package(k))
                    .map(|k| k.to_lowercase()),
            );
        }
        root_pkgs
    } else {
        args.packages.clone()
    };

    let update_packages: Vec<String> = if !effective_packages.is_empty() {
        match &old_lock {
            None => {
                return Err(mozart_core::exit_code::bail(
                    mozart_core::exit_code::NO_LOCK_FILE_FOR_PARTIAL_UPDATE,
                    "No lock file found. Cannot perform partial update. Run `mozart update` first.",
                ));
            }
            Some(lock) => {
                // 1. Expand wildcards
                let mut expanded = expand_packages(
                    &effective_packages,
                    Some(lock),
                    args.with_dependencies,
                    args.with_all_dependencies,
                );

                // 2. Interactive selection (filter the expanded list)
                if args.interactive {
                    expanded = interactive_select_packages(expanded);
                }

                expanded
            }
        }
    } else {
        // No specific packages: full update mode
        // If --interactive, show all locked packages and let user select
        if args.interactive {
            match &old_lock {
                None => {
                    console.info(&console_format!(
                        "<warning>No lock file found. --interactive mode skipped.</warning>"
                    ));
                    vec![]
                }
                Some(lock) => {
                    let all_names: Vec<String> = lock
                        .packages
                        .iter()
                        .map(|p| p.name.to_lowercase())
                        .chain(
                            lock.packages_dev
                                .iter()
                                .flatten()
                                .map(|p| p.name.to_lowercase()),
                        )
                        .collect();
                    interactive_select_packages(all_names)
                }
            }
        } else {
            vec![]
        }
    };

    // Apply partial update (pin non-requested packages) when a subset was named
    if !update_packages.is_empty() {
        match &old_lock {
            None => {
                return Err(mozart_core::exit_code::bail(
                    mozart_core::exit_code::NO_LOCK_FILE_FOR_PARTIAL_UPDATE,
                    "No lock file found. Cannot perform partial update. Run `mozart update` first.",
                ));
            }
            Some(lock) => {
                resolved = apply_partial_update(resolved, lock, &update_packages);
            }
        }
    } else if args.minimal_changes && update_packages.is_empty() {
        // Full update with --minimal-changes: pin everything to locked versions
        // (only updates packages whose constraints have changed in composer.json)
        if let Some(ref lock) = old_lock {
            console.info("Minimal changes mode: preserving locked versions where possible.");
            resolved = apply_minimal_changes(resolved, lock);
        }
    }

    // Apply --patch-only filter: restrict updates to patch-level changes only
    if args.patch_only
        && let Some(ref lock) = old_lock
    {
        console.info("Patch-only mode: restricting updates to patch-level changes.");
        resolved = apply_patch_only(resolved, lock);
    }

    // Step 9: Generate new lock file
    let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content: composer_json_content.clone(),
        composer_json: composer_json.clone(),
        include_dev: dev_mode,
        repo_cache: None,
    })
    .await?;

    // Step 10: Compute and print change report
    let changes = compute_update_changes(old_lock.as_ref(), &new_lock, dev_mode);

    let installs: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Install { .. }))
        .collect();
    let updates: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Update { .. }))
        .collect();
    let removals: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Remove { .. }))
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

    // Print individual change lines
    let prefix = if args.dry_run { "Would" } else { "" };
    for change in &changes {
        match &change.kind {
            ChangeKind::Remove { old_version } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - {} remove {} ({})",
                        prefix, change.name, old_version
                    ));
                } else {
                    console.info(&format!("  - Removing {} ({})", change.name, old_version));
                }
            }
            ChangeKind::Install { new_version } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - {} install {} ({})",
                        prefix, change.name, new_version
                    ));
                } else {
                    console.info(&format!("  - Installing {} ({})", change.name, new_version));
                }
            }
            ChangeKind::Update {
                old_version,
                new_version,
            } => {
                if args.dry_run {
                    console.info(&format!(
                        "  - {} update {} ({} => {})",
                        prefix, change.name, old_version, new_version
                    ));
                } else {
                    console.info(&format!(
                        "  - Updating {} ({} => {})",
                        change.name, old_version, new_version
                    ));
                }
            }
            ChangeKind::Unchanged => {}
        }
    }

    // Step 11: Write lock file (unless --dry-run)
    if !args.dry_run {
        console.info("Writing lock file");
        new_lock.write_to_file(&lock_path)?;
    }

    // Step 11b: Bump composer.json constraints if --bump-after-update
    if let Some(ref bump_mode) = args.bump_after_update
        && !args.dry_run
    {
        let mode = bump_mode.as_deref().unwrap_or("all");
        let bump_require = mode == "all" || mode == "no-dev";
        let bump_require_dev = mode == "all" || mode == "dev";

        // Build locked versions map from the new lock
        let mut locked_versions: HashMap<String, (String, Option<String>)> = HashMap::new();
        for pkg in &new_lock.packages {
            locked_versions.insert(
                pkg.name.to_lowercase(),
                (pkg.version.clone(), pkg.version_normalized.clone()),
            );
        }
        if let Some(ref dev_pkgs) = new_lock.packages_dev {
            for pkg in dev_pkgs {
                locked_versions.insert(
                    pkg.name.to_lowercase(),
                    (pkg.version.clone(), pkg.version_normalized.clone()),
                );
            }
        }

        let mut bumped = 0u32;
        let mut root = composer_json.clone();

        if bump_require {
            for (pkg_name, constraint) in &composer_json.require {
                if is_platform_package(pkg_name) {
                    continue;
                }
                if let Some((pretty_version, version_normalized)) =
                    locked_versions.get(&pkg_name.to_lowercase())
                    && let Some(new_constraint) = mozart_core::version_bumper::bump_requirement(
                        constraint,
                        pretty_version,
                        version_normalized.as_deref(),
                    )
                {
                    console.info(&format!(
                        "  Bumping {}: {} => {}",
                        pkg_name, constraint, new_constraint
                    ));
                    root.require.insert(pkg_name.clone(), new_constraint);
                    bumped += 1;
                }
            }
        }

        if bump_require_dev {
            for (pkg_name, constraint) in &composer_json.require_dev {
                if is_platform_package(pkg_name) {
                    continue;
                }
                if let Some((pretty_version, version_normalized)) =
                    locked_versions.get(&pkg_name.to_lowercase())
                    && let Some(new_constraint) = mozart_core::version_bumper::bump_requirement(
                        constraint,
                        pretty_version,
                        version_normalized.as_deref(),
                    )
                {
                    console.info(&format!(
                        "  Bumping {}: {} => {}",
                        pkg_name, constraint, new_constraint
                    ));
                    root.require_dev.insert(pkg_name.clone(), new_constraint);
                    bumped += 1;
                }
            }
        }

        if bumped > 0 {
            package::write_to_file(&root, &composer_json_path)?;

            // Update lock file content-hash to match the new composer.json
            let new_content = std::fs::read_to_string(&composer_json_path)?;
            let new_hash = lockfile::LockFile::compute_content_hash(&new_content)?;
            let mut updated_lock = new_lock.clone();
            updated_lock.content_hash = new_hash;
            updated_lock.write_to_file(&lock_path)?;

            console.info(&format!(
                "{} has been updated ({bumped} changes).",
                composer_json_path.display()
            ));
        }
    }

    // Step 12: Install packages (unless --no-install or --dry-run)
    if !args.no_install && !args.dry_run {
        // Warn about prefer-source (not yet supported)
        let prefer_source = args.prefer_source
            || args
                .prefer_install
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("source"))
                .unwrap_or(false);
        if prefer_source {
            console.info(&console_format!(
                "<warning>Warning: Source installs are not yet supported. Falling back to dist.</warning>"
            ));
        }

        super::install::install_from_lock(
            &new_lock,
            &working_dir,
            &vendor_dir,
            &super::install::InstallConfig {
                dev_mode,
                dry_run: false, // dry_run already checked above
                no_autoloader: args.no_autoloader,
                no_progress: args.no_progress,
                ignore_platform_reqs: args.ignore_platform_reqs,
                ignore_platform_req: args.ignore_platform_req.clone(),
                optimize_autoloader: args.optimize_autoloader,
                classmap_authoritative: args.classmap_authoritative,
                apcu_autoloader: false,
                apcu_autoloader_prefix: None,
            },
        )
        .await?;
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// --lock mode handler
// ─────────────────────────────────────────────────────────────────────────────

/// Handle the `--lock` mode: refresh the content-hash of the existing lock file.
///
/// Reads the existing composer.lock, computes the new content-hash from the current
/// composer.json, and writes the updated lock file back to disk if the hash differs.
fn handle_lock_mode(
    lock_path: &std::path::Path,
    composer_json_content: &str,
    dry_run: bool,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    if !lock_path.exists() {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::LOCK_FILE_INVALID,
            "No lock file found. Run `mozart update` to generate one.",
        ));
    }

    let mut lock = lockfile::LockFile::read_from_file(lock_path)?;

    let new_hash = lockfile::LockFile::compute_content_hash(composer_json_content)?;

    if new_hash == lock.content_hash {
        console.info("Lock file is already up to date");
        return Ok(());
    }

    lock.content_hash = new_hash;

    if !dry_run {
        lock.write_to_file(lock_path)?;
        console.info("Lock file hash updated successfully.");
    } else {
        console.info("Would update lock file hash.");
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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

    fn make_resolved_package(name: &str, version: &str) -> ResolvedPackage {
        ResolvedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: format!("{}.0", version),
            is_dev: false,
        }
    }

    // ──────────── parse_minimum_stability ────────────

    #[test]
    fn test_parse_minimum_stability_stable() {
        assert_eq!(parse_minimum_stability("stable"), Stability::Stable);
        assert_eq!(parse_minimum_stability("STABLE"), Stability::Stable);
        assert_eq!(parse_minimum_stability("Stable"), Stability::Stable);
    }

    #[test]
    fn test_parse_minimum_stability_rc() {
        assert_eq!(parse_minimum_stability("RC"), Stability::RC);
        assert_eq!(parse_minimum_stability("rc"), Stability::RC);
    }

    #[test]
    fn test_parse_minimum_stability_beta() {
        assert_eq!(parse_minimum_stability("beta"), Stability::Beta);
        assert_eq!(parse_minimum_stability("BETA"), Stability::Beta);
    }

    #[test]
    fn test_parse_minimum_stability_alpha() {
        assert_eq!(parse_minimum_stability("alpha"), Stability::Alpha);
        assert_eq!(parse_minimum_stability("ALPHA"), Stability::Alpha);
    }

    #[test]
    fn test_parse_minimum_stability_dev() {
        assert_eq!(parse_minimum_stability("dev"), Stability::Dev);
        assert_eq!(parse_minimum_stability("DEV"), Stability::Dev);
    }

    #[test]
    fn test_parse_minimum_stability_unknown_defaults_to_stable() {
        assert_eq!(parse_minimum_stability("unknown"), Stability::Stable);
        assert_eq!(parse_minimum_stability(""), Stability::Stable);
    }

    // ──────────── compute_update_changes ────────────

    #[test]
    fn test_compute_update_changes_all_new() {
        // No old lock: all packages in new lock should be Install
        let new_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);

        let changes = compute_update_changes(None, &new_lock, false);

        assert_eq!(changes.len(), 2);
        for change in &changes {
            assert!(
                matches!(change.kind, ChangeKind::Install { .. }),
                "Expected Install, got {:?} for {}",
                change.kind,
                change.name
            );
        }
    }

    #[test]
    fn test_compute_update_changes_update() {
        // Old lock has psr/log at 3.0.0; new lock has it at 3.0.1 -> Update
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.1")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "psr/log");
        assert!(matches!(
            &changes[0].kind,
            ChangeKind::Update {
                old_version,
                new_version
            } if old_version == "3.0.0" && new_version == "3.0.1"
        ));
    }

    #[test]
    fn test_compute_update_changes_remove() {
        // Old lock has monolog; new lock doesn't -> Remove
        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "monolog/monolog");
        assert!(matches!(
            &changes[0].kind,
            ChangeKind::Remove { old_version } if old_version == "3.8.0"
        ));
    }

    #[test]
    fn test_compute_update_changes_unchanged_not_in_result() {
        // Same version in both locks -> no changes
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        assert!(
            changes.is_empty(),
            "Unchanged packages should not appear in changes list"
        );
    }

    #[test]
    fn test_compute_update_changes_mixed() {
        // Mixed scenario: install, update, remove, unchanged
        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),         // unchanged
            make_locked_package("monolog/monolog", "3.7.0"), // will be updated
            make_locked_package("old/package", "1.0.0"),     // will be removed
        ]);
        let new_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),         // unchanged
            make_locked_package("monolog/monolog", "3.8.0"), // updated
            make_locked_package("new/package", "2.0.0"),     // installed
        ]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        // 3 changes: update monolog, remove old/package, install new/package
        assert_eq!(changes.len(), 3);

        let monolog = changes
            .iter()
            .find(|c| c.name == "monolog/monolog")
            .unwrap();
        assert!(matches!(
            &monolog.kind,
            ChangeKind::Update { old_version, new_version }
            if old_version == "3.7.0" && new_version == "3.8.0"
        ));

        let removed = changes.iter().find(|c| c.name == "old/package").unwrap();
        assert!(matches!(&removed.kind, ChangeKind::Remove { .. }));

        let installed = changes.iter().find(|c| c.name == "new/package").unwrap();
        assert!(matches!(&installed.kind, ChangeKind::Install { .. }));
    }

    #[test]
    fn test_compute_update_changes_dev_packages_included() {
        // dev_mode=true: dev packages are also compared
        let mut old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        old_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "10.0.0")]);

        let mut new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        new_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, true);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "phpunit/phpunit");
        assert!(matches!(&changes[0].kind, ChangeKind::Update { .. }));
    }

    #[test]
    fn test_compute_update_changes_dev_packages_excluded_when_no_dev() {
        // dev_mode=false: dev packages are ignored
        let mut old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        old_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "10.0.0")]);

        let mut new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        new_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        // No changes because we're not including dev packages
        assert!(
            changes.is_empty(),
            "Dev packages should not appear in changes when dev_mode=false"
        );
    }

    // ──────────── apply_partial_update ────────────

    #[test]
    fn test_apply_partial_update_keeps_non_specified_packages() {
        // old lock has psr/log 3.0.0 and monolog 3.7.0
        // resolver found psr/log 3.0.1 and monolog 3.8.0
        // we only want to update monolog
        // expected: psr/log stays at 3.0.0, monolog becomes 3.8.0

        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.7.0"),
        ]);

        let resolved = vec![
            make_resolved_package("psr/log", "3.0.1"),
            make_resolved_package("monolog/monolog", "3.8.0"),
        ];

        let update_packages = vec!["monolog/monolog".to_string()];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(
            psr.version, "3.0.0",
            "psr/log should be kept at old version"
        );

        let monolog = result.iter().find(|p| p.name == "monolog/monolog").unwrap();
        assert_eq!(
            monolog.version, "3.8.0",
            "monolog/monolog should use new version"
        );
    }

    #[test]
    fn test_apply_partial_update_case_insensitive() {
        // update_packages uses mixed case, package names may be lowercase
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![make_resolved_package("psr/log", "3.0.1")];

        // Not updating psr/log (not in update list); should revert to 3.0.0
        let update_packages = vec!["MonoLog/Monolog".to_string()];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(psr.version, "3.0.0");
    }

    #[test]
    fn test_apply_partial_update_new_package_in_update_list() {
        // A brand new package resolved that is in the update list should use the new version
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![
            make_resolved_package("psr/log", "3.0.0"),
            make_resolved_package("new/package", "1.0.0"),
        ];

        let update_packages = vec!["new/package".to_string()];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        let new_pkg = result.iter().find(|p| p.name == "new/package").unwrap();
        assert_eq!(new_pkg.version, "1.0.0");
    }

    #[test]
    fn test_apply_partial_update_full_update_mode() {
        // If update_packages is empty, it should behave like full update (no swapping)
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![make_resolved_package("psr/log", "3.0.1")];

        // Empty update list means... everything is not in the update set,
        // so old versions are preserved. This is the expected behavior for partial mode
        // when packages is empty (which shouldn't happen - full update is separate path).
        let update_packages: Vec<String> = vec![];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        // When update_packages is empty, nothing is in the update set, so old versions revert
        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(psr.version, "3.0.0");
    }

    // ──────────── lock mode helpers ────────────

    #[test]
    fn test_handle_lock_mode_updates_hash() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");

        // Write an existing lock with a known hash
        let mut lock = minimal_lock(vec![]);
        lock.content_hash = "old_hash_value".to_string();
        lock.write_to_file(&lock_path).unwrap();

        // Composer.json content that will produce a different hash
        let composer_json_content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        let result = handle_lock_mode(&lock_path, composer_json_content, false, &console);
        assert!(result.is_ok());

        // Read back and verify hash changed
        let updated_lock = lockfile::LockFile::read_from_file(&lock_path).unwrap();
        assert_ne!(updated_lock.content_hash, "old_hash_value");
        let expected_hash =
            lockfile::LockFile::compute_content_hash(composer_json_content).unwrap();
        assert_eq!(updated_lock.content_hash, expected_hash);
    }

    #[test]
    fn test_handle_lock_mode_no_change_when_hash_matches() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");

        let composer_json_content = r#"{"name": "test/project", "require": {}}"#;
        let correct_hash = lockfile::LockFile::compute_content_hash(composer_json_content).unwrap();

        let mut lock = minimal_lock(vec![]);
        lock.content_hash = correct_hash.clone();
        lock.write_to_file(&lock_path).unwrap();

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        let result = handle_lock_mode(&lock_path, composer_json_content, false, &console);
        assert!(result.is_ok());

        // Hash should not have changed
        let reloaded = lockfile::LockFile::read_from_file(&lock_path).unwrap();
        assert_eq!(reloaded.content_hash, correct_hash);
    }

    #[test]
    fn test_handle_lock_mode_dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");

        let mut lock = minimal_lock(vec![]);
        lock.content_hash = "original_hash".to_string();
        lock.write_to_file(&lock_path).unwrap();

        let composer_json_content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        let result = handle_lock_mode(&lock_path, composer_json_content, true, &console);
        assert!(result.is_ok());

        // Hash should NOT have changed (dry_run=true)
        let reloaded = lockfile::LockFile::read_from_file(&lock_path).unwrap();
        assert_eq!(reloaded.content_hash, "original_hash");
    }

    // ──────────── glob_matches ────────────

    #[test]
    fn test_glob_matches_exact() {
        assert!(glob_matches("monolog/monolog", "monolog/monolog"));
        assert!(!glob_matches("monolog/monolog", "monolog/logger"));
    }

    #[test]
    fn test_glob_matches_case_insensitive() {
        assert!(glob_matches("Monolog/Monolog", "monolog/monolog"));
        assert!(glob_matches("symfony/*", "Symfony/Console"));
    }

    #[test]
    fn test_glob_matches_vendor_wildcard() {
        assert!(glob_matches("symfony/*", "symfony/console"));
        assert!(glob_matches("symfony/*", "symfony/http-kernel"));
        assert!(!glob_matches("symfony/*", "monolog/monolog"));
    }

    #[test]
    fn test_glob_matches_wildcard_in_name() {
        assert!(glob_matches("monolog/mono*", "monolog/monolog"));
        assert!(!glob_matches("monolog/mono*", "monolog/logger"));
    }

    #[test]
    fn test_glob_matches_wildcard_no_slash() {
        // Without a '/' the pattern still works as a full name match
        assert!(!glob_matches("symfony/*", "monolog/monolog"));
    }

    #[test]
    fn test_glob_matches_different_segment_count() {
        // "vendor/*" has 2 segments; "monolog" has only 1: no match
        assert!(!glob_matches("vendor/*", "monolog"));
        // Pattern with 1 segment vs name with 2 segments: no match
        assert!(!glob_matches("monolog", "monolog/monolog"));
    }

    // ──────────── expand_wildcards ────────────

    #[test]
    fn test_expand_wildcards_no_wildcard_passthrough() {
        let lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let specs = vec!["psr/log".to_string(), "nonexistent/pkg".to_string()];
        let result = expand_wildcards(&specs, &lock);
        assert_eq!(result, vec!["psr/log", "nonexistent/pkg"]);
    }

    #[test]
    fn test_expand_wildcards_vendor_star() {
        let lock = minimal_lock(vec![
            make_locked_package("symfony/console", "7.0.0"),
            make_locked_package("symfony/http-kernel", "7.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);
        let specs = vec!["symfony/*".to_string()];
        let mut result = expand_wildcards(&specs, &lock);
        result.sort();
        assert_eq!(result, vec!["symfony/console", "symfony/http-kernel"]);
    }

    #[test]
    fn test_expand_wildcards_no_match_emits_warning() {
        let lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let specs = vec!["unknown/*".to_string()];
        // Should return empty (no match), no panic
        let result = expand_wildcards(&specs, &lock);
        assert!(result.is_empty());
    }

    #[test]
    fn test_expand_wildcards_deduplication() {
        let lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let specs = vec!["psr/log".to_string(), "psr/log".to_string()];
        let result = expand_wildcards(&specs, &lock);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "psr/log");
    }

    #[test]
    fn test_expand_wildcards_also_checks_dev() {
        let mut lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);
        let specs = vec!["phpunit/*".to_string()];
        let result = expand_wildcards(&specs, &lock);
        assert_eq!(result, vec!["phpunit/phpunit"]);
    }

    // ──────────── expand_with_direct_dependencies ────────────

    #[test]
    fn test_expand_with_direct_deps_adds_require() {
        // monolog/monolog requires psr/log
        let mut pkg = make_locked_package("monolog/monolog", "3.8.0");
        pkg.require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![pkg, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_with_direct_dependencies(vec!["monolog/monolog".to_string()], &lock);
        let mut result_sorted = result.clone();
        result_sorted.sort();
        assert!(result_sorted.contains(&"monolog/monolog".to_string()));
        assert!(result_sorted.contains(&"psr/log".to_string()));
    }

    #[test]
    fn test_expand_with_direct_deps_skips_platform() {
        let mut pkg = make_locked_package("monolog/monolog", "3.8.0");
        pkg.require.insert("php".to_string(), ">=8.1".to_string());
        pkg.require.insert("ext-json".to_string(), "*".to_string());
        pkg.require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![pkg, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_with_direct_dependencies(vec!["monolog/monolog".to_string()], &lock);
        // Should NOT include php or ext-json
        assert!(!result.contains(&"php".to_string()));
        assert!(!result.contains(&"ext-json".to_string()));
        assert!(result.contains(&"psr/log".to_string()));
    }

    #[test]
    fn test_expand_with_direct_deps_no_duplicates() {
        // Both packages in the list require psr/log
        let mut pkg_a = make_locked_package("foo/a", "1.0.0");
        pkg_a
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());
        let mut pkg_b = make_locked_package("foo/b", "1.0.0");
        pkg_b
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![pkg_a, pkg_b, make_locked_package("psr/log", "3.0.0")]);

        let result =
            expand_with_direct_dependencies(vec!["foo/a".to_string(), "foo/b".to_string()], &lock);
        let psr_count = result.iter().filter(|s| s.as_str() == "psr/log").count();
        assert_eq!(psr_count, 1, "psr/log should appear only once");
    }

    // ──────────── expand_with_all_dependencies ────────────

    #[test]
    fn test_expand_all_deps_transitive() {
        // a -> b -> c
        let mut pkg_a = make_locked_package("foo/a", "1.0.0");
        pkg_a
            .require
            .insert("foo/b".to_string(), "^1.0".to_string());
        let mut pkg_b = make_locked_package("foo/b", "1.0.0");
        pkg_b
            .require
            .insert("foo/c".to_string(), "^1.0".to_string());
        let pkg_c = make_locked_package("foo/c", "1.0.0");

        let lock = minimal_lock(vec![pkg_a, pkg_b, pkg_c]);

        let result = expand_with_all_dependencies(vec!["foo/a".to_string()], &lock);
        assert!(result.contains(&"foo/a".to_string()));
        assert!(result.contains(&"foo/b".to_string()));
        assert!(result.contains(&"foo/c".to_string()));
    }

    #[test]
    fn test_expand_all_deps_no_infinite_loop() {
        // Circular reference: a -> b -> a
        let mut pkg_a = make_locked_package("foo/a", "1.0.0");
        pkg_a
            .require
            .insert("foo/b".to_string(), "^1.0".to_string());
        let mut pkg_b = make_locked_package("foo/b", "1.0.0");
        pkg_b
            .require
            .insert("foo/a".to_string(), "^1.0".to_string());

        let lock = minimal_lock(vec![pkg_a, pkg_b]);

        // Must not loop infinitely
        let result = expand_with_all_dependencies(vec!["foo/a".to_string()], &lock);
        assert!(result.contains(&"foo/a".to_string()));
        assert!(result.contains(&"foo/b".to_string()));
        assert_eq!(result.len(), 2);
    }

    // ──────────── expand_packages ────────────

    #[test]
    fn test_expand_packages_wildcard_with_direct_deps() {
        // symfony/* expands to symfony/console; symfony/console requires psr/log
        let mut console_pkg = make_locked_package("symfony/console", "7.0.0");
        console_pkg
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![console_pkg, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_packages(
            &["symfony/*".to_string()],
            Some(&lock),
            true,  // with_dependencies
            false, // with_all_dependencies
        );

        assert!(result.contains(&"symfony/console".to_string()));
        assert!(result.contains(&"psr/log".to_string()));
    }

    // ──────────── apply_minimal_changes ────────────

    #[test]
    fn test_apply_minimal_changes_pins_all() {
        // Resolver found psr/log 3.0.1, but old lock has 3.0.0
        // apply_minimal_changes should pin back to 3.0.0
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![make_resolved_package("psr/log", "3.0.1")];

        let result = apply_minimal_changes(resolved, &old_lock);
        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(
            psr.version, "3.0.0",
            "minimal-changes should pin to locked version"
        );
    }

    // ──────────── Integration test (network, #[ignore]) ────────────

    #[tokio::test]
    #[ignore]
    async fn test_update_full_e2e() {
        use mozart_core::package::RawPackageData;
        use mozart_registry::lockfile::{LockFileGenerationRequest, generate_lock_file};
        use mozart_registry::resolver::{ResolveRequest, resolve};

        let composer_json_content =
            r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let composer_json: RawPackageData = serde_json::from_str(composer_json_content).unwrap();

        let request = ResolveRequest {
            root_name: String::new(),
            require: vec![("monolog/monolog".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let resolved = resolve(&request).await.expect("Resolution should succeed");
        assert!(!resolved.is_empty());
        assert!(resolved.iter().any(|p| p.name == "monolog/monolog"));

        let lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.to_string(),
            composer_json,
            include_dev: false,
            repo_cache: None,
        })
        .await
        .expect("Lock file generation should succeed");

        assert!(!lock.content_hash.is_empty());
        assert!(!lock.packages.is_empty());
        assert!(lock.packages.iter().any(|p| p.name == "monolog/monolog"));
    }

    #[test]
    fn test_update_lock_only_e2e() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");

        // Write a lock with an outdated hash
        let mut lock = minimal_lock(vec![]);
        lock.content_hash = "outdated_hash".to_string();
        lock.write_to_file(&lock_path).unwrap();

        let composer_json_content = r#"{"name": "test/project", "require": {"psr/log": "^3.0"}}"#;
        let expected_hash =
            lockfile::LockFile::compute_content_hash(composer_json_content).unwrap();

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        handle_lock_mode(&lock_path, composer_json_content, false, &console).unwrap();

        let updated = lockfile::LockFile::read_from_file(&lock_path).unwrap();
        assert_eq!(updated.content_hash, expected_hash);
        // The packages should be unchanged (lock mode doesn't resolve)
        assert!(updated.packages.is_empty());
    }
}
