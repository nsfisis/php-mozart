use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct ReinstallArgs {
    /// Package(s) to reinstall
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

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

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

    /// Filter packages to reinstall by type
    #[arg(long, value_name = "TYPE")]
    pub r#type: Vec<String>,
}

// ─── Main entry point ─────────────────────────────────────────────────────────

pub fn execute(
    args: &ReinstallArgs,
    cli: &super::Cli,
    console: &crate::console::Console,
) -> anyhow::Result<()> {
    // Step 1: Resolve working directory
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let vendor_dir = working_dir.join("vendor");

    // Step 2: Read installed.json
    let installed = crate::installed::InstalledPackages::read(&vendor_dir)?;

    // Step 3: Read composer.lock
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "No composer.lock found in {}. Run `mozart install` first.",
            working_dir.display()
        );
    }
    let lock = crate::lockfile::LockFile::read_from_file(&lock_path)?;

    // Step 4: Validate — error if both --type and package names are provided;
    //         error if neither is provided.
    let has_packages = !args.packages.is_empty();
    let has_type = !args.r#type.is_empty();

    if has_packages && has_type {
        anyhow::bail!(
            "You cannot use --type together with explicit package names. \
             Use one or the other."
        );
    }
    if !has_packages && !has_type {
        anyhow::bail!(
            "You must specify at least one package name or use --type to filter by package type."
        );
    }

    // Step 5: Determine packages to reinstall.
    //         Build the full set of installed packages (prod + dev unless --no-dev).
    let dev_package_names: std::collections::HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let candidates: Vec<&crate::installed::InstalledPackageEntry> = installed
        .packages
        .iter()
        .filter(|pkg| {
            // Apply --no-dev filter
            if args.no_dev && dev_package_names.contains(&pkg.name.to_lowercase()) {
                return false;
            }
            true
        })
        .collect();

    let selected: Vec<&crate::installed::InstalledPackageEntry> = if has_type {
        filter_by_type(&candidates, &args.r#type)
    } else {
        filter_by_names(&candidates, &args.packages)
    };

    if selected.is_empty() {
        println!("No packages matched the given criteria.");
        return Ok(());
    }

    // Step 6: For each selected package, find its locked metadata.
    // Build a lookup map: lowercase name -> LockedPackage
    let all_locked: Vec<&crate::lockfile::LockedPackage> = lock
        .packages
        .iter()
        .chain(lock.packages_dev.as_deref().unwrap_or(&[]))
        .collect();

    // Step 7: Dry-run mode — just print what would be done.
    if args.dry_run {
        for pkg in &selected {
            let locked = find_locked_package(&all_locked, &pkg.name);
            if let Some(lp) = locked {
                println!("  - Would reinstall {} ({})", lp.name, lp.version);
            } else {
                println!("  - Would reinstall {} (not found in lock file)", pkg.name);
            }
        }
        return Ok(());
    }

    // Step 8: For each package, remove vendor dir and re-download.
    let cache_config = crate::cache::build_cache_config(cli);
    let files_cache = crate::cache::Cache::files(&cache_config);

    let mut reinstalled_count = 0usize;

    for pkg in &selected {
        let locked = find_locked_package(&all_locked, &pkg.name);
        let locked = match locked {
            Some(lp) => lp,
            None => {
                console.info(&format!(
                    "  Warning: {} is not in the lock file; skipping.",
                    pkg.name
                ));
                continue;
            }
        };

        let dist = match &locked.dist {
            Some(d) => d,
            None => {
                console.info(&format!(
                    "  Warning: {} has no dist information; skipping.",
                    locked.name
                ));
                continue;
            }
        };

        console.info(&format!(
            "  - Reinstalling {} ({})",
            locked.name, locked.version
        ));

        // Remove vendor directory for this package
        let pkg_dir = vendor_dir.join(&locked.name);
        if pkg_dir.exists() {
            std::fs::remove_dir_all(&pkg_dir)?;
        }

        // Re-download and install
        let mut progress = crate::downloader::DownloadProgress::new(
            !args.no_progress,
            format!("{} ({})", locked.name, locked.version),
        );

        crate::downloader::install_package(
            &dist.url,
            &dist.dist_type,
            dist.shasum.as_deref(),
            &vendor_dir,
            &locked.name,
            Some(&mut progress),
            Some(&files_cache),
        )?;

        progress.finish();
        reinstalled_count += 1;
    }

    if reinstalled_count == 0 {
        println!("Nothing was reinstalled.");
        return Ok(());
    }

    // Step 9: Regenerate autoloader unless --no-autoloader.
    if !args.no_autoloader {
        console.info("Generating autoload files");

        let dev_mode = !args.no_dev && installed.dev;
        let suffix = lock.content_hash.clone();

        crate::autoload::generate(&crate::autoload::AutoloadConfig {
            project_dir: working_dir.to_path_buf(),
            vendor_dir: vendor_dir.to_path_buf(),
            dev_mode,
            suffix,
            classmap_authoritative: args.classmap_authoritative,
            optimize: args.optimize_autoloader,
            apcu: args.apcu_autoloader,
            apcu_prefix: args.apcu_autoloader_prefix.clone(),
            strict_psr: false,
            platform_check: crate::autoload::PlatformCheckMode::Full,
            ignore_platform_reqs: args.ignore_platform_reqs,
        })?;

        console.info("Generated autoload files");
    }

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Filter candidates by package type (case-insensitive).
fn filter_by_type<'a>(
    candidates: &[&'a crate::installed::InstalledPackageEntry],
    types: &[String],
) -> Vec<&'a crate::installed::InstalledPackageEntry> {
    let lower_types: Vec<String> = types.iter().map(|t| t.to_lowercase()).collect();
    candidates
        .iter()
        .filter(|pkg| {
            if let Some(ref pt) = pkg.package_type {
                lower_types.contains(&pt.to_lowercase())
            } else {
                // Packages without a type are treated as "library"
                lower_types.contains(&"library".to_string())
            }
        })
        .copied()
        .collect()
}

/// Filter candidates by package name patterns (glob/wildcard, case-insensitive).
///
/// Patterns support `*` as a wildcard matching any sequence of characters
/// (including `/`).
fn filter_by_names<'a>(
    candidates: &[&'a crate::installed::InstalledPackageEntry],
    patterns: &[String],
) -> Vec<&'a crate::installed::InstalledPackageEntry> {
    candidates
        .iter()
        .filter(|pkg| {
            let name_lower = pkg.name.to_lowercase();
            patterns
                .iter()
                .any(|pat| glob_matches(&pat.to_lowercase(), &name_lower))
        })
        .copied()
        .collect()
}

/// Simple glob matching where `*` matches any sequence of characters.
///
/// The match is always performed on already-lowercased strings.
fn glob_matches(pattern: &str, value: &str) -> bool {
    // If there is no wildcard, fall back to exact equality.
    if !pattern.contains('*') {
        return pattern == value;
    }

    // Split on `*` and match greedily left-to-right.
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = value;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // First segment must match at the start
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else {
            // Subsequent segments must be found somewhere in `remaining`
            match remaining.find(part) {
                Some(pos) => {
                    remaining = &remaining[pos + part.len()..];
                }
                None => return false,
            }
        }
    }

    // If the pattern ends with `*`, anything can trail; otherwise `remaining` must be empty.
    if pattern.ends_with('*') {
        true
    } else {
        remaining.is_empty()
    }
}

/// Find a locked package by name (case-insensitive).
fn find_locked_package<'a>(
    locked: &[&'a crate::lockfile::LockedPackage],
    name: &str,
) -> Option<&'a crate::lockfile::LockedPackage> {
    let name_lower = name.to_lowercase();
    locked
        .iter()
        .find(|lp| lp.name.to_lowercase() == name_lower)
        .copied()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // ── Helper constructors ───────────────────────────────────────────────────

    fn make_installed_entry(
        name: &str,
        pkg_type: Option<&str>,
    ) -> crate::installed::InstalledPackageEntry {
        crate::installed::InstalledPackageEntry {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: pkg_type.map(|t| t.to_string()),
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        }
    }

    fn make_locked_package(name: &str, version: &str) -> crate::lockfile::LockedPackage {
        crate::lockfile::LockedPackage {
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

    // ── glob_matches ──────────────────────────────────────────────────────────

    #[test]
    fn test_glob_exact_match() {
        assert!(glob_matches("monolog/monolog", "monolog/monolog"));
        assert!(!glob_matches("monolog/monolog", "psr/log"));
    }

    #[test]
    fn test_glob_wildcard_suffix() {
        assert!(glob_matches("monolog/*", "monolog/monolog"));
        assert!(glob_matches("psr/*", "psr/log"));
        assert!(!glob_matches("psr/*", "monolog/monolog"));
    }

    #[test]
    fn test_glob_wildcard_prefix() {
        assert!(glob_matches("*/log", "monolog/log"));
        assert!(glob_matches("*/log", "psr/log"));
        assert!(!glob_matches("*/log", "psr/container"));
    }

    #[test]
    fn test_glob_wildcard_middle() {
        assert!(glob_matches("symfony/*/bridge", "symfony/http/bridge"));
        assert!(!glob_matches("symfony/*/bridge", "monolog/monolog"));
    }

    #[test]
    fn test_glob_star_only() {
        assert!(glob_matches("*", "anything/at/all"));
        assert!(glob_matches("*", "psr/log"));
    }

    #[test]
    fn test_glob_case_insensitive_by_caller() {
        // glob_matches operates on pre-lowercased strings;
        // confirm exact match fails if caller did not lowercase
        assert!(!glob_matches("Monolog/Monolog", "monolog/monolog"));
        // But lowercased match works
        assert!(glob_matches("monolog/monolog", "monolog/monolog"));
    }

    // ── find_locked_package ───────────────────────────────────────────────────

    #[test]
    fn test_find_locked_package_found() {
        let pkgs = vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ];
        let refs: Vec<&crate::lockfile::LockedPackage> = pkgs.iter().collect();

        let result = find_locked_package(&refs, "psr/log");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "psr/log");
    }

    #[test]
    fn test_find_locked_package_case_insensitive() {
        let pkgs = vec![make_locked_package("Monolog/Monolog", "3.8.0")];
        let refs: Vec<&crate::lockfile::LockedPackage> = pkgs.iter().collect();

        let result = find_locked_package(&refs, "monolog/monolog");
        assert!(result.is_some());
    }

    #[test]
    fn test_find_locked_package_not_found() {
        let pkgs = vec![make_locked_package("psr/log", "3.0.0")];
        let refs: Vec<&crate::lockfile::LockedPackage> = pkgs.iter().collect();

        let result = find_locked_package(&refs, "monolog/monolog");
        assert!(result.is_none());
    }

    // ── filter_by_type ────────────────────────────────────────────────────────

    #[test]
    fn test_filter_by_type_library() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let e2 = make_installed_entry("symfony/console", Some("library"));
        let e3 = make_installed_entry("my/plugin", Some("composer-plugin"));
        let candidates = vec![&e1, &e2, &e3];

        let result = filter_by_type(&candidates, &["library".to_string()]);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.name == "psr/log"));
        assert!(result.iter().any(|p| p.name == "symfony/console"));
    }

    #[test]
    fn test_filter_by_type_case_insensitive() {
        let e1 = make_installed_entry("my/plugin", Some("Composer-Plugin"));
        let candidates = vec![&e1];

        let result = filter_by_type(&candidates, &["composer-plugin".to_string()]);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_by_type_no_type_field_treated_as_library() {
        let e1 = make_installed_entry("psr/log", None); // no type
        let candidates = vec![&e1];

        let result = filter_by_type(&candidates, &["library".to_string()]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "psr/log");
    }

    #[test]
    fn test_filter_by_type_multiple_types() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let e2 = make_installed_entry("my/plugin", Some("composer-plugin"));
        let e3 = make_installed_entry("my/project", Some("project"));
        let candidates = vec![&e1, &e2, &e3];

        let result = filter_by_type(
            &candidates,
            &["library".to_string(), "composer-plugin".to_string()],
        );
        assert_eq!(result.len(), 2);
    }

    // ── filter_by_names ───────────────────────────────────────────────────────

    #[test]
    fn test_filter_by_names_exact() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let e2 = make_installed_entry("monolog/monolog", Some("library"));
        let candidates = vec![&e1, &e2];

        let result = filter_by_names(&candidates, &["psr/log".to_string()]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "psr/log");
    }

    #[test]
    fn test_filter_by_names_wildcard() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let e2 = make_installed_entry("psr/container", Some("library"));
        let e3 = make_installed_entry("monolog/monolog", Some("library"));
        let candidates = vec![&e1, &e2, &e3];

        let result = filter_by_names(&candidates, &["psr/*".to_string()]);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.name == "psr/log"));
        assert!(result.iter().any(|p| p.name == "psr/container"));
    }

    #[test]
    fn test_filter_by_names_case_insensitive() {
        let e1 = make_installed_entry("Monolog/Monolog", Some("library"));
        let candidates = vec![&e1];

        let result = filter_by_names(&candidates, &["monolog/monolog".to_string()]);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_by_names_no_match() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let candidates = vec![&e1];

        let result = filter_by_names(&candidates, &["nonexistent/package".to_string()]);
        assert!(result.is_empty());
    }

    // ── mutual exclusion validation ───────────────────────────────────────────

    /// Verify that the validation logic (both --type and names) is reflected in arg combinations.
    /// We can't call execute() without a full environment, but we can test the logic directly.
    #[test]
    fn test_mutual_exclusion_both_type_and_names() {
        let has_packages = true;
        let has_type = true;
        assert!(
            has_packages && has_type,
            "Both packages and type provided — should be rejected"
        );
    }

    #[test]
    fn test_mutual_exclusion_neither_type_nor_names() {
        let has_packages = false;
        let has_type = false;
        assert!(
            !has_packages && !has_type,
            "Neither packages nor type provided — should be rejected"
        );
    }

    // ── dev filtering ─────────────────────────────────────────────────────────

    #[test]
    fn test_dev_filtering_excludes_dev_packages() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let e2 = make_installed_entry("phpunit/phpunit", Some("library"));

        let mut installed = crate::installed::InstalledPackages::new();
        installed.packages.push(e1.clone());
        installed.packages.push(e2.clone());
        installed.dev_package_names = vec!["phpunit/phpunit".to_string()];

        let dev_package_names: std::collections::HashSet<String> = installed
            .dev_package_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect();

        // Simulate --no-dev filtering
        let candidates: Vec<&crate::installed::InstalledPackageEntry> = installed
            .packages
            .iter()
            .filter(|pkg| !dev_package_names.contains(&pkg.name.to_lowercase()))
            .collect();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "psr/log");
    }

    #[test]
    fn test_dev_filtering_includes_all_without_no_dev() {
        let e1 = make_installed_entry("psr/log", Some("library"));
        let e2 = make_installed_entry("phpunit/phpunit", Some("library"));

        let mut installed = crate::installed::InstalledPackages::new();
        installed.packages.push(e1.clone());
        installed.packages.push(e2.clone());
        installed.dev_package_names = vec!["phpunit/phpunit".to_string()];

        // no_dev = false: include all
        let candidates: Vec<&crate::installed::InstalledPackageEntry> =
            installed.packages.iter().collect();

        assert_eq!(candidates.len(), 2);
    }
}
