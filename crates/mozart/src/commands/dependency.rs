//! Shared logic for `depends` and `prohibits` commands.
//!
//! `depends` (aka `why`) answers: "Which packages require package X?"
//! `prohibits` (aka `why-not`) answers: "Which packages prevent version X of package Y from being
//! installed?"

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::Result;

// ─────────────────────────────────────────────────────────────────────────────
// Core types
// ─────────────────────────────────────────────────────────────────────────────

/// Normalised view of a package's dependency information.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    /// Runtime requirements (`require` section).
    pub require: BTreeMap<String, String>,
    /// Dev requirements (`require-dev`) — only non-empty for the root package.
    pub require_dev: BTreeMap<String, String>,
    /// Conflict declarations (`conflict` section).
    pub conflict: BTreeMap<String, String>,
    /// Whether this is the root `composer.json` package.
    pub is_root: bool,
}

/// A single result node in the dependency graph walk.
#[derive(Debug, Clone)]
pub struct DependencyResult {
    /// Name of the package that has the link.
    pub package_name: String,
    /// Version of the package that has the link.
    pub package_version: String,
    /// Human-readable link type: "requires", "requires (dev)", or "conflicts".
    pub link_description: String,
    /// The target package name (the one being queried).
    pub link_target: String,
    /// The constraint string from the link (e.g. "^1.0").
    pub link_constraint: String,
    /// Children found during a recursive walk (empty for flat results).
    pub children: Vec<DependencyResult>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Package loading
// ─────────────────────────────────────────────────────────────────────────────

/// Load all packages relevant to the dependency query.
///
/// When `locked` is true (or the lock file exists), reads from `composer.lock`.
/// Otherwise falls back to `vendor/composer/installed.json`.
/// The root `composer.json` is always added as a synthetic entry.
pub fn load_packages(working_dir: &Path, locked: bool) -> Result<Vec<PackageInfo>> {
    let lock_path = working_dir.join("composer.lock");
    let composer_json_path = working_dir.join("composer.json");

    // Load locked / installed packages
    let mut packages: Vec<PackageInfo> = if locked || lock_path.exists() {
        load_from_lockfile(&lock_path)?
    } else {
        load_from_installed(working_dir)?
    };

    // Add the root package (composer.json) as a synthetic entry
    if composer_json_path.exists()
        && let Ok(root) = mozart_core::package::read_from_file(&composer_json_path)
    {
        // Extract conflict from extra_fields if present
        let conflict: BTreeMap<String, String> = root
            .extra_fields
            .get("conflict")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        packages.push(PackageInfo {
            name: root.name.clone(),
            version: "ROOT".to_string(),
            require: root.require,
            require_dev: root.require_dev,
            conflict,
            is_root: true,
        });
    }

    Ok(packages)
}

fn load_from_lockfile(lock_path: &Path) -> Result<Vec<PackageInfo>> {
    if !lock_path.exists() {
        anyhow::bail!("composer.lock not found — run `mozart install` first or omit --locked");
    }
    let lock = mozart_registry::lockfile::LockFile::read_from_file(lock_path)?;

    let mut packages: Vec<PackageInfo> = Vec::new();

    for pkg in &lock.packages {
        packages.push(PackageInfo {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            require: pkg.require.clone(),
            require_dev: BTreeMap::new(), // locked packages don't expose require-dev
            conflict: pkg.conflict.clone(),
            is_root: false,
        });
    }

    if let Some(ref dev_pkgs) = lock.packages_dev {
        for pkg in dev_pkgs {
            packages.push(PackageInfo {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                require: pkg.require.clone(),
                require_dev: BTreeMap::new(),
                conflict: pkg.conflict.clone(),
                is_root: false,
            });
        }
    }

    Ok(packages)
}

fn load_from_installed(working_dir: &Path) -> Result<Vec<PackageInfo>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    let packages = installed
        .packages
        .iter()
        .map(|p| {
            // InstalledPackageEntry uses extra_fields for require/conflict; we do a best-effort
            // extraction since installed.json doesn't always carry full dep info.
            let require = p
                .extra_fields
                .get("require")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();

            let conflict = p
                .extra_fields
                .get("conflict")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();

            PackageInfo {
                name: p.name.clone(),
                version: p.version.clone(),
                require,
                require_dev: BTreeMap::new(),
                conflict,
                is_root: false,
            }
        })
        .collect();

    Ok(packages)
}

// ─────────────────────────────────────────────────────────────────────────────
// Core algorithm
// ─────────────────────────────────────────────────────────────────────────────

/// Find all packages that have a dependency relationship with the needle(s).
///
/// * `packages`   — the full set of packages to search through.
/// * `needles`    — package names to look for (usually just one).
/// * `constraint` — when `Some`, used for the `prohibits` check (see below).
/// * `inverted`   — if `true`, run the "prohibits" logic instead of "depends":
///     - A package *prohibits* version V of package P if:
///       a) it **requires** P with a constraint that does NOT match V, OR
///       b) it **conflicts** with P at a constraint that matches V.
/// * `recursive`  — walk transitively up to the root.
pub fn get_dependents(
    packages: &[PackageInfo],
    needles: &[String],
    constraint: Option<&mozart_constraint::VersionConstraint>,
    inverted: bool,
    recursive: bool,
) -> Result<Vec<DependencyResult>> {
    if inverted {
        get_prohibitors(packages, needles, constraint, recursive)
    } else {
        get_dependents_forward(packages, needles, recursive)
    }
}

// ── Forward (depends) ─────────────────────────────────────────────────────────

fn get_dependents_forward(
    packages: &[PackageInfo],
    needles: &[String],
    recursive: bool,
) -> Result<Vec<DependencyResult>> {
    let needle_set: HashSet<String> = needles.iter().map(|n| n.to_lowercase()).collect();

    // Build name→PackageInfo lookup
    let pkg_map: BTreeMap<String, &PackageInfo> = packages
        .iter()
        .map(|p| (p.name.to_lowercase(), p))
        .collect();

    if recursive {
        // Recursive: BFS from needles upward to root, building a tree
        let mut visited: HashSet<String> = HashSet::new();
        let mut results: Vec<DependencyResult> = Vec::new();

        for needle in needles {
            let needle_lower = needle.to_lowercase();
            let direct = collect_direct_requires(packages, &needle_lower);
            for mut result in direct {
                let pkg_lower = result.package_name.to_lowercase();
                if visited.insert(pkg_lower.clone()) {
                    // Recurse: who requires this package?
                    result.children = recurse_dependents(
                        packages,
                        &pkg_lower,
                        &pkg_map,
                        &mut visited,
                        &needle_set,
                    );
                    results.push(result);
                }
            }
        }
        Ok(results)
    } else {
        // Flat: just direct dependents
        let mut results: Vec<DependencyResult> = Vec::new();
        for needle in needles {
            let needle_lower = needle.to_lowercase();
            results.extend(collect_direct_requires(packages, &needle_lower));
        }
        Ok(results)
    }
}

/// Collect all packages that directly require `needle`.
fn collect_direct_requires(packages: &[PackageInfo], needle: &str) -> Vec<DependencyResult> {
    let mut results = Vec::new();
    for pkg in packages {
        // Check `require`
        if let Some((target, constraint)) =
            pkg.require.iter().find(|(k, _)| k.to_lowercase() == needle)
        {
            results.push(DependencyResult {
                package_name: pkg.name.clone(),
                package_version: pkg.version.clone(),
                link_description: "requires".to_string(),
                link_target: target.clone(),
                link_constraint: constraint.clone(),
                children: vec![],
            });
        }
        // Check `require-dev` (root package only)
        if pkg.is_root
            && let Some((target, constraint)) = pkg
                .require_dev
                .iter()
                .find(|(k, _)| k.to_lowercase() == needle)
        {
            results.push(DependencyResult {
                package_name: pkg.name.clone(),
                package_version: pkg.version.clone(),
                link_description: "requires (dev)".to_string(),
                link_target: target.clone(),
                link_constraint: constraint.clone(),
                children: vec![],
            });
        }
    }
    results
}

/// Recursively find who requires `needle` (used by recursive depends).
fn recurse_dependents(
    packages: &[PackageInfo],
    needle: &str,
    pkg_map: &BTreeMap<String, &PackageInfo>,
    visited: &mut HashSet<String>,
    _original_needles: &HashSet<String>,
) -> Vec<DependencyResult> {
    let _ = pkg_map; // kept for potential future use
    let direct = collect_direct_requires(packages, needle);
    let mut results = Vec::new();
    for mut result in direct {
        let pkg_lower = result.package_name.to_lowercase();
        if visited.insert(pkg_lower.clone()) {
            result.children =
                recurse_dependents(packages, &pkg_lower, pkg_map, visited, _original_needles);
            results.push(result);
        }
    }
    results
}

// ── Inverted (prohibits) ──────────────────────────────────────────────────────

fn get_prohibitors(
    packages: &[PackageInfo],
    needles: &[String],
    constraint: Option<&mozart_constraint::VersionConstraint>,
    _recursive: bool,
) -> Result<Vec<DependencyResult>> {
    let mut results: Vec<DependencyResult> = Vec::new();

    for needle in needles {
        let needle_lower = needle.to_lowercase();
        for pkg in packages {
            // Case 1: package requires the needle, but the required constraint
            // does NOT match the requested version (i.e. it would reject it).
            if let Some((target, req_constraint_str)) = pkg
                .require
                .iter()
                .find(|(k, _)| k.to_lowercase() == needle_lower)
                && let Some(requested_version) = constraint
                && let Ok(pkg_constraint) =
                    mozart_constraint::VersionConstraint::parse(req_constraint_str)
            {
                // The package requires `needle` but with a different
                // (incompatible) constraint — it blocks the requested version.
                // We check: does any version satisfying the requested constraint
                // NOT satisfy the package's constraint?
                if constraint_prohibits(requested_version, &pkg_constraint) {
                    results.push(DependencyResult {
                        package_name: pkg.name.clone(),
                        package_version: pkg.version.clone(),
                        link_description: "requires".to_string(),
                        link_target: target.clone(),
                        link_constraint: req_constraint_str.clone(),
                        children: vec![],
                    });
                }
            }

            // Also check require-dev for root
            if pkg.is_root
                && let Some((target, req_constraint_str)) = pkg
                    .require_dev
                    .iter()
                    .find(|(k, _)| k.to_lowercase() == needle_lower)
                && let Some(requested_version) = constraint
                && let Ok(pkg_constraint) =
                    mozart_constraint::VersionConstraint::parse(req_constraint_str)
                && constraint_prohibits(requested_version, &pkg_constraint)
            {
                results.push(DependencyResult {
                    package_name: pkg.name.clone(),
                    package_version: pkg.version.clone(),
                    link_description: "requires (dev)".to_string(),
                    link_target: target.clone(),
                    link_constraint: req_constraint_str.clone(),
                    children: vec![],
                });
            }

            // Case 2: package *conflicts* with the needle at a version that
            // overlaps with the requested version (i.e. the conflict blocks it).
            if let Some((target, conflict_constraint_str)) = pkg
                .conflict
                .iter()
                .find(|(k, _)| k.to_lowercase() == needle_lower)
                && let Some(requested_version) = constraint
                && let Ok(conflict_constraint) =
                    mozart_constraint::VersionConstraint::parse(conflict_constraint_str)
            {
                // If the conflict constraint overlaps with (matches) the
                // requested version range, this package conflicts with it.
                if constraint_overlaps(requested_version, &conflict_constraint) {
                    results.push(DependencyResult {
                        package_name: pkg.name.clone(),
                        package_version: pkg.version.clone(),
                        link_description: "conflicts".to_string(),
                        link_target: target.clone(),
                        link_constraint: conflict_constraint_str.clone(),
                        children: vec![],
                    });
                }
            }
        }
    }

    Ok(results)
}

/// Returns `true` if `requested` (the version the user wants to install) is
/// **not** matched by `pkg_constraint` (the constraint the installed package
/// requires), meaning the installed package would block installation.
///
/// We sample a set of "representative versions" from the requested constraint
/// and check whether none of them satisfy the package's constraint.
fn constraint_prohibits(
    requested: &mozart_constraint::VersionConstraint,
    pkg_constraint: &mozart_constraint::VersionConstraint,
) -> bool {
    // We try to determine if there is any version satisfying *requested* that
    // does NOT satisfy *pkg_constraint*.
    // Strategy: collect "probe" versions that the requested constraint implies,
    // then check if any probe is rejected by pkg_constraint.
    let probes = sample_versions_from_constraint(requested);
    if probes.is_empty() {
        // Cannot determine — report as prohibiting
        return true;
    }
    // If ANY probe satisfies the requested constraint but NOT pkg_constraint → prohibits
    probes
        .iter()
        .any(|v| requested.matches(v) && !pkg_constraint.matches(v))
}

/// Returns `true` if the conflict constraint overlaps with the requested version.
/// That is, if the conflict constraint matches at least one version that the
/// requested constraint also matches.
fn constraint_overlaps(
    requested: &mozart_constraint::VersionConstraint,
    conflict_constraint: &mozart_constraint::VersionConstraint,
) -> bool {
    let probes = sample_versions_from_constraint(requested);
    if probes.is_empty() {
        return true;
    }
    probes
        .iter()
        .any(|v| requested.matches(v) && conflict_constraint.matches(v))
}

/// Generate a small set of concrete `Version` values that probe the shape of a
/// constraint.  These are used for the "does this constraint overlap/prohibit
/// that constraint?" heuristic.
fn sample_versions_from_constraint(
    constraint: &mozart_constraint::VersionConstraint,
) -> Vec<mozart_constraint::Version> {
    use mozart_constraint::Version;

    // Broad grid of versions to probe
    let candidates: &[&str] = &[
        "0.0.1",
        "0.1.0",
        "0.9.0",
        "1.0.0",
        "1.0.1",
        "1.1.0",
        "1.2.0",
        "1.5.0",
        "1.9.0",
        "1.9.9",
        "2.0.0",
        "2.1.0",
        "2.5.0",
        "2.9.9",
        "3.0.0",
        "3.1.0",
        "3.9.9",
        "4.0.0",
        "4.9.0",
        "5.0.0",
        "6.0.0",
        "7.0.0",
        "8.0.0",
        "9.0.0",
        "10.0.0",
        "0.0.1-alpha1",
        "1.0.0-beta1",
        "1.0.0-RC1",
    ];

    candidates
        .iter()
        .filter_map(|s| Version::parse(s).ok())
        .filter(|v| constraint.matches(v))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Output helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Print results as a flat table.
///
/// Columns: package name | version | link description | link constraint
pub fn print_table(results: &[DependencyResult]) {
    if results.is_empty() {
        println!("{}", mozart_core::console::info("No relationships found."));
        return;
    }

    // Column widths
    let name_w = results
        .iter()
        .map(|r| r.package_name.len())
        .max()
        .unwrap_or(0);
    let ver_w = results
        .iter()
        .map(|r| r.package_version.len())
        .max()
        .unwrap_or(0);
    let desc_w = results
        .iter()
        .map(|r| r.link_description.len())
        .max()
        .unwrap_or(0);

    for r in results {
        println!(
            "{:<name_w$}  {:<ver_w$}  {:<desc_w$}  {}",
            mozart_core::console::info(&r.package_name),
            mozart_core::console::comment(&r.package_version),
            r.link_description,
            mozart_core::console::comment(&r.link_constraint),
            name_w = name_w,
            ver_w = ver_w,
            desc_w = desc_w,
        );
    }
}

/// Print results as a nested tree using box-drawing characters.
///
/// Example output:
///
/// ```text
/// vendor/a  1.0.0  requires  ^1.0
/// └─ vendor/b  2.0.0  requires  ^2.0
///    └─ root/project  ROOT  requires  ^2.0
/// ```
pub fn print_tree(results: &[DependencyResult], depth: usize) {
    if results.is_empty() && depth == 0 {
        println!("{}", mozart_core::console::info("No relationships found."));
        return;
    }

    let count = results.len();
    for (i, r) in results.iter().enumerate() {
        let is_last = i + 1 == count;
        let prefix = tree_prefix(depth, is_last);

        println!(
            "{}{:<}  {}  {}  {}",
            prefix,
            mozart_core::console::info(&r.package_name),
            mozart_core::console::comment(&r.package_version),
            r.link_description,
            mozart_core::console::comment(&r.link_constraint),
        );

        if !r.children.is_empty() {
            print_tree(&r.children, depth + 1);
        }
    }
}

fn tree_prefix(depth: usize, is_last: bool) -> String {
    if depth == 0 {
        return String::new();
    }
    let indent = "   ".repeat(depth - 1);
    let branch = if is_last { "└─ " } else { "├─ " };
    format!("{indent}{branch}")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pkg(
        name: &str,
        version: &str,
        require: &[(&str, &str)],
        conflict: &[(&str, &str)],
        is_root: bool,
    ) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            require: require
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            require_dev: BTreeMap::new(),
            conflict: conflict
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            is_root,
        }
    }

    // ── depends tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_forward_dependency() {
        // root requires A, A requires B → depends B returns A (and root not A)
        let packages = vec![
            make_pkg("root/project", "ROOT", &[("vendor/a", "^1.0")], &[], true),
            make_pkg("vendor/a", "1.0.0", &[("vendor/b", "^2.0")], &[], false),
            make_pkg("vendor/b", "2.0.0", &[], &[], false),
        ];
        let needles = vec!["vendor/b".to_string()];
        let results = get_dependents(&packages, &needles, None, false, false).unwrap();
        assert_eq!(results.len(), 1, "Only A requires B directly");
        assert_eq!(results[0].package_name, "vendor/a");
        assert_eq!(results[0].link_description, "requires");
        assert_eq!(results[0].link_constraint, "^2.0");
    }

    #[test]
    fn test_recursive_dependency() {
        // root requires A, A requires B → depends B --recursive returns A, with root as child
        let packages = vec![
            make_pkg("root/project", "ROOT", &[("vendor/a", "^1.0")], &[], true),
            make_pkg("vendor/a", "1.0.0", &[("vendor/b", "^2.0")], &[], false),
            make_pkg("vendor/b", "2.0.0", &[], &[], false),
        ];
        let needles = vec!["vendor/b".to_string()];
        let results = get_dependents(&packages, &needles, None, false, true).unwrap();
        // A is found as direct dependent of B
        assert!(!results.is_empty());
        let a_result = results.iter().find(|r| r.package_name == "vendor/a");
        assert!(a_result.is_some(), "vendor/a should be found");
        // root should appear as a child of vendor/a
        let children = &a_result.unwrap().children;
        assert!(
            children.iter().any(|c| c.package_name == "root/project"),
            "root/project should be a child of vendor/a"
        );
    }

    #[test]
    fn test_no_dependents() {
        // Nothing requires X
        let packages = vec![
            make_pkg("root/project", "ROOT", &[("vendor/a", "^1.0")], &[], true),
            make_pkg("vendor/a", "1.0.0", &[], &[], false),
        ];
        let needles = vec!["vendor/x".to_string()];
        let results = get_dependents(&packages, &needles, None, false, false).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_circular_detection() {
        // A requires B, B requires A — should not loop forever
        let packages = vec![
            make_pkg("vendor/a", "1.0.0", &[("vendor/b", "^1.0")], &[], false),
            make_pkg("vendor/b", "1.0.0", &[("vendor/a", "^1.0")], &[], false),
        ];
        let needles = vec!["vendor/b".to_string()];
        // Should terminate without stack overflow
        let results = get_dependents(&packages, &needles, None, false, true).unwrap();
        // vendor/a requires vendor/b → found; vendor/b would recurse back to vendor/a
        // but visited set prevents infinite loop
        assert!(!results.is_empty());
    }

    // ── prohibits tests ───────────────────────────────────────────────────────

    #[test]
    fn test_prohibits_basic() {
        // root requires A ^1.0; user asks "who prohibits A 2.0"
        // → root requires A ^1.0 which doesn't match 2.0 → root prohibits it
        let packages = vec![
            make_pkg("root/project", "ROOT", &[("vendor/a", "^1.0")], &[], true),
            make_pkg("vendor/a", "1.0.0", &[], &[], false),
        ];
        let constraint = mozart_constraint::VersionConstraint::parse("2.0.0").unwrap();
        let needles = vec!["vendor/a".to_string()];
        let results = get_dependents(&packages, &needles, Some(&constraint), true, false).unwrap();
        assert!(!results.is_empty(), "root should prohibit vendor/a 2.0");
        assert_eq!(results[0].package_name, "root/project");
        assert_eq!(results[0].link_description, "requires");
    }

    #[test]
    fn test_prohibits_conflict_field() {
        // pkg/b conflicts with vendor/a ^2.0 → prohibits vendor/a 2.0
        let packages = vec![
            make_pkg(
                "root/project",
                "ROOT",
                &[("vendor/a", "^1.0"), ("vendor/b", "^1.0")],
                &[],
                true,
            ),
            make_pkg("vendor/a", "1.0.0", &[], &[], false),
            make_pkg(
                "vendor/b",
                "1.0.0",
                &[],
                &[("vendor/a", "^2.0")], // conflicts with vendor/a ^2.0
                false,
            ),
        ];
        let constraint = mozart_constraint::VersionConstraint::parse("2.0.0").unwrap();
        let needles = vec!["vendor/a".to_string()];
        let results = get_dependents(&packages, &needles, Some(&constraint), true, false).unwrap();
        // vendor/b conflicts with vendor/a ^2.0 which covers 2.0.0
        let conflict_result = results.iter().find(|r| r.package_name == "vendor/b");
        assert!(
            conflict_result.is_some(),
            "vendor/b should prohibit vendor/a 2.0 via conflict"
        );
        assert_eq!(conflict_result.unwrap().link_description, "conflicts");
    }

    #[test]
    fn test_prohibits_no_issue() {
        // root requires A ^2.0; user asks "who prohibits A 2.5"
        // → root's constraint ^2.0 DOES match 2.5 → nobody prohibits it
        let packages = vec![
            make_pkg("root/project", "ROOT", &[("vendor/a", "^2.0")], &[], true),
            make_pkg("vendor/a", "2.0.0", &[], &[], false),
        ];
        let constraint = mozart_constraint::VersionConstraint::parse("2.5.0").unwrap();
        let needles = vec!["vendor/a".to_string()];
        let results = get_dependents(&packages, &needles, Some(&constraint), true, false).unwrap();
        assert!(
            results.is_empty(),
            "Nobody prohibits vendor/a 2.5 when root requires ^2.0"
        );
    }

    // ── print helpers (smoke tests) ───────────────────────────────────────────

    #[test]
    fn test_print_table_empty() {
        print_table(&[]);
    }

    #[test]
    fn test_print_table_single() {
        let results = vec![DependencyResult {
            package_name: "vendor/a".to_string(),
            package_version: "1.0.0".to_string(),
            link_description: "requires".to_string(),
            link_target: "vendor/b".to_string(),
            link_constraint: "^2.0".to_string(),
            children: vec![],
        }];
        print_table(&results);
    }

    #[test]
    fn test_print_tree_empty() {
        print_tree(&[], 0);
    }

    #[test]
    fn test_print_tree_nested() {
        let results = vec![DependencyResult {
            package_name: "vendor/a".to_string(),
            package_version: "1.0.0".to_string(),
            link_description: "requires".to_string(),
            link_target: "vendor/b".to_string(),
            link_constraint: "^2.0".to_string(),
            children: vec![DependencyResult {
                package_name: "root/project".to_string(),
                package_version: "ROOT".to_string(),
                link_description: "requires".to_string(),
                link_target: "vendor/a".to_string(),
                link_constraint: "^1.0".to_string(),
                children: vec![],
            }],
        }];
        print_tree(&results, 0);
    }
}
