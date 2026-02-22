use clap::Args;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct SuggestsArgs {
    /// Package(s) to list suggestions for
    pub packages: Vec<String>,

    /// Group output by package
    #[arg(long)]
    pub by_package: bool,

    /// Group output by suggestion
    #[arg(long)]
    pub by_suggestion: bool,

    /// Show suggestions for all packages, not just root
    #[arg(short, long)]
    pub all: bool,

    /// Show only suggested package names in list format
    #[arg(long)]
    pub list: bool,

    /// Disables suggestions from require-dev packages
    #[arg(long)]
    pub no_dev: bool,
}

// ─── Data structures ─────────────────────────────────────────────────────────

struct Suggestion {
    source: String, // package making the suggestion
    target: String, // suggested package name
    reason: String, // human-readable reason (may be empty)
}

// ─── Main entry point ────────────────────────────────────────────────────────

pub async fn execute(
    args: &SuggestsArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let lock_path = working_dir.join("composer.lock");
    let has_lock = lock_path.exists();

    // 1. Collect raw suggestions from locked or installed packages
    let mut suggestions: Vec<Suggestion> = if has_lock {
        collect_suggestions_from_locked(&working_dir, args.no_dev)?
    } else {
        collect_suggestions_from_installed(&working_dir, args.no_dev)?
    };

    // Also collect root package's own suggestions
    let root_suggestions = collect_suggestions_from_root(&working_dir)?;
    suggestions.extend(root_suggestions);

    // 2. Collect installed names for filtering
    let installed_names = if has_lock {
        collect_installed_names_from_lock(&working_dir, args.no_dev)?
    } else {
        collect_installed_names_from_installed(&working_dir, args.no_dev)?
    };

    // 3. Determine direct-deps-only filter
    let (package_filter, direct_deps_only): (HashSet<String>, Option<HashSet<String>>) = {
        if !args.packages.is_empty() {
            // Filter by the explicitly named packages
            let filter: HashSet<String> = args.packages.iter().map(|s| s.to_lowercase()).collect();
            (filter, None)
        } else if args.all {
            (HashSet::new(), None)
        } else {
            // Default: only direct deps from composer.json
            let direct = compute_direct_deps(&working_dir)?;
            (HashSet::new(), Some(direct))
        }
    };

    // Count total suggestions before filtering by direct deps
    let total_before_direct_filter = if direct_deps_only.is_some() {
        // Count how many would survive without the direct-deps filter
        suggestions
            .iter()
            .filter(|s| !installed_names.contains(&s.target.to_lowercase()))
            .filter(|s| {
                if !package_filter.is_empty() {
                    package_filter.contains(&s.source.to_lowercase())
                } else {
                    true
                }
            })
            .count()
    } else {
        0
    };

    // 4. Filter suggestions
    let filtered: Vec<&Suggestion> = suggestions
        .iter()
        .filter(|s| {
            // Skip if target is already installed
            if installed_names.contains(&s.target.to_lowercase()) {
                return false;
            }
            // If package_filter is non-empty, skip if source not in filter
            if !package_filter.is_empty() && !package_filter.contains(&s.source.to_lowercase()) {
                return false;
            }
            // If direct_deps_only is Some, skip if source not in that set
            if let Some(ref direct) = direct_deps_only
                && !direct.contains(&s.source.to_lowercase())
            {
                return false;
            }
            true
        })
        .collect();

    // 5. Print info message about transitive suggestions
    if direct_deps_only.is_some() {
        let shown = filtered.len();
        let diff = total_before_direct_filter.saturating_sub(shown);
        if diff > 0 {
            println!(
                "{} additional suggestions by transitive dependencies can be shown with --all",
                diff
            );
        }
    }

    // 6. Render output
    if args.list {
        render_list(&filtered);
    } else if args.by_suggestion && !args.by_package {
        render_by_suggestion(&filtered);
    } else if args.by_package && args.by_suggestion {
        render_by_package(&filtered);
        println!("{}", "-".repeat(78));
        render_by_suggestion(&filtered);
    } else {
        // Default: by-package
        render_by_package(&filtered);
    }

    Ok(())
}

// ─── Suggestion collection ───────────────────────────────────────────────────

fn collect_suggestions_from_locked(
    working_dir: &Path,
    no_dev: bool,
) -> anyhow::Result<Vec<Suggestion>> {
    let lock_path = working_dir.join("composer.lock");
    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();
    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    let mut result = Vec::new();
    for pkg in all_packages {
        if let Some(ref suggest_map) = pkg.suggest {
            for (target, reason) in suggest_map {
                result.push(Suggestion {
                    source: pkg.name.clone(),
                    target: target.clone(),
                    reason: reason.clone(),
                });
            }
        }
    }
    Ok(result)
}

fn collect_suggestions_from_installed(
    working_dir: &Path,
    no_dev: bool,
) -> anyhow::Result<Vec<Suggestion>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    if installed.packages.is_empty() {
        let installed_json = vendor_dir.join("composer/installed.json");
        if !installed_json.exists() {
            anyhow::bail!(
                "No composer.lock and no installed.json found. \
                 Run `mozart install` first."
            );
        }
    }

    let dev_names: HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let mut result = Vec::new();
    for pkg in &installed.packages {
        if no_dev && dev_names.contains(&pkg.name.to_lowercase()) {
            continue;
        }
        // suggest is stored in extra_fields as a JSON object
        if let Some(suggest_val) = pkg.extra_fields.get("suggest")
            && let Some(obj) = suggest_val.as_object()
        {
            for (target, reason_val) in obj {
                let reason = reason_val.as_str().unwrap_or("").to_string();
                result.push(Suggestion {
                    source: pkg.name.clone(),
                    target: target.clone(),
                    reason,
                });
            }
        }
    }
    Ok(result)
}

fn collect_suggestions_from_root(working_dir: &Path) -> anyhow::Result<Vec<Suggestion>> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        return Ok(vec![]);
    }

    let root = mozart_core::package::read_from_file(&composer_json_path)?;

    // suggest is in extra_fields since RawPackageData doesn't model it explicitly
    let suggest_val = root.extra_fields.get("suggest");
    let Some(suggest_val) = suggest_val else {
        return Ok(vec![]);
    };

    let Some(obj) = suggest_val.as_object() else {
        return Ok(vec![]);
    };

    let mut result = Vec::new();
    for (target, reason_val) in obj {
        let reason = reason_val.as_str().unwrap_or("").to_string();
        result.push(Suggestion {
            source: root.name.clone(),
            target: target.clone(),
            reason,
        });
    }
    Ok(result)
}

// ─── Installed name collection ───────────────────────────────────────────────

fn collect_installed_names_from_lock(
    working_dir: &Path,
    no_dev: bool,
) -> anyhow::Result<HashSet<String>> {
    let lock_path = working_dir.join("composer.lock");
    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut names: HashSet<String> = HashSet::new();

    let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();
    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    for pkg in all_packages {
        names.insert(pkg.name.to_lowercase());

        // Also add provide and replace virtual package names
        for key in pkg.extra_fields.keys() {
            if (key == "provide" || key == "replace")
                && let Some(obj) = pkg.extra_fields[key].as_object()
            {
                for name in obj.keys() {
                    names.insert(name.to_lowercase());
                }
            }
        }
    }

    // Add platform packages (any package starting with php, ext-, lib-)
    add_platform_names_from_lock(&lock, &mut names);

    Ok(names)
}

fn collect_installed_names_from_installed(
    working_dir: &Path,
    no_dev: bool,
) -> anyhow::Result<HashSet<String>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    let dev_names: HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let mut names: HashSet<String> = HashSet::new();

    for pkg in &installed.packages {
        if no_dev && dev_names.contains(&pkg.name.to_lowercase()) {
            continue;
        }
        names.insert(pkg.name.to_lowercase());

        // provide / replace
        for key in &["provide", "replace"] {
            if let Some(val) = pkg.extra_fields.get(*key)
                && let Some(obj) = val.as_object()
            {
                for name in obj.keys() {
                    names.insert(name.to_lowercase());
                }
            }
        }
    }

    // Add platform packages from require/require-dev in composer.json
    let composer_json_path = working_dir.join("composer.json");
    if composer_json_path.exists()
        && let Ok(root) = mozart_core::package::read_from_file(&composer_json_path)
    {
        for name in root.require.keys().chain(root.require_dev.keys()) {
            if is_platform_package(name) {
                names.insert(name.to_lowercase());
            }
        }
    }

    Ok(names)
}

fn add_platform_names_from_lock(
    lock: &mozart_registry::lockfile::LockFile,
    names: &mut HashSet<String>,
) {
    // Collect platform keys from the lock's platform and platform_dev objects
    if let Some(obj) = lock.platform.as_object() {
        for key in obj.keys() {
            if is_platform_package(key) {
                names.insert(key.to_lowercase());
            }
        }
    }
    if let Some(obj) = lock.platform_dev.as_object() {
        for key in obj.keys() {
            if is_platform_package(key) {
                names.insert(key.to_lowercase());
            }
        }
    }
}

fn is_platform_package(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "php" || n.starts_with("php-") || n.starts_with("ext-") || n.starts_with("lib-")
}

// ─── Direct deps helper ───────────────────────────────────────────────────────

fn compute_direct_deps(working_dir: &Path) -> anyhow::Result<HashSet<String>> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        return Ok(HashSet::new());
    }
    let root = mozart_core::package::read_from_file(&composer_json_path)?;
    let mut deps: HashSet<String> = HashSet::new();
    // Include the root package itself so its suggestions are shown
    if !root.name.is_empty() {
        deps.insert(root.name.to_lowercase());
    }
    for name in root.require.keys().chain(root.require_dev.keys()) {
        deps.insert(name.to_lowercase());
    }
    Ok(deps)
}

// ─── Sanitization ────────────────────────────────────────────────────────────

/// Sanitize a suggestion reason string for safe terminal output.
/// Replaces newlines with spaces and strips control characters.
fn sanitize_reason(reason: &str) -> String {
    reason
        .replace(['\n', '\r'], " ")
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .collect()
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render_list(suggestions: &[&Suggestion]) {
    let mut targets: Vec<&str> = suggestions.iter().map(|s| s.target.as_str()).collect();
    targets.sort_unstable();
    targets.dedup();
    for t in targets {
        println!("{}", t);
    }
}

fn render_by_package(suggestions: &[&Suggestion]) {
    // Group by source, preserving insertion order via BTreeMap (sorted)
    let mut grouped: BTreeMap<&str, Vec<&Suggestion>> = BTreeMap::new();
    for s in suggestions {
        grouped.entry(s.source.as_str()).or_default().push(s);
    }
    for (source, items) in &grouped {
        println!("{} suggests:", source);
        for s in items {
            let reason = sanitize_reason(&s.reason);
            if reason.is_empty() {
                println!(" - {}", s.target);
            } else {
                println!(" - {}: {}", s.target, reason);
            }
        }
        println!();
    }
}

fn render_by_suggestion(suggestions: &[&Suggestion]) {
    // Group by target
    let mut grouped: BTreeMap<&str, Vec<&Suggestion>> = BTreeMap::new();
    for s in suggestions {
        grouped.entry(s.target.as_str()).or_default().push(s);
    }
    for (target, items) in &grouped {
        println!("{} is suggested by:", target);
        for s in items {
            let reason = sanitize_reason(&s.reason);
            if reason.is_empty() {
                println!(" - {}", s.source);
            } else {
                println!(" - {}: {}", s.source, reason);
            }
        }
        println!();
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_suggestion(source: &str, target: &str, reason: &str) -> Suggestion {
        Suggestion {
            source: source.to_string(),
            target: target.to_string(),
            reason: reason.to_string(),
        }
    }

    fn make_locked_package(
        name: &str,
        suggest: Option<BTreeMap<String, String>>,
    ) -> mozart_registry::lockfile::LockedPackage {
        mozart_registry::lockfile::LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            conflict: BTreeMap::new(),
            suggest,
            package_type: None,
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

    fn make_installed_entry(
        name: &str,
        suggest: Option<BTreeMap<String, String>>,
    ) -> mozart_registry::installed::InstalledPackageEntry {
        let mut extra_fields: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        if let Some(s) = suggest {
            let map: serde_json::Map<String, serde_json::Value> = s
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect();
            extra_fields.insert("suggest".to_string(), serde_json::Value::Object(map));
        }
        mozart_registry::installed::InstalledPackageEntry {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields,
        }
    }

    fn minimal_lock(
        packages: Vec<mozart_registry::lockfile::LockedPackage>,
        packages_dev: Option<Vec<mozart_registry::lockfile::LockedPackage>>,
    ) -> mozart_registry::lockfile::LockFile {
        mozart_registry::lockfile::LockFile {
            readme: mozart_registry::lockfile::LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages,
            packages_dev,
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

    // ── Filter tests ──────────────────────────────────────────────────────────

    #[test]
    fn test_filter_removes_installed_targets() {
        let suggestions = vec![
            make_suggestion("vendor/a", "ext-intl", "for internationalization"),
            make_suggestion("vendor/b", "vendor/optional", "for extra features"),
            make_suggestion("vendor/c", "ext-mbstring", "for string processing"),
        ];
        let refs: Vec<&Suggestion> = suggestions.iter().collect();

        let mut installed: HashSet<String> = HashSet::new();
        installed.insert("ext-intl".to_string());
        installed.insert("ext-mbstring".to_string());

        let filtered: Vec<&Suggestion> = refs
            .iter()
            .copied()
            .filter(|s| !installed.contains(&s.target.to_lowercase()))
            .collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].target, "vendor/optional");
    }

    #[test]
    fn test_filter_by_package_names() {
        let suggestions = vec![
            make_suggestion("vendor/a", "vendor/x", "reason"),
            make_suggestion("vendor/b", "vendor/y", "reason"),
            make_suggestion("vendor/c", "vendor/z", "reason"),
        ];
        let refs: Vec<&Suggestion> = suggestions.iter().collect();

        let mut filter: HashSet<String> = HashSet::new();
        filter.insert("vendor/a".to_string());
        filter.insert("vendor/c".to_string());

        let filtered: Vec<&Suggestion> = refs
            .iter()
            .copied()
            .filter(|s| filter.contains(&s.source.to_lowercase()))
            .collect();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].source, "vendor/a");
        assert_eq!(filtered[1].source, "vendor/c");
    }

    #[test]
    fn test_filter_direct_deps_only() {
        let suggestions = vec![
            make_suggestion("vendor/direct", "vendor/x", "reason"),
            make_suggestion("vendor/transitive", "vendor/y", "reason"),
        ];
        let refs: Vec<&Suggestion> = suggestions.iter().collect();

        let mut direct: HashSet<String> = HashSet::new();
        direct.insert("vendor/direct".to_string());

        let filtered: Vec<&Suggestion> = refs
            .iter()
            .copied()
            .filter(|s| direct.contains(&s.source.to_lowercase()))
            .collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "vendor/direct");
    }

    #[test]
    fn test_filter_no_filter() {
        let suggestions = vec![
            make_suggestion("vendor/a", "vendor/x", ""),
            make_suggestion("vendor/b", "vendor/y", ""),
            make_suggestion("vendor/c", "vendor/z", ""),
        ];
        let refs: Vec<&Suggestion> = suggestions.iter().collect();
        let installed: HashSet<String> = HashSet::new();

        let filtered: Vec<&Suggestion> = refs
            .iter()
            .copied()
            .filter(|s| !installed.contains(&s.target.to_lowercase()))
            .collect();

        assert_eq!(filtered.len(), 3);
    }

    // ── Collection tests ──────────────────────────────────────────────────────

    #[test]
    fn test_suggests_from_lockfile() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let mut suggest_a = BTreeMap::new();
        suggest_a.insert(
            "ext-intl".to_string(),
            "For internationalization".to_string(),
        );
        suggest_a.insert(
            "vendor/optional".to_string(),
            "Optional features".to_string(),
        );

        let lock = minimal_lock(
            vec![make_locked_package("vendor/a", Some(suggest_a))],
            Some(vec![]),
        );
        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let suggestions = collect_suggestions_from_locked(working_dir, false).unwrap();
        assert_eq!(suggestions.len(), 2);
        assert!(suggestions.iter().all(|s| s.source == "vendor/a"));
        let targets: HashSet<&str> = suggestions.iter().map(|s| s.target.as_str()).collect();
        assert!(targets.contains("ext-intl"));
        assert!(targets.contains("vendor/optional"));
    }

    #[test]
    fn test_suggests_from_lockfile_no_dev() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let mut prod_suggest = BTreeMap::new();
        prod_suggest.insert(
            "vendor/prod-opt".to_string(),
            "Production option".to_string(),
        );

        let mut dev_suggest = BTreeMap::new();
        dev_suggest.insert("vendor/dev-opt".to_string(), "Dev option".to_string());

        let lock = minimal_lock(
            vec![make_locked_package("vendor/prod", Some(prod_suggest))],
            Some(vec![make_locked_package("vendor/dev", Some(dev_suggest))]),
        );
        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        // With no_dev=true: only production suggestions
        let suggestions = collect_suggestions_from_locked(working_dir, true).unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].source, "vendor/prod");
        assert_eq!(suggestions[0].target, "vendor/prod-opt");

        // With no_dev=false: both
        let suggestions_all = collect_suggestions_from_locked(working_dir, false).unwrap();
        assert_eq!(suggestions_all.len(), 2);
    }

    #[test]
    fn test_suggests_from_installed() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        let mut suggest = BTreeMap::new();
        suggest.insert("ext-redis".to_string(), "For Redis caching".to_string());

        let mut installed = mozart_registry::installed::InstalledPackages::new();
        installed.upsert(make_installed_entry("vendor/cache", Some(suggest)));
        installed.upsert(make_installed_entry("vendor/other", None));
        installed.write(&vendor_dir).unwrap();

        let suggestions = collect_suggestions_from_installed(working_dir, false).unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].source, "vendor/cache");
        assert_eq!(suggestions[0].target, "ext-redis");
        assert_eq!(suggestions[0].reason, "For Redis caching");
    }

    #[test]
    fn test_suggests_from_root() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let composer_json = serde_json::json!({
            "name": "my/project",
            "require": {},
            "suggest": {
                "vendor/optional-pkg": "Provides extra functionality"
            }
        });
        std::fs::write(
            working_dir.join("composer.json"),
            serde_json::to_string_pretty(&composer_json).unwrap(),
        )
        .unwrap();

        let suggestions = collect_suggestions_from_root(working_dir).unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].source, "my/project");
        assert_eq!(suggestions[0].target, "vendor/optional-pkg");
        assert_eq!(suggestions[0].reason, "Provides extra functionality");
    }

    #[test]
    fn test_suggests_filters_already_installed() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let mut suggest = BTreeMap::new();
        suggest.insert(
            "vendor/already-here".to_string(),
            "Already installed".to_string(),
        );
        suggest.insert(
            "vendor/not-here".to_string(),
            "Not yet installed".to_string(),
        );

        let lock = minimal_lock(
            vec![
                make_locked_package("vendor/a", Some(suggest)),
                make_locked_package("vendor/already-here", None),
            ],
            Some(vec![]),
        );
        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let suggestions = collect_suggestions_from_locked(working_dir, false).unwrap();
        let installed = collect_installed_names_from_lock(working_dir, false).unwrap();

        let filtered: Vec<&Suggestion> = suggestions
            .iter()
            .filter(|s| !installed.contains(&s.target.to_lowercase()))
            .collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].target, "vendor/not-here");
    }

    #[test]
    fn test_suggests_empty() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let lock = minimal_lock(
            vec![make_locked_package("vendor/no-suggestions", None)],
            Some(vec![]),
        );
        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let suggestions = collect_suggestions_from_locked(working_dir, false).unwrap();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_collect_installed_names() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let mut suggest_a = BTreeMap::new();
        suggest_a.insert("vendor/opt".to_string(), "optional".to_string());

        let lock = minimal_lock(
            vec![
                make_locked_package("vendor/pkg-a", Some(suggest_a)),
                make_locked_package("vendor/pkg-b", None),
            ],
            Some(vec![make_locked_package("vendor/pkg-dev", None)]),
        );
        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let names = collect_installed_names_from_lock(working_dir, false).unwrap();
        assert!(names.contains("vendor/pkg-a"));
        assert!(names.contains("vendor/pkg-b"));
        assert!(names.contains("vendor/pkg-dev"));

        // With no_dev=true: dev package excluded
        let names_no_dev = collect_installed_names_from_lock(working_dir, true).unwrap();
        assert!(names_no_dev.contains("vendor/pkg-a"));
        assert!(names_no_dev.contains("vendor/pkg-b"));
        assert!(!names_no_dev.contains("vendor/pkg-dev"));
    }
}
