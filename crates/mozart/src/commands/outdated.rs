use clap::Args;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct OutdatedArgs {
    /// Package to inspect
    pub package: Option<String>,

    /// Show only packages that are outdated
    #[arg(short, long)]
    pub outdated: bool,

    /// Show all installed packages
    #[arg(short, long)]
    pub all: bool,

    /// Show packages from the lock file
    #[arg(long)]
    pub locked: bool,

    /// Shows only packages that are directly required by the root package
    #[arg(short = 'D', long)]
    pub direct: bool,

    /// Return a non-zero exit code when there are outdated packages
    #[arg(long)]
    pub strict: bool,

    /// Only show packages that have major SemVer-compatible updates
    #[arg(short = 'M', long)]
    pub major_only: bool,

    /// Only show packages that have minor SemVer-compatible updates
    #[arg(short = 'm', long)]
    pub minor_only: bool,

    /// Only show packages that have patch SemVer-compatible updates
    #[arg(short = 'p', long)]
    pub patch_only: bool,

    /// Sort packages by age of the last update
    #[arg(short = 'A', long)]
    pub sort_by_age: bool,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,

    /// Ignore specified package(s)
    #[arg(long)]
    pub ignore: Vec<String>,

    /// Disables listing of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,
}

// ─── Core types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateCategory {
    UpToDate,
    /// Constraint allows the update — show in RED (you SHOULD update)
    SemverCompatible,
    /// New major / constraint change needed — show in YELLOW
    SemverIncompatible,
}

#[derive(Debug, Clone)]
struct PackageInfo {
    name: String,
    version: String,
    version_normalized: String,
    description: String,
}

#[derive(Debug, Clone)]
struct OutdatedEntry {
    name: String,
    current_version: String,
    latest_version: String,
    description: String,
    category: UpdateCategory,
    is_direct: bool,
}

// ─── Main entry point ───────────────────────────────────────────────────────

pub fn execute(
    args: &OutdatedArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // Load packages (installed or locked)
    let packages = if args.locked {
        load_locked_packages(&working_dir, args.no_dev)?
    } else {
        load_installed_packages(&working_dir, args.no_dev)?
    };

    if packages.is_empty() {
        return Ok(());
    }

    // Load root composer.json for --direct filtering and constraint lookup
    let composer_json_path = working_dir.join("composer.json");
    let root_package = if composer_json_path.exists() {
        mozart_core::package::read_from_file(&composer_json_path).ok()
    } else {
        None
    };

    // Build set of direct dependency names
    let direct_names: HashSet<String> = if let Some(ref root) = root_package {
        let mut names: HashSet<String> = root.require.keys().map(|k| k.to_lowercase()).collect();
        if !args.no_dev {
            names.extend(root.require_dev.keys().map(|k| k.to_lowercase()));
        }
        names
    } else {
        HashSet::new()
    };

    // Build constraint map from root composer.json
    let root_constraints: BTreeMap<String, String> = if let Some(ref root) = root_package {
        let mut map: BTreeMap<String, String> = root
            .require
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        if !args.no_dev {
            map.extend(
                root.require_dev
                    .iter()
                    .map(|(k, v)| (k.to_lowercase(), v.clone())),
            );
        }
        map
    } else {
        BTreeMap::new()
    };

    // Build ignore set
    let ignore_set: HashSet<String> = args.ignore.iter().map(|n| n.to_lowercase()).collect();

    // Process each package
    let mut entries: Vec<OutdatedEntry> = Vec::new();
    for pkg in &packages {
        // Skip ignored packages
        if ignore_set.contains(&pkg.name.to_lowercase()) {
            continue;
        }

        // --direct filter
        if args.direct && !direct_names.contains(&pkg.name.to_lowercase()) {
            continue;
        }

        // --package filter
        if let Some(ref filter) = args.package
            && !pkg.name.eq_ignore_ascii_case(filter)
        {
            continue;
        }

        // Fetch latest version from Packagist
        let latest = match fetch_latest_version(&pkg.name) {
            Ok(v) => v,
            Err(_) => {
                // Skip packages we can't fetch (platform packages, private, etc.)
                continue;
            }
        };

        // Classify the update
        let root_constraint = root_constraints.get(&pkg.name.to_lowercase()).cloned();
        let category = classify_update(
            &pkg.version_normalized,
            &latest.version_normalized,
            root_constraint.as_deref(),
        );

        // If showing all, include up-to-date; otherwise only show outdated
        if !args.all && category == UpdateCategory::UpToDate {
            continue;
        }

        // Apply level filter (--major-only, --minor-only, --patch-only)
        if (args.major_only || args.minor_only || args.patch_only)
            && category != UpdateCategory::UpToDate
            && !passes_level_filter(args, &pkg.version_normalized, &latest.version_normalized)
        {
            continue;
        }

        let is_direct = direct_names.contains(&pkg.name.to_lowercase());

        entries.push(OutdatedEntry {
            name: pkg.name.clone(),
            current_version: pkg.version.clone(),
            latest_version: latest.version.clone(),
            description: latest.description.clone(),
            category,
            is_direct,
        });
    }

    // Sort alphabetically by name
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    // Render output
    let format = args.format.as_deref().unwrap_or("text");
    match format {
        "json" => render_json(&entries)?,
        _ => render_text(&entries),
    }

    // --strict: exit with code 1 if any outdated packages exist
    if args.strict {
        let has_outdated = entries
            .iter()
            .any(|e| e.category != UpdateCategory::UpToDate);
        if has_outdated {
            std::process::exit(1);
        }
    }

    Ok(())
}

// ─── Package loading ────────────────────────────────────────────────────────

fn load_installed_packages(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<PackageInfo>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    let dev_names: HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let mut packages: Vec<PackageInfo> = installed
        .packages
        .iter()
        .filter(|p| {
            // Skip dev packages when --no-dev
            if no_dev && dev_names.contains(&p.name.to_lowercase()) {
                return false;
            }
            // Skip platform packages
            if is_platform_package(&p.name) {
                return false;
            }
            true
        })
        .map(|p| {
            let version_normalized = p
                .version_normalized
                .clone()
                .unwrap_or_else(|| normalize_version_simple(&p.version));
            let description = p
                .extra_fields
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            PackageInfo {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized,
                description,
            }
        })
        .collect();

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

fn load_locked_packages(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<PackageInfo>> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "A valid composer.json and composer.lock file is required to run this command with --locked"
        );
    }

    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();

    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    let packages: Vec<PackageInfo> = all_packages
        .iter()
        .filter(|p| !is_platform_package(&p.name))
        .map(|p| {
            let version_normalized = p
                .version_normalized
                .clone()
                .unwrap_or_else(|| normalize_version_simple(&p.version));
            let description = p.description.as_deref().unwrap_or("").to_string();
            PackageInfo {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized,
                description,
            }
        })
        .collect();

    Ok(packages)
}

// ─── Version fetching ────────────────────────────────────────────────────────

fn fetch_latest_version(name: &str) -> anyhow::Result<PackageInfo> {
    use mozart_core::package::Stability;
    use mozart_registry::version::find_best_candidate;

    let versions = mozart_registry::packagist::fetch_package_versions(name, None)?;
    let best = find_best_candidate(&versions, Stability::Stable)
        .ok_or_else(|| anyhow::anyhow!("No stable version found for {name}"))?;

    Ok(PackageInfo {
        name: name.to_string(),
        version: best.version.clone(),
        version_normalized: best.version_normalized.clone(),
        description: best.description.as_deref().unwrap_or("").to_string(),
    })
}

// ─── Classification ──────────────────────────────────────────────────────────

/// Determine the update category for a package.
///
/// - If latest <= current → UpToDate
/// - If root constraint exists and latest matches it → SemverCompatible (red)
/// - If root constraint exists but latest doesn't match → SemverIncompatible (yellow)
/// - Fallback (no constraint): same major = compatible, different major = incompatible
fn classify_update(
    current_normalized: &str,
    latest_normalized: &str,
    root_constraint: Option<&str>,
) -> UpdateCategory {
    use mozart_registry::version::compare_normalized_versions;

    // If latest is not newer than current, it's up-to-date
    if compare_normalized_versions(latest_normalized, current_normalized) != Ordering::Greater {
        return UpdateCategory::UpToDate;
    }

    // We have an update available — classify it
    if let Some(constraint_str) = root_constraint
        && let Ok(constraint) = mozart_constraint::VersionConstraint::parse(constraint_str)
        && let Ok(latest_ver) = mozart_constraint::Version::parse(latest_normalized)
    {
        if constraint.matches(&latest_ver) {
            return UpdateCategory::SemverCompatible;
        } else {
            return UpdateCategory::SemverIncompatible;
        }
    }

    // Fallback: no constraint or parse failed — compare major versions
    let current_major = extract_major(current_normalized);
    let latest_major = extract_major(latest_normalized);
    if current_major == latest_major {
        UpdateCategory::SemverCompatible
    } else {
        UpdateCategory::SemverIncompatible
    }
}

/// Extract the major version number from a normalized version string like "1.2.3.0".
fn extract_major(version_normalized: &str) -> u64 {
    let base = if let Some(pos) = version_normalized.find('-') {
        &version_normalized[..pos]
    } else {
        version_normalized
    };
    base.split('.')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
}

/// Extract the minor version number from a normalized version string like "1.2.3.0".
fn extract_minor(version_normalized: &str) -> u64 {
    let base = if let Some(pos) = version_normalized.find('-') {
        &version_normalized[..pos]
    } else {
        version_normalized
    };
    base.split('.')
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
}

/// Extract the patch version number from a normalized version string like "1.2.3.0".
fn extract_patch(version_normalized: &str) -> u64 {
    let base = if let Some(pos) = version_normalized.find('-') {
        &version_normalized[..pos]
    } else {
        version_normalized
    };
    base.split('.')
        .nth(2)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
}

// ─── Level filtering ─────────────────────────────────────────────────────────

/// Check whether a version change passes the --major-only/--minor-only/--patch-only filter.
///
/// Returns true if the version change matches the requested level.
fn passes_level_filter(args: &OutdatedArgs, current: &str, latest: &str) -> bool {
    let cur_major = extract_major(current);
    let lat_major = extract_major(latest);
    let cur_minor = extract_minor(current);
    let lat_minor = extract_minor(latest);
    let cur_patch = extract_patch(current);
    let lat_patch = extract_patch(latest);

    if args.major_only {
        return lat_major > cur_major;
    }
    if args.minor_only {
        return lat_major == cur_major && lat_minor > cur_minor;
    }
    if args.patch_only {
        return lat_major == cur_major && lat_minor == cur_minor && lat_patch > cur_patch;
    }

    // No level filter active
    true
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render_text(entries: &[OutdatedEntry]) {
    if entries.is_empty() {
        println!(
            "{}",
            mozart_core::console::info("All packages are up to date.")
        );
        return;
    }

    // Compute column widths
    let name_width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let cur_width = entries
        .iter()
        .map(|e| e.current_version.len())
        .max()
        .unwrap_or(0);
    let lat_width = entries
        .iter()
        .map(|e| e.latest_version.len())
        .max()
        .unwrap_or(0);

    for entry in entries {
        let name_col = format!("{:<width$}", entry.name, width = name_width);
        let cur_col = format!("{:<width$}", entry.current_version, width = cur_width);
        let lat_col = format!("{:<width$}", entry.latest_version, width = lat_width);

        let (name_str, lat_str) = match entry.category {
            UpdateCategory::UpToDate => (
                mozart_core::console::info(&name_col).to_string(),
                mozart_core::console::info(&lat_col).to_string(),
            ),
            UpdateCategory::SemverCompatible => (
                mozart_core::console::highlight(&name_col).to_string(),
                mozart_core::console::highlight(&lat_col).to_string(),
            ),
            UpdateCategory::SemverIncompatible => (
                mozart_core::console::comment(&name_col).to_string(),
                mozart_core::console::comment(&lat_col).to_string(),
            ),
        };

        println!(
            "{} {} {} {}",
            name_str,
            mozart_core::console::comment(&cur_col),
            lat_str,
            entry.description
        );
    }
}

fn render_json(entries: &[OutdatedEntry]) -> anyhow::Result<()> {
    let json_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let status = match entry.category {
                UpdateCategory::UpToDate => "up-to-date",
                UpdateCategory::SemverCompatible => "semver-safe-update",
                UpdateCategory::SemverIncompatible => "update-possible",
            };
            serde_json::json!({
                "name": entry.name,
                "version": entry.current_version,
                "latest": entry.latest_version,
                "latest-status": status,
                "description": entry.description,
                "direct-dependency": entry.is_direct,
            })
        })
        .collect();

    let output = serde_json::json!({ "installed": json_entries });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Returns true if the given package name is a platform package (php, ext-*, etc.).
fn is_platform_package(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "php"
        || lower.starts_with("ext-")
        || lower.starts_with("lib-")
        || lower == "php-64bit"
        || lower == "php-ipv6"
        || lower == "php-zts"
        || lower == "php-debug"
        || lower == "composer-plugin-api"
        || lower == "composer-runtime-api"
}

/// Simple version normalizer fallback when `version_normalized` is absent.
/// Strips leading 'v' and appends '.0' segments to reach 4 parts.
fn normalize_version_simple(version: &str) -> String {
    let v = version.strip_prefix('v').unwrap_or(version);
    // Split off pre-release suffix
    let (base, suffix) = if let Some(pos) = v.find('-') {
        (&v[..pos], Some(&v[pos..]))
    } else {
        (v, None)
    };
    let parts: Vec<&str> = base.split('.').collect();
    let mut segments: Vec<String> = parts.iter().take(4).map(|p| p.to_string()).collect();
    while segments.len() < 4 {
        segments.push("0".to_string());
    }
    let mut result = segments.join(".");
    if let Some(suf) = suffix {
        result.push_str(suf);
    }
    result
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_update ──────────────────────────────────────────────────────

    #[test]
    fn test_classify_up_to_date_equal() {
        let cat = classify_update("1.2.3.0", "1.2.3.0", None);
        assert_eq!(cat, UpdateCategory::UpToDate);
    }

    #[test]
    fn test_classify_up_to_date_latest_older() {
        let cat = classify_update("2.0.0.0", "1.5.0.0", None);
        assert_eq!(cat, UpdateCategory::UpToDate);
    }

    #[test]
    fn test_classify_semver_compatible_with_constraint() {
        // Current 1.2.0, latest 1.3.0, constraint ^1.0 — latest matches constraint
        let cat = classify_update("1.2.0.0", "1.3.0.0", Some("^1.0"));
        assert_eq!(cat, UpdateCategory::SemverCompatible);
    }

    #[test]
    fn test_classify_semver_incompatible_with_constraint() {
        // Current 1.2.0, latest 2.0.0, constraint ^1.0 — latest doesn't match
        let cat = classify_update("1.2.0.0", "2.0.0.0", Some("^1.0"));
        assert_eq!(cat, UpdateCategory::SemverIncompatible);
    }

    #[test]
    fn test_classify_no_constraint_same_major() {
        // No constraint, same major → SemverCompatible
        let cat = classify_update("1.2.0.0", "1.5.0.0", None);
        assert_eq!(cat, UpdateCategory::SemverCompatible);
    }

    #[test]
    fn test_classify_no_constraint_different_major() {
        // No constraint, different major → SemverIncompatible
        let cat = classify_update("1.9.0.0", "2.0.0.0", None);
        assert_eq!(cat, UpdateCategory::SemverIncompatible);
    }

    #[test]
    fn test_classify_no_constraint_patch_update() {
        // No constraint, same major.minor, patch bump → SemverCompatible
        let cat = classify_update("1.2.3.0", "1.2.4.0", None);
        assert_eq!(cat, UpdateCategory::SemverCompatible);
    }

    // ── passes_level_filter ──────────────────────────────────────────────────

    fn make_args_with_filter(major: bool, minor: bool, patch: bool) -> OutdatedArgs {
        OutdatedArgs {
            package: None,
            outdated: false,
            all: false,
            locked: false,
            direct: false,
            strict: false,
            major_only: major,
            minor_only: minor,
            patch_only: patch,
            sort_by_age: false,
            format: None,
            ignore: vec![],
            no_dev: false,
            ignore_platform_req: vec![],
            ignore_platform_reqs: false,
        }
    }

    #[test]
    fn test_passes_level_filter_no_filter() {
        let args = make_args_with_filter(false, false, false);
        assert!(passes_level_filter(&args, "1.0.0.0", "2.0.0.0"));
        assert!(passes_level_filter(&args, "1.0.0.0", "1.1.0.0"));
        assert!(passes_level_filter(&args, "1.0.0.0", "1.0.1.0"));
    }

    #[test]
    fn test_passes_level_filter_major_only() {
        let args = make_args_with_filter(true, false, false);
        // Major bump: 1 → 2
        assert!(passes_level_filter(&args, "1.0.0.0", "2.0.0.0"));
        // Minor bump: same major
        assert!(!passes_level_filter(&args, "1.0.0.0", "1.1.0.0"));
        // Patch bump: same major
        assert!(!passes_level_filter(&args, "1.0.0.0", "1.0.1.0"));
    }

    #[test]
    fn test_passes_level_filter_minor_only() {
        let args = make_args_with_filter(false, true, false);
        // Major bump: different major, not a minor-only bump
        assert!(!passes_level_filter(&args, "1.0.0.0", "2.0.0.0"));
        // Minor bump: same major, different minor
        assert!(passes_level_filter(&args, "1.0.0.0", "1.1.0.0"));
        // Patch bump: same major+minor
        assert!(!passes_level_filter(&args, "1.0.0.0", "1.0.1.0"));
    }

    #[test]
    fn test_passes_level_filter_patch_only() {
        let args = make_args_with_filter(false, false, true);
        // Major bump
        assert!(!passes_level_filter(&args, "1.0.0.0", "2.0.0.0"));
        // Minor bump
        assert!(!passes_level_filter(&args, "1.0.0.0", "1.1.0.0"));
        // Patch bump: same major+minor, different patch
        assert!(passes_level_filter(&args, "1.0.0.0", "1.0.1.0"));
        // Patch same: not a bump
        assert!(!passes_level_filter(&args, "1.0.1.0", "1.0.1.0"));
    }

    // ── normalize_version_simple ──────────────────────────────────────────────

    #[test]
    fn test_normalize_version_simple_short() {
        assert_eq!(normalize_version_simple("1.2"), "1.2.0.0");
    }

    #[test]
    fn test_normalize_version_simple_three_parts() {
        assert_eq!(normalize_version_simple("1.2.3"), "1.2.3.0");
    }

    #[test]
    fn test_normalize_version_simple_four_parts() {
        assert_eq!(normalize_version_simple("1.2.3.4"), "1.2.3.4");
    }

    #[test]
    fn test_normalize_version_simple_v_prefix() {
        assert_eq!(normalize_version_simple("v1.2.3"), "1.2.3.0");
    }

    #[test]
    fn test_normalize_version_simple_with_prerelease() {
        assert_eq!(normalize_version_simple("1.2.3-beta1"), "1.2.3.0-beta1");
    }

    // ── extract_major/minor/patch ─────────────────────────────────────────────

    #[test]
    fn test_extract_major() {
        assert_eq!(extract_major("2.3.4.0"), 2);
        assert_eq!(extract_major("0.1.2.0"), 0);
        assert_eq!(extract_major("2.3.4.0-beta1"), 2);
    }

    #[test]
    fn test_extract_minor() {
        assert_eq!(extract_minor("2.3.4.0"), 3);
        assert_eq!(extract_minor("1.0.0.0"), 0);
    }

    #[test]
    fn test_extract_patch() {
        assert_eq!(extract_patch("2.3.4.0"), 4);
        assert_eq!(extract_patch("1.2.0.0"), 0);
    }

    // ── is_platform_package ───────────────────────────────────────────────────

    #[test]
    fn test_is_platform_package() {
        assert!(is_platform_package("php"));
        assert!(is_platform_package("ext-json"));
        assert!(is_platform_package("lib-pcre"));
        assert!(is_platform_package("composer-plugin-api"));
        assert!(!is_platform_package("monolog/monolog"));
        assert!(!is_platform_package("psr/log"));
    }

    // ── render_json (smoke test with no network) ──────────────────────────────

    #[test]
    fn test_render_json_empty() {
        // Should succeed without error on empty input
        render_json(&[]).unwrap();
    }

    #[test]
    fn test_render_json_with_entries() {
        let entries = vec![
            OutdatedEntry {
                name: "monolog/monolog".to_string(),
                current_version: "3.0.0".to_string(),
                latest_version: "3.8.0".to_string(),
                description: "A logging library".to_string(),
                category: UpdateCategory::SemverCompatible,
                is_direct: true,
            },
            OutdatedEntry {
                name: "psr/log".to_string(),
                current_version: "2.0.0".to_string(),
                latest_version: "3.0.0".to_string(),
                description: "PSR-3 logging interface".to_string(),
                category: UpdateCategory::SemverIncompatible,
                is_direct: false,
            },
        ];
        render_json(&entries).unwrap();
    }
}
