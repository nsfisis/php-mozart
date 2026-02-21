use clap::Args;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ShowArgs {
    /// Package to inspect
    pub package: Option<String>,

    /// Version constraint
    pub version: Option<String>,

    /// List all packages
    #[arg(long)]
    pub all: bool,

    /// List packages from the lock file
    #[arg(long)]
    pub locked: bool,

    /// Show only installed packages (enabled by default)
    #[arg(short, long)]
    pub installed: bool,

    /// List platform packages only
    #[arg(short, long)]
    pub platform: bool,

    /// List available packages only
    #[arg(short = 'a', long)]
    pub available: bool,

    /// Show information about the root package
    #[arg(short, long, name = "self")]
    pub self_info: bool,

    /// Show package names only
    #[arg(short = 'N', long)]
    pub name_only: bool,

    /// Show package paths only
    #[arg(short = 'P', long)]
    pub path: bool,

    /// List the dependencies as a tree
    #[arg(short, long)]
    pub tree: bool,

    /// Show the latest version
    #[arg(short, long)]
    pub latest: bool,

    /// Show only packages that are outdated
    #[arg(short, long)]
    pub outdated: bool,

    /// Ignore specified package(s)
    #[arg(long)]
    pub ignore: Vec<String>,

    /// Only show packages that have major SemVer-compatible updates
    #[arg(short = 'M', long)]
    pub major_only: bool,

    /// Only show packages that have minor SemVer-compatible updates
    #[arg(short = 'm', long)]
    pub minor_only: bool,

    /// Only show packages that have patch SemVer-compatible updates
    #[arg(long)]
    pub patch_only: bool,

    /// Sort packages by age of the last update
    #[arg(short = 'A', long)]
    pub sort_by_age: bool,

    /// Shows only packages that are directly required by the root package
    #[arg(short = 'D', long)]
    pub direct: bool,

    /// Return a non-zero exit code when there are outdated packages
    #[arg(long)]
    pub strict: bool,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,

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

pub fn execute(args: &ShowArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // --platform: show detected platform packages
    if args.platform {
        return show_platform(args, &working_dir);
    }

    // --self: show root package info (unless --installed or --locked override)
    if args.self_info && !args.installed && !args.locked {
        return show_self(args, &working_dir);
    }

    // --tree: show dependency tree (uses lock file)
    if args.tree {
        return show_tree(args, &working_dir);
    }

    // --available: show available versions for installed packages
    if args.available {
        return show_available(args, &working_dir);
    }

    // --locked: show from lock file
    if args.locked {
        return execute_locked(args, &working_dir);
    }

    // Default: installed mode
    execute_installed(args, &working_dir)
}

// ─── Installed mode ────────────────────────────────────────────────────────

fn execute_installed(args: &ShowArgs, working_dir: &Path) -> anyhow::Result<()> {
    let vendor_dir = working_dir.join("vendor");
    let installed = crate::installed::InstalledPackages::read(&vendor_dir)?;

    if installed.packages.is_empty() {
        // Warn if composer.json has requirements but nothing is installed
        let composer_json_path = working_dir.join("composer.json");
        if composer_json_path.exists() {
            let root = crate::package::read_from_file(&composer_json_path)?;
            if !root.require.is_empty() || !root.require_dev.is_empty() {
                eprintln!(
                    "{}",
                    crate::console::warning(
                        "No dependencies installed. Try running mozart install or update."
                    )
                );
            }
        }
        return Ok(());
    }

    // --path with a specific package name: just show the path for that one package
    if args.path
        && let Some(ref package_name) = args.package
        && !package_name.contains('*')
    {
        let pkg = installed
            .packages
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(package_name));
        match pkg {
            Some(p) => {
                let install_path = vendor_dir.join(&p.name);
                let path_str = resolve_path(&install_path);
                println!("{} {}", p.name, path_str);
            }
            None => {
                anyhow::bail!(
                    "Package \"{}\" not found, try using --available (-a) to show all available packages",
                    package_name
                );
            }
        }
        return Ok(());
    }

    // Filter packages
    let mut packages = filter_installed_packages(&installed, args, working_dir)?;

    // Apply wildcard or exact package filter
    if let Some(ref package_filter) = args.package {
        if package_filter.contains('*') {
            packages.retain(|p| matches_wildcard(&p.name, package_filter));
            show_installed_package_list(&packages, args, &vendor_dir)?;
            return Ok(());
        } else {
            // Single package detail view
            return show_installed_package_detail(&installed, package_filter, working_dir);
        }
    }

    // --path list mode
    if args.path {
        for pkg in &packages {
            let install_path = vendor_dir.join(&pkg.name);
            let path_str = resolve_path(&install_path);
            println!("{} {}", pkg.name, path_str);
        }
        return Ok(());
    }

    // List view
    show_installed_package_list(&packages, args, &vendor_dir)
}

fn filter_installed_packages<'a>(
    installed: &'a crate::installed::InstalledPackages,
    args: &ShowArgs,
    working_dir: &Path,
) -> anyhow::Result<Vec<&'a crate::installed::InstalledPackageEntry>> {
    let mut packages: Vec<&crate::installed::InstalledPackageEntry> =
        installed.packages.iter().collect();

    // --no-dev: exclude dev packages
    if args.no_dev {
        let dev_names: HashSet<String> = installed
            .dev_package_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        packages.retain(|p| !dev_names.contains(&p.name.to_lowercase()));
    }

    // --direct: only show packages directly required by root
    if args.direct {
        let composer_json_path = working_dir.join("composer.json");
        if composer_json_path.exists() {
            let root = crate::package::read_from_file(&composer_json_path)?;
            let mut direct_names: HashSet<String> =
                root.require.keys().map(|k| k.to_lowercase()).collect();
            if !args.no_dev {
                direct_names.extend(root.require_dev.keys().map(|k| k.to_lowercase()));
            }
            packages.retain(|p| direct_names.contains(&p.name.to_lowercase()));
        }
    }

    // Sort alphabetically by name
    packages.sort_by_key(|a| a.name.to_lowercase());

    Ok(packages)
}

fn show_installed_package_list(
    packages: &[&crate::installed::InstalledPackageEntry],
    args: &ShowArgs,
    _vendor_dir: &Path,
) -> anyhow::Result<()> {
    // --latest / --outdated: fetch latest versions from Packagist
    let show_latest = args.latest || args.outdated;

    if args.name_only {
        for pkg in packages {
            println!("{}", pkg.name);
        }
        return Ok(());
    }

    if packages.is_empty() {
        return Ok(());
    }

    // Build ignore set
    let ignore_set: HashSet<String> = args.ignore.iter().map(|n| n.to_lowercase()).collect();

    // Gather entries (fetch latest if needed, apply outdated filter)
    let mut entries: Vec<InstalledListEntry> = Vec::new();
    for pkg in packages {
        if ignore_set.contains(&pkg.name.to_lowercase()) {
            continue;
        }

        let version_normalized = pkg
            .version_normalized
            .clone()
            .unwrap_or_else(|| normalize_version_simple(&pkg.version));
        let description = get_installed_description(pkg);

        let latest_info = if show_latest {
            fetch_latest_for_package(&pkg.name).ok()
        } else {
            None
        };

        // --outdated: skip packages that are up-to-date
        if args.outdated {
            if let Some(ref li) = latest_info {
                use crate::version::compare_normalized_versions;
                use std::cmp::Ordering;
                if compare_normalized_versions(&li.version_normalized, &version_normalized)
                    != Ordering::Greater
                {
                    continue;
                }
            } else {
                // Cannot determine latest: skip
                continue;
            }
        }

        entries.push(InstalledListEntry {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            version_normalized,
            description,
            latest_info,
        });
    }

    // --strict: exit 1 if any outdated
    let has_outdated = entries.iter().any(|e| e.latest_info.is_some());

    // JSON output
    let format = args.format.as_deref().unwrap_or("text");
    if format == "json" {
        render_installed_json(&entries)?;
        if args.strict && has_outdated {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Text output
    let name_width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let version_width = entries
        .iter()
        .map(|e| format_version(&e.version).len())
        .max()
        .unwrap_or(0);
    let latest_width = if show_latest {
        entries
            .iter()
            .map(|e| {
                e.latest_info
                    .as_ref()
                    .map(|li| format_version(&li.version).len())
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    } else {
        0
    };

    for entry in &entries {
        let version = format_version(&entry.version);
        let category = entry
            .latest_info
            .as_ref()
            .map(|li| classify_update_category(&entry.version_normalized, &li.version_normalized));

        let name_str = match category {
            Some(ListUpdateKind::Compatible) => {
                crate::console::highlight(&format!("{:<width$}", entry.name, width = name_width))
                    .to_string()
            }
            Some(ListUpdateKind::Incompatible) => {
                crate::console::comment(&format!("{:<width$}", entry.name, width = name_width))
                    .to_string()
            }
            _ => crate::console::info(&format!("{:<width$}", entry.name, width = name_width))
                .to_string(),
        };

        let version_str =
            crate::console::comment(&format!("{:<width$}", version, width = version_width))
                .to_string();

        if show_latest {
            let latest_str = match entry.latest_info.as_ref() {
                Some(li) => {
                    let lv = format_version(&li.version);
                    match category {
                        Some(ListUpdateKind::Compatible) => crate::console::highlight(&format!(
                            "{:<width$}",
                            lv,
                            width = latest_width
                        ))
                        .to_string(),
                        Some(ListUpdateKind::Incompatible) => crate::console::comment(&format!(
                            "{:<width$}",
                            lv,
                            width = latest_width
                        ))
                        .to_string(),
                        _ => crate::console::info(&format!("{:<width$}", lv, width = latest_width))
                            .to_string(),
                    }
                }
                None => format!("{:<width$}", "", width = latest_width),
            };
            println!(
                "{} {} {} {}",
                name_str, version_str, latest_str, entry.description
            );
        } else {
            println!("{} {} {}", name_str, version_str, entry.description);
        }
    }

    if args.strict && has_outdated {
        std::process::exit(1);
    }

    Ok(())
}

/// Entry for the installed package list (with optional latest info)
struct InstalledListEntry {
    name: String,
    version: String,
    version_normalized: String,
    description: String,
    latest_info: Option<LatestInfo>,
}

struct LatestInfo {
    version: String,
    version_normalized: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListUpdateKind {
    UpToDate,
    Compatible,
    Incompatible,
}

fn classify_update_category(current_normalized: &str, latest_normalized: &str) -> ListUpdateKind {
    use crate::version::compare_normalized_versions;
    use std::cmp::Ordering;

    if compare_normalized_versions(latest_normalized, current_normalized) != Ordering::Greater {
        return ListUpdateKind::UpToDate;
    }

    // Compare major versions to determine compatibility
    let current_major = extract_major(current_normalized);
    let latest_major = extract_major(latest_normalized);
    if current_major == latest_major {
        ListUpdateKind::Compatible
    } else {
        ListUpdateKind::Incompatible
    }
}

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

fn fetch_latest_for_package(name: &str) -> anyhow::Result<LatestInfo> {
    use crate::package::Stability;
    use crate::version::find_best_candidate;

    let versions = crate::packagist::fetch_package_versions(name, None)?;
    let best = find_best_candidate(&versions, Stability::Stable)
        .ok_or_else(|| anyhow::anyhow!("No stable version found for {name}"))?;

    Ok(LatestInfo {
        version: best.version.clone(),
        version_normalized: best.version_normalized.clone(),
    })
}

fn render_installed_json(entries: &[InstalledListEntry]) -> anyhow::Result<()> {
    let json_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let mut obj = serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "description": entry.description,
            });
            if let Some(ref li) = entry.latest_info {
                obj["latest"] = serde_json::Value::String(li.version.clone());
                let status =
                    if classify_update_category(&entry.version_normalized, &li.version_normalized)
                        == ListUpdateKind::UpToDate
                    {
                        "up-to-date"
                    } else {
                        "outdated"
                    };
                obj["latest-status"] = serde_json::Value::String(status.to_string());
            }
            obj
        })
        .collect();

    let output = serde_json::json!({ "installed": json_entries });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn show_installed_package_detail(
    installed: &crate::installed::InstalledPackages,
    package_name: &str,
    working_dir: &Path,
) -> anyhow::Result<()> {
    // Find the package (case-insensitive)
    let pkg = installed
        .packages
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(package_name));

    let pkg = match pkg {
        Some(p) => p,
        None => {
            anyhow::bail!(
                "Package \"{}\" not found, try using --available (-a) to show all available packages",
                package_name
            );
        }
    };

    let vendor_dir = working_dir.join("vendor");

    println!("{} : {}", crate::console::info("name"), pkg.name);
    println!(
        "{} : {}",
        crate::console::info("descrip."),
        get_installed_description(pkg)
    );
    println!(
        "{} : {}",
        crate::console::info("keywords"),
        get_installed_keywords(pkg)
    );
    println!(
        "{} : {}",
        crate::console::info("versions"),
        format_version_highlight(&pkg.version)
    );
    println!(
        "{} : {}",
        crate::console::info("type"),
        pkg.package_type.as_deref().unwrap_or("library")
    );

    // License
    if let Some(licenses) = get_installed_license(pkg) {
        println!("{} : {}", crate::console::info("license"), licenses);
    }

    // Homepage
    if let Some(homepage) = get_installed_homepage(pkg) {
        println!("{} : {}", crate::console::info("homepage"), homepage);
    }

    // Source
    if let Some(source) = &pkg.source {
        let source_type = source.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let source_url = source.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let source_ref = source
            .get("reference")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!(
            "{} : [{}] {} {}",
            crate::console::info("source"),
            source_type,
            crate::console::comment(source_url),
            source_ref
        );
    }

    // Dist
    if let Some(dist) = &pkg.dist {
        let dist_type = dist.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let dist_url = dist.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let dist_ref = dist.get("reference").and_then(|v| v.as_str()).unwrap_or("");
        println!(
            "{} : [{}] {} {}",
            crate::console::info("dist"),
            dist_type,
            crate::console::comment(dist_url),
            dist_ref
        );
    }

    // Path
    let install_path = vendor_dir.join(&pkg.name);
    if install_path.exists() {
        println!(
            "{} : {}",
            crate::console::info("path"),
            install_path.display()
        );
    }

    // Requires
    if let Some(requires) = pkg.extra_fields.get("require").and_then(|v| v.as_object())
        && !requires.is_empty()
    {
        println!();
        println!("{}", crate::console::info("requires"));
        for (name, constraint) in requires {
            let c = constraint.as_str().unwrap_or("");
            println!("{} {}", name, crate::console::comment(c));
        }
    }

    // Requires (dev)
    if let Some(requires_dev) = pkg
        .extra_fields
        .get("require-dev")
        .and_then(|v| v.as_object())
        && !requires_dev.is_empty()
    {
        println!();
        println!("{}", crate::console::info("requires (dev)"));
        for (name, constraint) in requires_dev {
            let c = constraint.as_str().unwrap_or("");
            println!("{} {}", name, crate::console::comment(c));
        }
    }

    Ok(())
}

// ─── Locked mode ───────────────────────────────────────────────────────────

fn execute_locked(args: &ShowArgs, working_dir: &Path) -> anyhow::Result<()> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "A valid composer.json and composer.lock files is required to run this command with --locked"
        );
    }

    let lock = crate::lockfile::LockFile::read_from_file(&lock_path)?;

    // Combine packages and packages-dev
    let mut packages: Vec<&crate::lockfile::LockedPackage> = lock.packages.iter().collect();

    if let Some(ref pkgs_dev) = lock.packages_dev
        && !args.no_dev
    {
        packages.extend(pkgs_dev.iter());
    }

    // --direct filter
    if args.direct {
        let composer_json_path = working_dir.join("composer.json");
        if composer_json_path.exists() {
            let root = crate::package::read_from_file(&composer_json_path)?;
            let mut direct_names: HashSet<String> =
                root.require.keys().map(|k| k.to_lowercase()).collect();
            if !args.no_dev {
                direct_names.extend(root.require_dev.keys().map(|k| k.to_lowercase()));
            }
            packages.retain(|p| direct_names.contains(&p.name.to_lowercase()));
        }
    }

    // Sort alphabetically
    packages.sort_by_key(|a| a.name.to_lowercase());

    if let Some(ref package_filter) = args.package {
        if package_filter.contains('*') {
            packages.retain(|p| matches_wildcard(&p.name, package_filter));
            show_locked_package_list(&packages, args)?;
        } else {
            show_locked_package_detail(&lock, package_filter)?;
        }
    } else {
        show_locked_package_list(&packages, args)?;
    }

    Ok(())
}

fn show_locked_package_list(
    packages: &[&crate::lockfile::LockedPackage],
    args: &ShowArgs,
) -> anyhow::Result<()> {
    let show_latest = args.latest || args.outdated;

    if args.name_only {
        for pkg in packages {
            println!("{}", pkg.name);
        }
        return Ok(());
    }

    if packages.is_empty() {
        return Ok(());
    }

    // Build ignore set
    let ignore_set: HashSet<String> = args.ignore.iter().map(|n| n.to_lowercase()).collect();

    // Gather entries
    let mut entries: Vec<LockedListEntry> = Vec::new();
    for pkg in packages {
        if ignore_set.contains(&pkg.name.to_lowercase()) {
            continue;
        }

        let version_normalized = pkg
            .version_normalized
            .clone()
            .unwrap_or_else(|| normalize_version_simple(&pkg.version));
        let description = pkg.description.as_deref().unwrap_or("").to_string();

        let latest_info = if show_latest {
            fetch_latest_for_package(&pkg.name).ok()
        } else {
            None
        };

        // --outdated: skip packages that are up-to-date
        if args.outdated {
            if let Some(ref li) = latest_info {
                use crate::version::compare_normalized_versions;
                use std::cmp::Ordering;
                if compare_normalized_versions(&li.version_normalized, &version_normalized)
                    != Ordering::Greater
                {
                    continue;
                }
            } else {
                continue;
            }
        }

        entries.push(LockedListEntry {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            version_normalized,
            description,
            latest_info,
        });
    }

    let has_outdated = entries.iter().any(|e| e.latest_info.is_some());

    // JSON format
    let format = args.format.as_deref().unwrap_or("text");
    if format == "json" {
        render_locked_json(&entries)?;
        if args.strict && has_outdated {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Text format
    let name_width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let version_width = entries
        .iter()
        .map(|e| format_version(&e.version).len())
        .max()
        .unwrap_or(0);
    let latest_width = if show_latest {
        entries
            .iter()
            .map(|e| {
                e.latest_info
                    .as_ref()
                    .map(|li| format_version(&li.version).len())
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    } else {
        0
    };

    for entry in &entries {
        let version = format_version(&entry.version);
        let category = entry
            .latest_info
            .as_ref()
            .map(|li| classify_update_category(&entry.version_normalized, &li.version_normalized));

        let name_str = match category {
            Some(ListUpdateKind::Compatible) => {
                crate::console::highlight(&format!("{:<width$}", entry.name, width = name_width))
                    .to_string()
            }
            Some(ListUpdateKind::Incompatible) => {
                crate::console::comment(&format!("{:<width$}", entry.name, width = name_width))
                    .to_string()
            }
            _ => crate::console::info(&format!("{:<width$}", entry.name, width = name_width))
                .to_string(),
        };

        let version_str =
            crate::console::comment(&format!("{:<width$}", version, width = version_width))
                .to_string();

        if show_latest {
            let latest_str = match entry.latest_info.as_ref() {
                Some(li) => {
                    let lv = format_version(&li.version);
                    match category {
                        Some(ListUpdateKind::Compatible) => crate::console::highlight(&format!(
                            "{:<width$}",
                            lv,
                            width = latest_width
                        ))
                        .to_string(),
                        Some(ListUpdateKind::Incompatible) => crate::console::comment(&format!(
                            "{:<width$}",
                            lv,
                            width = latest_width
                        ))
                        .to_string(),
                        _ => crate::console::info(&format!("{:<width$}", lv, width = latest_width))
                            .to_string(),
                    }
                }
                None => format!("{:<width$}", "", width = latest_width),
            };
            println!(
                "{} {} {} {}",
                name_str, version_str, latest_str, entry.description
            );
        } else {
            println!("{} {} {}", name_str, version_str, entry.description);
        }
    }

    if args.strict && has_outdated {
        std::process::exit(1);
    }

    Ok(())
}

struct LockedListEntry {
    name: String,
    version: String,
    version_normalized: String,
    description: String,
    latest_info: Option<LatestInfo>,
}

fn render_locked_json(entries: &[LockedListEntry]) -> anyhow::Result<()> {
    let json_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let mut obj = serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "description": entry.description,
            });
            if let Some(ref li) = entry.latest_info {
                obj["latest"] = serde_json::Value::String(li.version.clone());
                let status =
                    if classify_update_category(&entry.version_normalized, &li.version_normalized)
                        == ListUpdateKind::UpToDate
                    {
                        "up-to-date"
                    } else {
                        "outdated"
                    };
                obj["latest-status"] = serde_json::Value::String(status.to_string());
            }
            obj
        })
        .collect();

    let output = serde_json::json!({ "installed": json_entries });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn show_locked_package_detail(
    lock: &crate::lockfile::LockFile,
    package_name: &str,
) -> anyhow::Result<()> {
    // Search in both packages and packages-dev
    let pkg = lock
        .packages
        .iter()
        .chain(lock.packages_dev.iter().flatten())
        .find(|p| p.name.eq_ignore_ascii_case(package_name));

    let pkg = match pkg {
        Some(p) => p,
        None => {
            anyhow::bail!("Package \"{}\" not found in lock file", package_name);
        }
    };

    println!("{} : {}", crate::console::info("name"), pkg.name);
    println!(
        "{} : {}",
        crate::console::info("descrip."),
        pkg.description.as_deref().unwrap_or("")
    );

    // Keywords
    let keywords = pkg
        .keywords
        .as_ref()
        .map(|kw| kw.join(", "))
        .unwrap_or_default();
    println!("{} : {}", crate::console::info("keywords"), keywords);

    println!(
        "{} : * {}",
        crate::console::info("versions"),
        format_version(&pkg.version)
    );
    println!(
        "{} : {}",
        crate::console::info("type"),
        pkg.package_type.as_deref().unwrap_or("library")
    );

    // License
    if let Some(ref licenses) = pkg.license {
        println!(
            "{} : {}",
            crate::console::info("license"),
            licenses.join(", ")
        );
    }

    // Homepage
    if let Some(ref homepage) = pkg.homepage {
        println!("{} : {}", crate::console::info("homepage"), homepage);
    }

    // Source
    if let Some(ref source) = pkg.source {
        println!(
            "{} : [{}] {} {}",
            crate::console::info("source"),
            source.source_type,
            crate::console::comment(&source.url),
            source.reference.as_deref().unwrap_or("")
        );
    }

    // Dist
    if let Some(ref dist) = pkg.dist {
        println!(
            "{} : [{}] {} {}",
            crate::console::info("dist"),
            dist.dist_type,
            crate::console::comment(&dist.url),
            dist.reference.as_deref().unwrap_or("")
        );
    }

    // Requires
    if !pkg.require.is_empty() {
        println!();
        println!("{}", crate::console::info("requires"));
        for (name, constraint) in &pkg.require {
            println!("{} {}", name, crate::console::comment(constraint));
        }
    }

    // Requires (dev)
    if !pkg.require_dev.is_empty() {
        println!();
        println!("{}", crate::console::info("requires (dev)"));
        for (name, constraint) in &pkg.require_dev {
            println!("{} {}", name, crate::console::comment(constraint));
        }
    }

    // Suggests
    if let Some(ref suggests) = pkg.suggest
        && !suggests.is_empty()
    {
        println!();
        println!("{}", crate::console::info("suggests"));
        for (name, reason) in suggests {
            println!("{} {}", name, crate::console::comment(reason));
        }
    }

    Ok(())
}

// ─── Self mode ─────────────────────────────────────────────────────────────

fn show_self(args: &ShowArgs, working_dir: &Path) -> anyhow::Result<()> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }
    let root = crate::package::read_from_file(&composer_json_path)?;

    if args.name_only {
        println!("{}", root.name);
        return Ok(());
    }

    println!("{} : {}", crate::console::info("name"), root.name);
    println!(
        "{} : {}",
        crate::console::info("descrip."),
        root.description.as_deref().unwrap_or("")
    );
    println!(
        "{} : {}",
        crate::console::info("type"),
        root.package_type.as_deref().unwrap_or("project")
    );
    if let Some(ref license) = root.license {
        println!("{} : {}", crate::console::info("license"), license);
    }
    if let Some(ref homepage) = root.homepage {
        println!("{} : {}", crate::console::info("homepage"), homepage);
    }

    // Requires
    if !root.require.is_empty() {
        println!();
        println!("{}", crate::console::info("requires"));
        for (name, constraint) in &root.require {
            println!("{} {}", name, crate::console::comment(constraint));
        }
    }

    // Requires (dev)
    if !root.require_dev.is_empty() {
        println!();
        println!("{}", crate::console::info("requires (dev)"));
        for (name, constraint) in &root.require_dev {
            println!("{} {}", name, crate::console::comment(constraint));
        }
    }

    Ok(())
}

// ─── Tree mode ─────────────────────────────────────────────────────────────

fn show_tree(args: &ShowArgs, working_dir: &Path) -> anyhow::Result<()> {
    let lock_path = working_dir.join("composer.lock");
    let composer_json_path = working_dir.join("composer.json");

    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }

    let root = crate::package::read_from_file(&composer_json_path)?;

    // Load all locked packages into a map for quick lookup
    let pkg_map: HashMap<String, &crate::lockfile::LockedPackage>;
    let lock_storage;
    if lock_path.exists() {
        lock_storage = crate::lockfile::LockFile::read_from_file(&lock_path)?;
        pkg_map = lock_storage
            .packages
            .iter()
            .chain(lock_storage.packages_dev.iter().flatten())
            .map(|p| (p.name.to_lowercase(), p))
            .collect();
    } else {
        pkg_map = HashMap::new();
    }

    // Determine roots to display: package filter or full tree
    let root_reqs: Vec<(String, String)> = if let Some(ref pkg_filter) = args.package {
        // If a specific package is requested, show its sub-tree
        vec![(pkg_filter.clone(), "*".to_string())]
    } else {
        // Show from root composer.json
        let mut reqs: Vec<(String, String)> = root
            .require
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !args.no_dev {
            reqs.extend(root.require_dev.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        reqs.sort_by(|a, b| a.0.cmp(&b.0));
        reqs
    };

    // Print root
    println!(
        "{} {}",
        crate::console::info(&root.name),
        crate::console::comment(root.description.as_deref().unwrap_or(""))
    );

    // Render each root dependency as a tree
    let mut visited_global: HashSet<String> = HashSet::new();
    let count = root_reqs.len();
    for (i, (dep_name, dep_constraint)) in root_reqs.iter().enumerate() {
        let is_last = i == count - 1;
        let prefix = if is_last { "└──" } else { "├──" };
        let child_prefix = if is_last { "    " } else { "│   " };

        print_tree_node(
            dep_name,
            dep_constraint,
            &pkg_map,
            prefix,
            child_prefix,
            &mut visited_global,
            0,
        );
    }

    Ok(())
}

fn print_tree_node(
    pkg_name: &str,
    constraint: &str,
    pkg_map: &HashMap<String, &crate::lockfile::LockedPackage>,
    prefix: &str,
    child_prefix: &str,
    visited: &mut HashSet<String>,
    depth: usize,
) {
    const MAX_DEPTH: usize = 10;

    let key = pkg_name.to_lowercase();

    // Look up the package in the lock file
    if let Some(pkg) = pkg_map.get(&key) {
        let description = pkg.description.as_deref().unwrap_or("");
        let version = format_version(&pkg.version);

        println!(
            "{} {} {} {}",
            prefix,
            crate::console::info(pkg_name),
            crate::console::comment(&version),
            description
        );

        // Detect circular dependency or depth limit
        if visited.contains(&key) || depth >= MAX_DEPTH {
            if visited.contains(&key) {
                println!("{}    {} (circular dependency)", child_prefix, pkg_name);
            }
            return;
        }

        visited.insert(key.clone());

        // Print children (require only, not require-dev for transitive)
        let children: Vec<(&String, &String)> = pkg.require.iter().collect();
        let child_count = children.len();
        for (ci, (child_name, child_constraint)) in children.iter().enumerate() {
            let child_key = child_name.to_lowercase();
            // Skip platform packages
            if is_platform_package(&child_key) {
                continue;
            }
            let is_last_child = ci == child_count - 1;
            let child_node_prefix = format!(
                "{}{}",
                child_prefix,
                if is_last_child {
                    "└──"
                } else {
                    "├──"
                }
            );
            let grandchild_prefix = format!(
                "{}{}",
                child_prefix,
                if is_last_child { "    " } else { "│   " }
            );

            print_tree_node(
                child_name,
                child_constraint,
                pkg_map,
                &child_node_prefix,
                &grandchild_prefix,
                visited,
                depth + 1,
            );
        }

        visited.remove(&key);
    } else {
        // Package not found in lock file (platform package or not installed)
        if !is_platform_package(&key) {
            println!(
                "{} {} {} (not installed)",
                prefix,
                crate::console::comment(pkg_name),
                constraint
            );
        }
    }
}

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

// ─── Platform mode ─────────────────────────────────────────────────────────

fn show_platform(args: &ShowArgs, working_dir: &Path) -> anyhow::Result<()> {
    // Collect platform info from lock file and system detection
    let mut platform_packages: Vec<(String, String, String)> = Vec::new(); // (name, version, source)

    // Try to detect PHP from the system
    let php_version = detect_php_version();

    // Load platform requirements from lock file if available
    let lock_path = working_dir.join("composer.lock");
    if lock_path.exists() {
        let lock = crate::lockfile::LockFile::read_from_file(&lock_path)?;

        // Collect platform entries from lock's platform field
        if let Some(obj) = lock.platform.as_object() {
            for (name, version_val) in obj {
                let version_str = version_val.as_str().unwrap_or("*").to_string();
                platform_packages.push((name.clone(), version_str, "lock".to_string()));
            }
        }
        if let Some(obj) = lock.platform_dev.as_object()
            && !args.no_dev
        {
            for (name, version_val) in obj {
                let version_str = version_val.as_str().unwrap_or("*").to_string();
                // Only add if not already present
                if !platform_packages.iter().any(|(n, _, _)| n == name) {
                    platform_packages.push((name.clone(), version_str, "lock-dev".to_string()));
                }
            }
        }
    }

    // Add detected PHP version if available and not already listed
    if let Some(ref ver) = php_version
        && !platform_packages.iter().any(|(n, _, _)| n == "php")
    {
        platform_packages.push(("php".to_string(), ver.clone(), "detected".to_string()));
    }

    // Detect PHP extensions if PHP is available
    let extensions = detect_php_extensions();
    for ext in &extensions {
        let ext_name = format!("ext-{ext}");
        if !platform_packages.iter().any(|(n, _, _)| *n == ext_name) {
            platform_packages.push((ext_name, "*".to_string(), "detected".to_string()));
        }
    }

    // Sort
    platform_packages.sort_by(|a, b| a.0.cmp(&b.0));

    // Determine format
    let format = args.format.as_deref().unwrap_or("text");
    if format == "json" {
        let json_entries: Vec<serde_json::Value> = platform_packages
            .iter()
            .map(|(name, version, source)| {
                serde_json::json!({
                    "name": name,
                    "version": version,
                    "source": source,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "platform": json_entries }))?
        );
        return Ok(());
    }

    if platform_packages.is_empty() {
        eprintln!(
            "No platform packages detected. Install PHP or add platform requirements to composer.json."
        );
        return Ok(());
    }

    if args.name_only {
        for (name, _, _) in &platform_packages {
            println!("{name}");
        }
        return Ok(());
    }

    let name_width = platform_packages
        .iter()
        .map(|(n, _, _)| n.len())
        .max()
        .unwrap_or(0);
    let version_width = platform_packages
        .iter()
        .map(|(_, v, _)| v.len())
        .max()
        .unwrap_or(0);

    for (name, version, _source) in &platform_packages {
        println!(
            "{} {}",
            crate::console::info(&format!("{:<width$}", name, width = name_width)),
            crate::console::comment(&format!("{:<width$}", version, width = version_width)),
        );
    }

    Ok(())
}

/// Try to detect the installed PHP version by running `php --version`.
fn detect_php_version() -> Option<String> {
    let output = std::process::Command::new("php")
        .arg("--version")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse "PHP 8.2.1 (cli) ..." → "8.2.1"
    let first_line = stdout.lines().next()?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() >= 2 && parts[0] == "PHP" {
        Some(parts[1].to_string())
    } else {
        None
    }
}

/// Try to detect PHP extensions by running `php -m`.
fn detect_php_extensions() -> Vec<String> {
    let output = match std::process::Command::new("php").arg("-m").output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };

    if !output.status.success() {
        return vec![];
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|line| {
            let l = line.trim();
            !l.is_empty()
                && !l.starts_with('[')
                && l.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        })
        .map(|l| l.trim().to_lowercase())
        .collect()
}

// ─── Available mode ─────────────────────────────────────────────────────────

fn show_available(args: &ShowArgs, working_dir: &Path) -> anyhow::Result<()> {
    // If a specific package name is given, show available versions for it
    if let Some(ref pkg_name) = args.package {
        return show_available_versions(pkg_name, args);
    }

    // Otherwise, show all installed packages with their available (latest) versions
    // by querying Packagist for each installed package
    let vendor_dir = working_dir.join("vendor");
    let installed = crate::installed::InstalledPackages::read(&vendor_dir);

    let installed = match installed {
        Ok(i) if !i.packages.is_empty() => i,
        _ => {
            // Try lock file
            let lock_path = working_dir.join("composer.lock");
            if lock_path.exists() {
                let lock = crate::lockfile::LockFile::read_from_file(&lock_path)?;
                println!(
                    "{}",
                    crate::console::info(
                        "Available versions for locked packages (from Packagist):"
                    )
                );
                println!();

                let mut all_packages: Vec<&crate::lockfile::LockedPackage> =
                    lock.packages.iter().collect();
                if !args.no_dev
                    && let Some(ref dev_pkgs) = lock.packages_dev
                {
                    all_packages.extend(dev_pkgs.iter());
                }

                for pkg in &all_packages {
                    if is_platform_package(&pkg.name) {
                        continue;
                    }
                    show_available_versions_inline(&pkg.name);
                }
                return Ok(());
            }

            eprintln!(
                "{}",
                crate::console::warning(
                    "No dependencies installed. Try running mozart install or update."
                )
            );
            return Ok(());
        }
    };

    println!(
        "{}",
        crate::console::info("Available versions for installed packages (from Packagist):")
    );
    println!();

    let format = args.format.as_deref().unwrap_or("text");

    if format == "json" {
        let mut json_entries: Vec<serde_json::Value> = Vec::new();
        for pkg in &installed.packages {
            if is_platform_package(&pkg.name) {
                continue;
            }
            match crate::packagist::fetch_package_versions(&pkg.name, None) {
                Ok(versions) => {
                    let version_strings: Vec<String> =
                        versions.iter().map(|v| v.version.clone()).collect();
                    json_entries.push(serde_json::json!({
                        "name": pkg.name,
                        "installed": pkg.version,
                        "available": version_strings,
                    }));
                }
                Err(_) => {
                    json_entries.push(serde_json::json!({
                        "name": pkg.name,
                        "installed": pkg.version,
                        "available": [],
                    }));
                }
            }
        }
        let output = serde_json::json!({ "packages": json_entries });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    for pkg in &installed.packages {
        if is_platform_package(&pkg.name) {
            continue;
        }
        show_available_versions_inline(&pkg.name);
    }

    Ok(())
}

fn show_available_versions(pkg_name: &str, args: &ShowArgs) -> anyhow::Result<()> {
    let versions = crate::packagist::fetch_package_versions(pkg_name, None)?;
    if versions.is_empty() {
        println!("No versions found for {pkg_name}");
        return Ok(());
    }

    let format = args.format.as_deref().unwrap_or("text");
    if format == "json" {
        let version_strings: Vec<String> = versions.iter().map(|v| v.version.clone()).collect();
        let output = serde_json::json!({
            "name": pkg_name,
            "versions": version_strings,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!(
        "{}",
        crate::console::info(&format!("Available versions for {pkg_name}:"))
    );
    for v in &versions {
        println!("  {}", crate::console::comment(&v.version));
    }
    Ok(())
}

fn show_available_versions_inline(pkg_name: &str) {
    match crate::packagist::fetch_package_versions(pkg_name, None) {
        Ok(versions) => {
            if versions.is_empty() {
                println!("{}: no versions found", crate::console::info(pkg_name));
                return;
            }
            // Show up to 5 most recent versions
            let shown: Vec<&str> = versions
                .iter()
                .take(5)
                .map(|v| v.version.as_str())
                .collect();
            let rest = if versions.len() > 5 {
                format!(" (+{} more)", versions.len() - 5)
            } else {
                String::new()
            };
            println!(
                "{}: {}{}",
                crate::console::info(pkg_name),
                crate::console::comment(&shown.join(", ")),
                rest
            );
        }
        Err(_) => {
            println!(
                "{}: (could not fetch from Packagist)",
                crate::console::comment(pkg_name)
            );
        }
    }
}

// ─── Helper functions ──────────────────────────────────────────────────────

/// Format version string for display: strip leading 'v' for text output.
fn format_version(version: &str) -> String {
    version.strip_prefix('v').unwrap_or(version).to_string()
}

/// Format version with highlight for the detail view (asterisk prefix).
fn format_version_highlight(version: &str) -> String {
    format!("* {}", format_version(version))
}

/// Extract description from an InstalledPackageEntry's extra_fields.
fn get_installed_description(pkg: &crate::installed::InstalledPackageEntry) -> String {
    pkg.extra_fields
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract keywords from an InstalledPackageEntry's extra_fields.
fn get_installed_keywords(pkg: &crate::installed::InstalledPackageEntry) -> String {
    pkg.extra_fields
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Extract license from an InstalledPackageEntry's extra_fields.
fn get_installed_license(pkg: &crate::installed::InstalledPackageEntry) -> Option<String> {
    pkg.extra_fields.get("license").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
    })
}

/// Extract homepage from an InstalledPackageEntry's extra_fields.
fn get_installed_homepage(pkg: &crate::installed::InstalledPackageEntry) -> Option<String> {
    pkg.extra_fields
        .get("homepage")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Resolve a path to its canonical form, falling back to the display form.
fn resolve_path(path: &Path) -> String {
    if path.exists() {
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    } else {
        path.display().to_string()
    }
}

/// Match a package name against a wildcard pattern (case-insensitive).
/// `*` matches any sequence of characters.
fn matches_wildcard(name: &str, pattern: &str) -> bool {
    let name_lower = name.to_lowercase();
    let pattern_lower = pattern.to_lowercase();
    let parts: Vec<&str> = pattern_lower.split('*').collect();

    if parts.len() == 1 {
        return name_lower == pattern_lower;
    }

    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match name_lower[pos..].find(*part) {
            Some(found) => {
                if i == 0 && found != 0 {
                    return false; // First segment must match at start
                }
                pos += found + part.len();
            }
            None => return false,
        }
    }

    // If pattern doesn't end with *, name must be fully consumed
    if !pattern_lower.ends_with('*') {
        return pos == name_lower.len();
    }

    true
}

/// Simple version normalizer fallback when `version_normalized` is absent.
fn normalize_version_simple(version: &str) -> String {
    let v = version.strip_prefix('v').unwrap_or(version);
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

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_version ──────────────────────────────────────────────────────

    #[test]
    fn test_format_version_strips_v() {
        assert_eq!(format_version("v1.2.3"), "1.2.3");
    }

    #[test]
    fn test_format_version_no_v() {
        assert_eq!(format_version("1.2.3"), "1.2.3");
    }

    #[test]
    fn test_format_version_keeps_dev() {
        assert_eq!(format_version("dev-main"), "dev-main");
    }

    // ── matches_wildcard ─────────────────────────────────────────────────────

    #[test]
    fn test_matches_wildcard_exact() {
        assert!(matches_wildcard("psr/log", "psr/log"));
    }

    #[test]
    fn test_matches_wildcard_star_end() {
        assert!(matches_wildcard("psr/log", "psr/*"));
    }

    #[test]
    fn test_matches_wildcard_star_start() {
        assert!(matches_wildcard("psr/log", "*/log"));
    }

    #[test]
    fn test_matches_wildcard_star_middle() {
        assert!(matches_wildcard("monolog/monolog", "mono*/mono*"));
    }

    #[test]
    fn test_matches_wildcard_no_match() {
        assert!(!matches_wildcard("psr/log", "symfony/*"));
    }

    #[test]
    fn test_matches_wildcard_case_insensitive() {
        assert!(matches_wildcard("PSR/Log", "psr/*"));
    }

    #[test]
    fn test_matches_wildcard_star_both_ends() {
        assert!(matches_wildcard("monolog/monolog", "*log*"));
    }

    #[test]
    fn test_matches_wildcard_no_wildcard_mismatch() {
        assert!(!matches_wildcard("psr/log", "psr/log2"));
    }

    #[test]
    fn test_matches_wildcard_trailing_chars_fail() {
        // pattern "psr/l" does not end with * so "psr/log" should not match
        assert!(!matches_wildcard("psr/log", "psr/l"));
    }

    // ── format_version_highlight ────────────────────────────────────────────

    #[test]
    fn test_format_version_highlight() {
        assert_eq!(format_version_highlight("v3.0.0"), "* 3.0.0");
        assert_eq!(format_version_highlight("3.0.0"), "* 3.0.0");
    }

    // ── get_installed_description ────────────────────────────────────────────

    #[test]
    fn test_get_installed_description_present() {
        use std::collections::BTreeMap;
        let mut extra = BTreeMap::new();
        extra.insert(
            "description".to_string(),
            serde_json::Value::String("A logging library".to_string()),
        );
        let pkg = crate::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: extra,
        };
        assert_eq!(get_installed_description(&pkg), "A logging library");
    }

    #[test]
    fn test_get_installed_description_absent() {
        use std::collections::BTreeMap;
        let pkg = crate::installed::InstalledPackageEntry {
            name: "psr/log".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        };
        assert_eq!(get_installed_description(&pkg), "");
    }

    // ── get_installed_keywords ───────────────────────────────────────────────

    #[test]
    fn test_get_installed_keywords() {
        use std::collections::BTreeMap;
        let mut extra = BTreeMap::new();
        extra.insert(
            "keywords".to_string(),
            serde_json::json!(["log", "psr3", "logging"]),
        );
        let pkg = crate::installed::InstalledPackageEntry {
            name: "psr/log".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: extra,
        };
        assert_eq!(get_installed_keywords(&pkg), "log, psr3, logging");
    }

    // ── is_platform_package ───────────────────────────────────────────────────

    #[test]
    fn test_is_platform_package_php() {
        assert!(is_platform_package("php"));
    }

    #[test]
    fn test_is_platform_package_ext() {
        assert!(is_platform_package("ext-json"));
        assert!(is_platform_package("ext-mbstring"));
    }

    #[test]
    fn test_is_platform_package_lib() {
        assert!(is_platform_package("lib-pcre"));
    }

    #[test]
    fn test_is_platform_package_not_platform() {
        assert!(!is_platform_package("monolog/monolog"));
        assert!(!is_platform_package("psr/log"));
    }

    // ── classify_update_category ─────────────────────────────────────────────

    #[test]
    fn test_classify_up_to_date() {
        assert_eq!(
            classify_update_category("1.2.3.0", "1.2.3.0"),
            ListUpdateKind::UpToDate
        );
    }

    #[test]
    fn test_classify_compatible_same_major() {
        assert_eq!(
            classify_update_category("1.2.0.0", "1.3.0.0"),
            ListUpdateKind::Compatible
        );
    }

    #[test]
    fn test_classify_incompatible_different_major() {
        assert_eq!(
            classify_update_category("1.9.0.0", "2.0.0.0"),
            ListUpdateKind::Incompatible
        );
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
    fn test_normalize_version_simple_v_prefix() {
        assert_eq!(normalize_version_simple("v1.2.3"), "1.2.3.0");
    }

    // ── extract_major ─────────────────────────────────────────────────────────

    #[test]
    fn test_extract_major_basic() {
        assert_eq!(extract_major("2.3.4.0"), 2);
        assert_eq!(extract_major("0.1.2.0"), 0);
    }

    #[test]
    fn test_extract_major_with_prerelease() {
        assert_eq!(extract_major("2.3.4.0-beta1"), 2);
    }
}
