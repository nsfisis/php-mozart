//! Shared logic for `depends` and `prohibits` commands.
//!
//! `depends` (aka `why`) answers: "Which packages require package X?"
//! `prohibits` (aka `why-not`) answers: "Which packages prevent version X of package Y from being
//! installed?"

use anyhow::Result;
use indexmap::IndexSet;
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::console_writeln;
use std::path::Path;

/// Inputs for [`do_execute`], collected from the `depends` / `prohibits` CLI args.
pub struct DoExecuteArgs<'a> {
    pub package: &'a str,
    /// Version constraint string (only set for `prohibits`).
    pub version: Option<&'a str>,
    pub recursive: bool,
    pub tree: bool,
    pub locked: bool,
    /// `true` for `prohibits` (why-not), `false` for `depends` (why).
    pub inverted: bool,
}

/// Shared implementation for `depends` (why) and `prohibits` (why-not).
///
/// Mirrors `BaseDependencyCommand::doExecute` in Composer: a single function
/// driven by `inverted` to switch between "who depends on X?" and
/// "who prevents X version V from being installed?".
pub fn do_execute(
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    args: DoExecuteArgs<'_>,
) -> Result<()> {
    let DoExecuteArgs {
        package,
        version,
        recursive,
        tree,
        locked,
        inverted,
    } = args;

    let working_dir = cli.working_dir()?;

    let packages = load_packages(&working_dir, locked)?;

    if packages.is_empty() {
        io.lock().unwrap().write_error(
            "No dependencies installed. Try running mozart install or update, or use --locked.",
        );
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    let target = package.to_lowercase();

    let target_known = packages.iter().any(|p| p.name.to_lowercase() == target);
    if !target_known {
        if !inverted && mozart_core::platform::is_platform_package(&target) {
            anyhow::bail!(
                "Could not find platform package \"{}\". Is PHP available?",
                package
            );
        }
        anyhow::bail!("Could not find package \"{}\" in your project", package);
    }

    let constraint = match version {
        Some(v) => Some(
            mozart_semver::VersionConstraint::parse(v)
                .map_err(|e| anyhow::anyhow!("Invalid version constraint '{}': {}", v, e))?,
        ),
        None => None,
    };

    let recursive = tree || recursive;
    let needles = vec![target];

    let results = get_dependents(
        &packages,
        &needles,
        constraint.as_ref(),
        inverted,
        recursive,
    )?;

    if results.is_empty() {
        if inverted {
            console_writeln!(
                io,
                "<info>{} {} can be installed.</info>",
                package,
                version.unwrap_or(""),
            );
            return Ok(());
        }
        io.lock().unwrap().info(&format!(
            "There is no installed package depending on \"{}\"",
            package
        ));
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    if tree {
        print_tree(&results, 0, io.clone());
    } else {
        print_table(&results, io.clone());
    }

    if !inverted {
        return Ok(());
    }

    // Resolution hint: pick the right composer command based on whether the
    // package sits in root's `require`, `require-dev`, or neither.
    let needle_lower = package.to_lowercase();
    let composer_command = packages
        .iter()
        .find(|p| p.is_root)
        .map(|root| {
            if root
                .require
                .keys()
                .any(|k| k.to_lowercase() == needle_lower)
            {
                "require"
            } else if root
                .require_dev
                .keys()
                .any(|k| k.to_lowercase() == needle_lower)
            {
                "require --dev"
            } else {
                "update"
            }
        })
        .unwrap_or("update");

    io.lock().unwrap().info(&format!(
        "Not finding what you were looking for? Try calling `composer {} \"{}:{}\" --dry-run` to get another view on the problem.",
        composer_command,
        package,
        version.unwrap_or("")
    ));

    Err(mozart_core::exit_code::bail_silent(
        mozart_core::exit_code::GENERAL_ERROR,
    ))
}

/// Normalised view of a package's dependency information.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    /// Runtime requirements (`require` section).
    pub require: indexmap::IndexMap<String, String>,
    /// Dev requirements (`require-dev`) — only non-empty for the root package.
    pub require_dev: indexmap::IndexMap<String, String>,
    /// Conflict declarations (`conflict` section).
    pub conflict: indexmap::IndexMap<String, String>,
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

/// Load all packages relevant to the dependency query.
///
/// When `locked` is true (or the lock file exists), reads from `composer.lock`.
/// Otherwise falls back to `vendor/composer/installed.json`.
/// The root `composer.json` is always added as a synthetic entry.
pub fn load_packages(working_dir: &Path, locked: bool) -> Result<Vec<PackageInfo>> {
    let lock_path = working_dir.join("composer.lock");
    let composer_json_path = working_dir.join("composer.json");

    // Load locked / installed packages
    let mut packages: Vec<PackageInfo> = if locked {
        load_from_lockfile(&lock_path)?
    } else {
        let installed = load_from_installed(working_dir);
        match installed {
            Ok(pkgs) if !pkgs.is_empty() => pkgs,
            _ => {
                if lock_path.exists() {
                    load_from_lockfile(&lock_path)?
                } else {
                    vec![]
                }
            }
        }
    };

    // Add platform packages (php, ext-*, lib-*, composer-*-api)
    let platform = mozart_core::platform::detect_platform();
    for pp in &platform {
        packages.push(PackageInfo {
            name: pp.name.clone(),
            version: pp.version.clone(),
            require: indexmap::IndexMap::new(),
            require_dev: indexmap::IndexMap::new(),
            conflict: indexmap::IndexMap::new(),
            is_root: false,
        });
    }

    // Add the root package (composer.json) as a synthetic entry
    if composer_json_path.exists()
        && let Ok(root) = mozart_core::package::read_from_file(&composer_json_path)
    {
        // Extract conflict from extra_fields if present
        let conflict = root
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
    let lock = mozart_core::repository::lockfile::LockFile::read_from_file(lock_path)?;

    let mut packages: Vec<PackageInfo> = Vec::new();

    for pkg in &lock.packages {
        packages.push(PackageInfo {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            require: pkg.require.clone(),
            require_dev: indexmap::IndexMap::new(), // locked packages don't expose require-dev
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
                require_dev: indexmap::IndexMap::new(),
                conflict: pkg.conflict.clone(),
                is_root: false,
            });
        }
    }

    Ok(packages)
}

fn load_from_installed(working_dir: &Path) -> Result<Vec<PackageInfo>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?;

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
                require_dev: indexmap::IndexMap::new(),
                conflict,
                is_root: false,
            }
        })
        .collect();

    Ok(packages)
}

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
    constraint: Option<&mozart_semver::VersionConstraint>,
    inverted: bool,
    recursive: bool,
) -> Result<Vec<DependencyResult>> {
    if inverted {
        get_prohibitors(packages, needles, constraint, recursive)
    } else {
        get_dependents_forward(packages, needles, recursive)
    }
}

fn get_dependents_forward(
    packages: &[PackageInfo],
    needles: &[String],
    recursive: bool,
) -> Result<Vec<DependencyResult>> {
    let needle_set: IndexSet<String> = needles.iter().map(|n| n.to_lowercase()).collect();

    // Build name→PackageInfo lookup
    let pkg_map: indexmap::IndexMap<_, _> = packages
        .iter()
        .map(|p| (p.name.to_lowercase(), p))
        .collect();

    if recursive {
        // Recursive: BFS from needles upward to root, building a tree
        let mut visited: IndexSet<String> = IndexSet::new();
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
    pkg_map: &indexmap::IndexMap<String, &PackageInfo>,
    visited: &mut IndexSet<String>,
    _original_needles: &IndexSet<String>,
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

fn get_prohibitors(
    packages: &[PackageInfo],
    needles: &[String],
    constraint: Option<&mozart_semver::VersionConstraint>,
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
                    mozart_semver::VersionConstraint::parse(req_constraint_str)
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
                    mozart_semver::VersionConstraint::parse(req_constraint_str)
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
                    mozart_semver::VersionConstraint::parse(conflict_constraint_str)
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
    requested: &mozart_semver::VersionConstraint,
    pkg_constraint: &mozart_semver::VersionConstraint,
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
    requested: &mozart_semver::VersionConstraint,
    conflict_constraint: &mozart_semver::VersionConstraint,
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
    constraint: &mozart_semver::VersionConstraint,
) -> Vec<mozart_semver::Version> {
    use mozart_semver::Version;

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

/// Print results as a flat table.
///
/// Columns: package name | version | link description | link constraint
pub fn print_table(
    results: &[DependencyResult],
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) {
    if results.is_empty() {
        console_writeln!(io, "<info>No relationships found.</info>");
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

    let mut seen: IndexSet<String> = IndexSet::new();
    for r in results {
        let key = format!(
            "{}|{}|{}|{}",
            r.package_name, r.package_version, r.link_description, r.link_constraint
        );
        if !seen.insert(key) {
            continue;
        }
        console_writeln!(
            io,
            "{:<name_w$}  {:<ver_w$}  {:<desc_w$}  {}",
            console_format!("<info>{}</info>", r.package_name),
            console_format!("<comment>{}</comment>", r.package_version),
            r.link_description,
            console_format!("<comment>{}</comment>", r.link_constraint),
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
pub fn print_tree(
    results: &[DependencyResult],
    depth: usize,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) {
    if results.is_empty() && depth == 0 {
        console_writeln!(io, "<info>No relationships found.</info>");
        return;
    }

    let count = results.len();
    for (i, r) in results.iter().enumerate() {
        let is_last = i + 1 == count;
        let prefix = tree_prefix(depth, is_last);

        console_writeln!(
            io,
            "{}{:<}  {}  {}  {}",
            prefix,
            console_format!("<info>{}</info>", r.package_name),
            console_format!("<comment>{}</comment>", r.package_version),
            r.link_description,
            console_format!("<comment>{}</comment>", r.link_constraint),
        );

        if !r.children.is_empty() {
            print_tree(&r.children, depth + 1, io.clone());
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
