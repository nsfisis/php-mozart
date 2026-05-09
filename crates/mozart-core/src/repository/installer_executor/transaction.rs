//! Transaction computation — lock-vs-installed diff and alias reconciliation.
//!
//! Mirrors `Composer\DependencyResolver\Transaction::calculateOperations` and
//! `Composer\Installer\InstalledFilesystemRepository` (the `ArrayDumper`
//! path). Kept separate so both `install` and `update` commands can share the
//! same operation-computation machinery without going through the `install`
//! command module.

use super::super::installed::{InstalledPackageEntry, InstalledPackages};
use super::super::lockfile::{LockFile, LockedPackage};
use indexmap::IndexSet;
use std::path::Path;

/// The action to take for a package during install.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    Update,
    Skip,
}

/// Compute install operations by comparing locked packages against installed packages.
///
/// Returns `(ops, removals)` where:
/// - `ops`: list of `(package, action)` ordered topologically — every package's
///   lock-internal `require` deps appear before it, matching Composer's
///   `Transaction::calculateOperations`.
/// - `removals`: list of package names that are installed but not locked.
pub fn compute_operations<'a>(
    locked: &[&'a LockedPackage],
    installed: &InstalledPackages,
) -> (Vec<(&'a LockedPackage, Action)>, Vec<String>) {
    let ordered = topological_sort(locked);

    let mut ops: Vec<(&'a LockedPackage, Action)> = Vec::new();
    for pkg in ordered {
        let installed_entry = installed
            .packages
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&pkg.name));
        let action = match installed_entry {
            None => Action::Install,
            Some(entry) if entry.version != pkg.version => Action::Update,
            Some(entry) if !installed_refs_match_locked(entry, pkg) => Action::Update,
            Some(entry) if !installed_abandoned_matches_locked(entry, pkg) => Action::Update,
            Some(_) => Action::Skip,
        };
        ops.push((pkg, action));
    }

    // Compute removals: packages in installed but not in locked. Iterate
    // installed.json in reverse, mirroring Composer's
    // `Transaction::calculateOperations`, which seeds `removeMap` from
    // `presentPackages` in order and then `array_unshift`s each entry onto
    // `operations` — flipping the iteration order.
    let locked_names: IndexSet<String> = locked.iter().map(|p| p.name.to_lowercase()).collect();
    let removals: Vec<String> = installed
        .packages
        .iter()
        .rev()
        .filter(|p| !locked_names.contains(&p.name.to_lowercase()))
        .map(|p| p.name.clone())
        .collect();

    (ops, removals)
}

/// Order a slice of locked packages so every package's `require` deps that
/// are present in the same slice come before it. Mirrors
/// `Composer\DependencyResolver\Transaction::calculateOperations` — the
/// stack-based DFS over the result map.
fn topological_sort<'a>(packages: &[&'a LockedPackage]) -> Vec<&'a LockedPackage> {
    use std::collections::BTreeMap;

    // Reverse-alphabetical sort, mirroring `setResultPackageMaps`.
    let mut sorted: Vec<&'a LockedPackage> = packages.to_vec();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.name.to_lowercase()));

    // Multimap: name → [packages]. A package contributes itself under its
    // own name *and* under every `provide`/`replace` entry.
    let mut resolves: BTreeMap<String, Vec<&'a LockedPackage>> = BTreeMap::new();
    for pkg in &sorted {
        let names = std::iter::once(pkg.name.to_lowercase())
            .chain(pkg.provide.keys().map(|s| s.to_lowercase()))
            .chain(pkg.replace.keys().map(|s| s.to_lowercase()));
        for n in names {
            resolves.entry(n).or_default().push(*pkg);
        }
    }

    // Mirror Composer's `getRootPackages`: walk in sorted order, removing
    // each package's required providers from the candidate-roots set.
    let mut roots_set: IndexSet<String> = sorted.iter().map(|p| p.name.to_lowercase()).collect();
    for pkg in &sorted {
        let pkg_lower = pkg.name.to_lowercase();
        if !roots_set.contains(&pkg_lower) {
            continue;
        }
        for dep in pkg.require.keys() {
            let dep_lower = dep.to_lowercase();
            if let Some(matches) = resolves.get(&dep_lower) {
                for &m in matches {
                    let m_lower = m.name.to_lowercase();
                    if m_lower != pkg_lower {
                        roots_set.shift_remove(&m_lower);
                    }
                }
            }
        }
    }

    let mut stack: Vec<&'a LockedPackage> = sorted
        .iter()
        .filter(|p| roots_set.contains(&p.name.to_lowercase()))
        .copied()
        .collect();

    let mut visited: IndexSet<String> = IndexSet::new();
    let mut processed: IndexSet<String> = IndexSet::new();
    let mut ordered: Vec<&'a LockedPackage> = Vec::with_capacity(packages.len());

    while let Some(pkg) = stack.pop() {
        let lower = pkg.name.to_lowercase();
        if processed.contains(&lower) {
            continue;
        }
        if !visited.contains(&lower) {
            visited.insert(lower);
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

    // Cycle / disconnected fallback: append any leftover packages.
    for pkg in packages {
        let lower = pkg.name.to_lowercase();
        if !processed.contains(&lower) {
            processed.insert(lower);
            ordered.push(*pkg);
        }
    }

    ordered
}

/// Pre-rendered MarkAliasUninstalled operation. Caller pre-computes the
/// display strings so the executor call site stays simple.
pub struct StaleInstalledAlias {
    pub name: String,
    pub alias_full: String,
    pub target_full: String,
}

/// `(package_name_lowercase, alias_pretty)` pairs the *new* lock's packages
/// will surface — used by `compute_stale_installed_aliases` to determine which
/// currently-installed alias packages no longer have a counterpart in the new
/// lock. Mirrors `Locker::getLockedRepository` running every locked package
/// through `ArrayLoader`.
fn lock_alias_pretty_pairs(lock: &LockFile) -> std::collections::HashSet<(String, String)> {
    use std::collections::HashSet;
    let mut set: HashSet<(String, String)> = HashSet::new();
    for a in &lock.aliases {
        set.insert((a.package.to_lowercase(), a.alias.clone()));
    }
    for pkg in lock
        .packages
        .iter()
        .chain(lock.packages_dev.iter().flatten())
    {
        let mut emitted_explicit = false;
        if let Some(map) = pkg
            .extra_fields
            .get("extra")
            .and_then(|e| e.get("branch-alias"))
            .and_then(|b| b.as_object())
        {
            for (source, target) in map {
                if !source.eq_ignore_ascii_case(&pkg.version) {
                    continue;
                }
                let Some(target_str) = target.as_str() else {
                    continue;
                };
                if !target_str.to_lowercase().ends_with("-dev") {
                    continue;
                }
                set.insert((pkg.name.to_lowercase(), target_str.to_string()));
                emitted_explicit = true;
            }
        }
        if emitted_explicit {
            continue;
        }
        let is_default_branch = pkg
            .extra_fields
            .get("default-branch")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !is_default_branch {
            continue;
        }
        let version_lower = pkg.version.to_lowercase();
        let is_dev_branch = version_lower.starts_with("dev-") || version_lower.ends_with("-dev");
        if !is_dev_branch {
            continue;
        }
        set.insert((pkg.name.to_lowercase(), "9999999-dev".to_string()));
    }
    set
}

/// Walk every `installed.json` entry, expand its `extra.branch-alias` map, and
/// emit a [`StaleInstalledAlias`] for each whose alias version doesn't appear
/// in the new lock. Mirrors `Transaction::calculateOperations`
/// `MarkAliasUninstalledOperation` logic.
pub fn compute_stale_installed_aliases(
    installed: &InstalledPackages,
    lock: &LockFile,
) -> Vec<StaleInstalledAlias> {
    use super::{
        format_full_pretty_version_for_installed, format_full_pretty_with_pretty_for_installed,
    };

    let preserved = lock_alias_pretty_pairs(lock);
    let still_present = |name: &str, alias_pretty: &str| -> bool {
        preserved.contains(&(name.to_lowercase(), alias_pretty.to_string()))
    };
    let mut stale = Vec::new();
    for entry in &installed.packages {
        let mut emitted_explicit = false;
        if let Some(branch_alias) = entry
            .extra_fields
            .get("extra")
            .and_then(|e| e.get("branch-alias"))
            .and_then(|b| b.as_object())
        {
            for (target_branch, alias_value) in branch_alias {
                if entry.version != *target_branch {
                    continue;
                }
                let Some(alias_pretty) = alias_value.as_str() else {
                    continue;
                };
                emitted_explicit = true;
                if still_present(&entry.name, alias_pretty) {
                    continue;
                }
                stale.push(StaleInstalledAlias {
                    name: entry.name.clone(),
                    alias_full: format_full_pretty_with_pretty_for_installed(alias_pretty, entry),
                    target_full: format_full_pretty_version_for_installed(entry),
                });
            }
        }

        // Synthetic `9999999-dev` default-branch alias.
        if emitted_explicit {
            continue;
        }
        let is_default_branch = entry
            .extra_fields
            .get("default-branch")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !is_default_branch {
            continue;
        }
        let version_lower = entry.version.to_lowercase();
        let is_dev_branch = version_lower.starts_with("dev-") || version_lower.ends_with("-dev");
        if !is_dev_branch {
            continue;
        }
        const DEFAULT_BRANCH_ALIAS: &str = "9999999-dev";
        if still_present(&entry.name, DEFAULT_BRANCH_ALIAS) {
            continue;
        }
        stale.push(StaleInstalledAlias {
            name: entry.name.clone(),
            alias_full: format_full_pretty_with_pretty_for_installed(DEFAULT_BRANCH_ALIAS, entry),
            target_full: format_full_pretty_version_for_installed(entry),
        });
    }
    stale
}

/// Collect the alias normalized-versions a previous install recorded for
/// `pkg_name`. Mirrors Composer's `presentAliasMap` seeding.
pub fn previously_installed_alias_versions(
    installed: &InstalledPackages,
    pkg_name: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for entry in &installed.packages {
        if !entry.name.eq_ignore_ascii_case(pkg_name) {
            continue;
        }
        let version_lower = entry.version.to_lowercase();
        let is_dev_branch = version_lower.starts_with("dev-") || version_lower.ends_with("-dev");
        if !is_dev_branch {
            continue;
        }

        let mut emitted_explicit_alias = false;
        if let Some(branch_alias_map) = entry
            .extra_fields
            .get("extra")
            .and_then(|e| e.get("branch-alias"))
            .and_then(|b| b.as_object())
        {
            for (source, target) in branch_alias_map {
                if !source.eq_ignore_ascii_case(&entry.version) {
                    continue;
                }
                let Some(target_str) = target.as_str() else {
                    continue;
                };
                if !target_str.to_lowercase().ends_with("-dev") {
                    continue;
                }
                if let Some(normalized) =
                    super::super::resolver::normalize_branch_alias_target(target_str)
                {
                    out.push(normalized);
                    emitted_explicit_alias = true;
                }
            }
        }

        if !emitted_explicit_alias
            && entry
                .extra_fields
                .get("default-branch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            out.push("9999999.9999999.9999999.9999999-dev".to_string());
        }
    }
    out
}

/// Convert a `LockedPackage` to an `InstalledPackageEntry`.
///
/// Mirrors Composer's `InstalledFilesystemRepository::write()` via
/// `ArrayDumper` — `extra_fields` is forwarded verbatim so flags like
/// `abandoned` and `default-branch` survive the lock → installed.json round
/// trip.
pub fn locked_to_installed_entry(pkg: &LockedPackage, _vendor_dir: &Path) -> InstalledPackageEntry {
    let install_path = format!("../{}", pkg.name);
    InstalledPackageEntry {
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
        homepage: pkg.homepage.clone(),
        support: pkg.support.clone(),
        extra_fields: pkg.extra_fields.clone(),
    }
}

fn installed_refs_match_locked(entry: &InstalledPackageEntry, locked: &LockedPackage) -> bool {
    let installed_source_ref = entry
        .source
        .as_ref()
        .and_then(|v| v.get("reference"))
        .and_then(|v| v.as_str());
    let installed_dist_ref = entry
        .dist
        .as_ref()
        .and_then(|v| v.get("reference"))
        .and_then(|v| v.as_str());
    let locked_source_ref = locked.source.as_ref().and_then(|s| s.reference.as_deref());
    let locked_dist_ref = locked.dist.as_ref().and_then(|d| d.reference.as_deref());
    installed_source_ref == locked_source_ref && installed_dist_ref == locked_dist_ref
}

fn abandoned_state(v: Option<&serde_json::Value>) -> (bool, Option<&str>) {
    match v {
        Some(serde_json::Value::Bool(b)) => (*b, None),
        Some(serde_json::Value::String(s)) => (true, Some(s.as_str())),
        _ => (false, None),
    }
}

fn installed_abandoned_matches_locked(
    entry: &InstalledPackageEntry,
    locked: &LockedPackage,
) -> bool {
    abandoned_state(entry.extra_fields.get("abandoned"))
        == abandoned_state(locked.extra_fields.get("abandoned"))
}
