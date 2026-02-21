use crate::console;
use crate::lockfile;
use crate::package::{self, Stability};
use crate::resolver::{self, PlatformConfig, ResolveRequest, ResolvedPackage};
use clap::Args;
use std::collections::HashMap;

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
// Main execute function
// ─────────────────────────────────────────────────────────────────────────────

pub fn execute(args: &UpdateArgs, cli: &super::Cli) -> anyhow::Result<()> {
    // Step 1: Resolve the working directory
    let working_dir = super::install::resolve_working_dir(cli);

    // Step 2: Handle deprecated flags
    if args.dev {
        eprintln!(
            "{}",
            console::warning(
                "The --dev option is deprecated. Dev packages are updated by default."
            )
        );
    }
    if args.no_suggest {
        eprintln!(
            "{}",
            console::warning("The --no-suggest option is deprecated and has no effect.")
        );
    }

    // Warn about deferred flags
    if args.with_dependencies || args.with_all_dependencies {
        eprintln!(
            "{}",
            console::warning(
                "--with-dependencies / --with-all-dependencies are not yet implemented and will be ignored."
            )
        );
    }
    if args.minimal_changes {
        eprintln!(
            "{}",
            console::warning("--minimal-changes is not yet implemented and will be ignored.")
        );
    }
    if args.patch_only {
        eprintln!(
            "{}",
            console::warning("--patch-only is not yet implemented and will be ignored.")
        );
    }
    if args.interactive {
        eprintln!(
            "{}",
            console::warning("--interactive is not yet implemented and will be ignored.")
        );
    }
    if args.root_reqs {
        eprintln!(
            "{}",
            console::warning("--root-reqs is not yet implemented and will be ignored.")
        );
    }
    if args.bump_after_update.is_some() {
        eprintln!(
            "{}",
            console::warning("--bump-after-update is not yet implemented and will be ignored.")
        );
    }

    // Step 3: Read composer.json
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        eprintln!(
            "{}",
            console::error(&format!(
                "Composer could not find a composer.json file in {}",
                working_dir.display()
            ))
        );
        std::process::exit(1);
    }
    let composer_json = package::read_from_file(&composer_json_path)?;
    let composer_json_content = std::fs::read_to_string(&composer_json_path)?;

    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");

    // Step 4: Handle --lock mode (early return)
    if args.lock {
        return handle_lock_mode(&lock_path, &composer_json_content, args.dry_run);
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
    };

    // Step 6: Print header and run resolver
    eprintln!("Loading composer repositories with package information");
    if dev_mode {
        eprintln!("Updating dependencies (including require-dev)");
    } else {
        eprintln!("Updating dependencies");
    }
    eprintln!("Resolving dependencies...");

    let mut resolved = match resolver::resolve(&request) {
        Ok(packages) => packages,
        Err(e) => {
            eprintln!("{}", console::error(&e.to_string()));
            std::process::exit(1);
        }
    };

    // Step 7: Read old lock file (for change reporting and partial update)
    let old_lock = if lock_path.exists() {
        match lockfile::LockFile::read_from_file(&lock_path) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!(
                    "{}",
                    console::warning(&format!(
                        "Could not read existing composer.lock: {}. Treating as a fresh install.",
                        e
                    ))
                );
                None
            }
        }
    } else {
        None
    };

    // Step 8: Handle partial update (if specific packages were named)
    if !args.packages.is_empty() {
        match &old_lock {
            None => {
                eprintln!(
                    "{}",
                    console::error(
                        "No lock file found. Cannot perform partial update. Run `mozart update` first."
                    )
                );
                std::process::exit(1);
            }
            Some(lock) => {
                resolved = apply_partial_update(resolved, lock, &args.packages);
            }
        }
    }

    // Step 9: Generate new lock file
    let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content: composer_json_content.clone(),
        composer_json: composer_json.clone(),
        include_dev: dev_mode,
    })?;

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

    // Print individual change lines
    let prefix = if args.dry_run { "Would" } else { "" };
    for change in &changes {
        match &change.kind {
            ChangeKind::Remove { old_version } => {
                if args.dry_run {
                    eprintln!("  - {} remove {} ({})", prefix, change.name, old_version);
                } else {
                    eprintln!("  - Removing {} ({})", change.name, old_version);
                }
            }
            ChangeKind::Install { new_version } => {
                if args.dry_run {
                    eprintln!("  - {} install {} ({})", prefix, change.name, new_version);
                } else {
                    eprintln!("  - Installing {} ({})", change.name, new_version);
                }
            }
            ChangeKind::Update {
                old_version,
                new_version,
            } => {
                if args.dry_run {
                    eprintln!(
                        "  - {} update {} ({} => {})",
                        prefix, change.name, old_version, new_version
                    );
                } else {
                    eprintln!(
                        "  - Updating {} ({} => {})",
                        change.name, old_version, new_version
                    );
                }
            }
            ChangeKind::Unchanged => {}
        }
    }

    // Step 11: Write lock file (unless --dry-run)
    if !args.dry_run {
        eprintln!("Writing lock file");
        new_lock.write_to_file(&lock_path)?;
    }

    // Step 12: Install packages (unless --no-install or --dry-run)
    if !args.no_install && !args.dry_run {
        super::install::install_from_lock(
            &new_lock,
            &working_dir,
            &vendor_dir,
            dev_mode,
            false, // dry_run already checked above
            args.no_autoloader,
        )?;
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
) -> anyhow::Result<()> {
    if !lock_path.exists() {
        eprintln!(
            "{}",
            console::error("No lock file found. Run `mozart update` to generate one.")
        );
        std::process::exit(1);
    }

    let mut lock = lockfile::LockFile::read_from_file(lock_path)?;

    let new_hash = lockfile::LockFile::compute_content_hash(composer_json_content)?;

    if new_hash == lock.content_hash {
        eprintln!("Lock file is already up to date");
        return Ok(());
    }

    lock.content_hash = new_hash;

    if !dry_run {
        lock.write_to_file(lock_path)?;
        eprintln!("Lock file hash updated successfully.");
    } else {
        eprintln!("Would update lock file hash.");
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

        let result = handle_lock_mode(&lock_path, composer_json_content, false);
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

        let result = handle_lock_mode(&lock_path, composer_json_content, false);
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

        let result = handle_lock_mode(&lock_path, composer_json_content, true);
        assert!(result.is_ok());

        // Hash should NOT have changed (dry_run=true)
        let reloaded = lockfile::LockFile::read_from_file(&lock_path).unwrap();
        assert_eq!(reloaded.content_hash, "original_hash");
    }

    // ──────────── Integration test (network, #[ignore]) ────────────

    #[test]
    #[ignore]
    fn test_update_full_e2e() {
        use crate::lockfile::{LockFileGenerationRequest, generate_lock_file};
        use crate::package::RawPackageData;
        use crate::resolver::{ResolveRequest, resolve};

        let composer_json_content =
            r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let composer_json: RawPackageData = serde_json::from_str(composer_json_content).unwrap();

        let request = ResolveRequest {
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
        };

        let resolved = resolve(&request).expect("Resolution should succeed");
        assert!(!resolved.is_empty());
        assert!(resolved.iter().any(|p| p.name == "monolog/monolog"));

        let lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.to_string(),
            composer_json,
            include_dev: false,
        })
        .expect("Lock file generation should succeed");

        assert!(!lock.content_hash.is_empty());
        assert!(!lock.packages.is_empty());
        assert!(lock.packages.iter().any(|p| p.name == "monolog/monolog"));
    }

    #[test]
    #[ignore]
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

        handle_lock_mode(&lock_path, composer_json_content, false).unwrap();

        let updated = lockfile::LockFile::read_from_file(&lock_path).unwrap();
        assert_eq!(updated.content_hash, expected_hash);
        // The packages should be unchanged (lock mode doesn't resolve)
        assert!(updated.packages.is_empty());
    }
}
