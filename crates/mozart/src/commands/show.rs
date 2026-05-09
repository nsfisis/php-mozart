use clap::Args;
use indexmap::{IndexMap, IndexSet};
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::matches_wildcard;
use mozart_core::platform::is_platform_package;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default, Args)]
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
    #[arg(short, long, default_value = "text")]
    pub format: String,

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

pub async fn execute(
    args: &ShowArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let cache_config = mozart_registry::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_registry::cache::Cache::repo(&cache_config);

    // A9: --installed deprecation warning (mirrors Composer 143-145)
    if args.installed && !args.self_info {
        console_writeln_error!(
            console,
            "<warning>You are using the deprecated option \"installed\". Only installed packages are shown by default now. The --all option can be used to show all packages.</warning>",
        );
    }

    // Validate mutually exclusive level filters
    let level_count = args.major_only as u8 + args.minor_only as u8 + args.patch_only as u8;
    if level_count > 1 {
        anyhow::bail!("Only one of --major-only, --minor-only or --patch-only can be used at once");
    }

    // --direct with --all, --platform, or --available
    if args.direct && (args.all || args.platform || args.available) {
        anyhow::bail!(
            "The --direct (-D) option is not usable in combination with --all, --platform (-p) or --available (-a)"
        );
    }

    // --tree with --all or --available
    if args.tree && (args.all || args.available) {
        anyhow::bail!(
            "The --tree (-t) option is not usable in combination with --all or --available (-a)"
        );
    }

    // --tree with --latest
    if args.tree && args.latest {
        anyhow::bail!("The --tree (-t) option is not usable in combination with --latest (-l)");
    }

    // --tree with --path
    if args.tree && args.path {
        anyhow::bail!("The --tree (-t) option is not usable in combination with --path (-P)");
    }

    // --format validation
    if args.format != "text" && args.format != "json" {
        anyhow::bail!(
            "Unsupported format \"{}\". See help for supported formats.",
            args.format
        );
    }

    // --self with a package argument
    if args.self_info && args.package.is_some() {
        anyhow::bail!("You cannot use --self together with a package name");
    }

    // --ignore without --outdated warning
    if !args.ignore.is_empty() && !args.outdated {
        console_writeln_error!(
            console,
            "<warning>You are using the option \"ignore\" for action other than \"outdated\", it will be ignored.</warning>",
        );
    }

    let working_dir = cli.working_dir()?;

    // --platform: show detected platform packages
    if args.platform {
        return show_platform(args, &working_dir, console);
    }

    // --self: show root package info
    if args.self_info && !args.installed && !args.locked {
        return show_self(args, &working_dir, console);
    }

    // --tree: show dependency tree
    if args.tree {
        return show_tree(args, &working_dir, console);
    }

    // --available: show available versions
    if args.available {
        return show_available(args, &working_dir, &repo_cache, console).await;
    }

    // --locked: show from lock file
    if args.locked {
        return execute_locked(args, &working_dir, &repo_cache, console).await;
    }

    // Default: installed mode
    execute_installed(args, &working_dir, &repo_cache, console).await
}

// ============================================================================
// Unified types
// ============================================================================

/// Mirrors Composer's latest-package data used in list view.
struct LatestInfo {
    version: String,
    version_normalized: String,
    /// None = not abandoned; Some("") = abandoned, no replacement suggested;
    /// Some("vendor/pkg") = abandoned, replacement suggested.
    abandoned: Option<String>,
}

/// Unified per-row data for the package list view.
struct PackageEntry {
    name: String,
    version: String,
    version_normalized: String,
    description: String,
    /// True when this package is a direct root requirement.
    is_direct: bool,
    /// Release date string from the package metadata (for --sort-by-age).
    release_date: Option<String>,
    latest_info: Option<LatestInfo>,
}

/// Unified data for the single-package detail view. Mirrors Composer's
/// `printPackageInfo` + `printMeta` + `printLinks`.
struct PackageDetail {
    name: String,
    description: String,
    keywords: Vec<String>,
    version: String,
    package_type: Option<String>,
    licenses: Vec<String>,
    homepage: Option<String>,
    source_type: Option<String>,
    source_url: Option<String>,
    source_ref: Option<String>,
    dist_type: Option<String>,
    dist_url: Option<String>,
    dist_ref: Option<String>,
    install_path: Option<String>,
    /// A13: release date ("released" field).
    release_date: Option<String>,
    /// A13: all names (canonical + provides + replaces).
    names: Vec<String>,
    /// A13: support links object.
    support: Option<serde_json::Value>,
    /// A13: autoload rules.
    autoload: Option<serde_json::Value>,
    require: BTreeMap<String, String>,
    require_dev: BTreeMap<String, String>,
    /// A12: conflict links.
    conflict: BTreeMap<String, String>,
    /// A12: provide links.
    provide: BTreeMap<String, String>,
    /// A12: replace links.
    replace: BTreeMap<String, String>,
    suggest: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListUpdateKind {
    UpToDate,
    Compatible,
    Incompatible,
}

// ============================================================================
// Helper utilities
// ============================================================================

/// Compute the set of direct-dependency package names from composer.json.
fn compute_direct_names(working_dir: &Path, no_dev: bool) -> anyhow::Result<IndexSet<String>> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        return Ok(IndexSet::new());
    }
    let root = mozart_core::package::read_from_file(&composer_json_path)?;
    let mut names: IndexSet<String> = root.require.keys().map(|k| k.to_lowercase()).collect();
    if !no_dev {
        names.extend(root.require_dev.keys().map(|k| k.to_lowercase()));
    }
    Ok(names)
}

/// Fetch the latest version of a package from Packagist, applying
/// --major-only / --minor-only / --patch-only constraints (A3).
async fn fetch_latest_for_package(
    name: &str,
    current_normalized: &str,
    args: &ShowArgs,
    repo_cache: &mozart_registry::cache::Cache,
) -> anyhow::Result<LatestInfo> {
    use mozart_core::package::Stability;
    use mozart_registry::version::find_best_candidate;

    let versions = mozart_registry::packagist::fetch_package_versions(name, repo_cache).await?;

    let current_major = extract_major(current_normalized);
    let current_minor = extract_minor(current_normalized);

    // Mirrors Composer ShowCommand::findLatestPackage 1494-1496:
    // dev-versioned packages cannot use major-only filtering.
    let is_dev = current_normalized.starts_with("dev-") || current_normalized.ends_with("-dev");
    if args.major_only && is_dev {
        anyhow::bail!("Cannot determine major update for dev version of {name}");
    }

    let filtered: Vec<mozart_registry::packagist::PackagistVersion> = versions
        .iter()
        .filter(|v| {
            let v_norm = &v.version_normalized;
            let v_major = extract_major(v_norm);
            let v_minor = extract_minor(v_norm);
            if args.major_only {
                v_major > current_major
            } else if args.minor_only {
                v_major == current_major
            } else if args.patch_only {
                v_major == current_major && v_minor == current_minor
            } else {
                true
            }
        })
        .cloned()
        .collect();

    let best = find_best_candidate(&filtered, Stability::Stable)
        .ok_or_else(|| anyhow::anyhow!("No suitable version found for {name}"))?;

    let abandoned = best.abandoned.as_ref().and_then(abandoned_info);

    Ok(LatestInfo {
        version: best.version.clone(),
        version_normalized: best.version_normalized.clone(),
        abandoned,
    })
}

/// Extract the abandonment string from a Packagist `abandoned` field value.
/// Returns None if the package is not abandoned.
fn abandoned_info(val: &serde_json::Value) -> Option<String> {
    match val {
        serde_json::Value::Bool(true) => Some(String::new()),
        serde_json::Value::String(s) if !s.is_empty() && s != "false" => Some(s.clone()),
        _ => None,
    }
}

fn classify_update_category(current_normalized: &str, latest_normalized: &str) -> ListUpdateKind {
    use mozart_registry::version::compare_normalized_versions;
    use std::cmp::Ordering;

    if compare_normalized_versions(latest_normalized, current_normalized) != Ordering::Greater {
        return ListUpdateKind::UpToDate;
    }

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

// ============================================================================
// List entry collection
// ============================================================================

async fn collect_installed_entries(
    packages: &[&mozart_registry::installed::InstalledPackageEntry],
    args: &ShowArgs,
    direct_names: &IndexSet<String>,
    repo_cache: &mozart_registry::cache::Cache,
) -> Vec<PackageEntry> {
    let show_latest = args.latest || args.outdated;
    let mut entries = Vec::new();

    for pkg in packages {
        if args
            .ignore
            .iter()
            .any(|pattern| matches_wildcard(&pkg.name, pattern))
        {
            continue;
        }

        let version_normalized = pkg
            .version_normalized
            .clone()
            .unwrap_or_else(|| normalize_version_simple(&pkg.version));
        let description = get_installed_description(pkg);
        let is_direct = direct_names.contains(&pkg.name.to_lowercase());
        let release_date = get_installed_release_date(pkg);

        let latest_info = if show_latest {
            fetch_latest_for_package(&pkg.name, &version_normalized, args, repo_cache)
                .await
                .ok()
        } else {
            None
        };

        if args.outdated {
            if let Some(ref li) = latest_info {
                use mozart_registry::version::compare_normalized_versions;
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

        entries.push(PackageEntry {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            version_normalized,
            description,
            is_direct,
            release_date,
            latest_info,
        });
    }

    entries
}

async fn collect_locked_entries(
    packages: &[&mozart_registry::lockfile::LockedPackage],
    args: &ShowArgs,
    direct_names: &IndexSet<String>,
    repo_cache: &mozart_registry::cache::Cache,
) -> Vec<PackageEntry> {
    let show_latest = args.latest || args.outdated;
    let mut entries = Vec::new();

    for pkg in packages {
        if args
            .ignore
            .iter()
            .any(|pattern| matches_wildcard(&pkg.name, pattern))
        {
            continue;
        }

        let version_normalized = pkg
            .version_normalized
            .clone()
            .unwrap_or_else(|| normalize_version_simple(&pkg.version));
        let description = pkg.description.as_deref().unwrap_or("").to_string();
        let is_direct = direct_names.contains(&pkg.name.to_lowercase());
        let release_date = pkg.time.clone();

        let latest_info = if show_latest {
            fetch_latest_for_package(&pkg.name, &version_normalized, args, repo_cache)
                .await
                .ok()
        } else {
            None
        };

        if args.outdated {
            if let Some(ref li) = latest_info {
                use mozart_registry::version::compare_normalized_versions;
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

        entries.push(PackageEntry {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            version_normalized,
            description,
            is_direct,
            release_date,
            latest_info,
        });
    }

    entries
}

// ============================================================================
// List rendering (unified)
// ============================================================================

/// Render the package list view. Returns true if any package is outdated
/// (for --strict handling). Mirrors Composer's list-view block (398–710).
fn render_package_list(
    entries: &mut [PackageEntry],
    args: &ShowArgs,
    section_key: &str,
    console: &mozart_core::console::Console,
) -> anyhow::Result<bool> {
    let show_latest = args.latest || args.outdated;

    // A4: --sort-by-age (mirrors Composer 497-504)
    if args.sort_by_age {
        entries.sort_by(|a, b| a.release_date.cmp(&b.release_date));
    }

    let has_outdated = entries.iter().any(|e| e.latest_info.is_some());

    if args.format == "json" {
        render_list_json(entries, section_key, console)?;
        return Ok(has_outdated);
    }

    // A6: Color legend (mirrors Composer 626-642)
    if show_latest && !entries.is_empty() {
        print_color_legend(console);
    }

    // A7: Direct/Transitive split (mirrors Composer 671-695)
    // Only applies when --latest is on and --direct is not set.
    if show_latest && !args.direct {
        let direct_entries: Vec<&PackageEntry> = entries.iter().filter(|e| e.is_direct).collect();
        let transitive_entries: Vec<&PackageEntry> =
            entries.iter().filter(|e| !e.is_direct).collect();

        console_writeln!(
            console,
            "<info>Direct dependencies required in composer.json:</info>",
        );
        if direct_entries.is_empty() {
            console_writeln!(console, "Everything up to date");
        } else {
            print_package_rows(&direct_entries, args, console);
        }

        console_writeln!(console, "");
        console_writeln!(
            console,
            "<info>Transitive dependencies not required in composer.json:</info>",
        );
        if transitive_entries.is_empty() {
            console_writeln!(console, "Everything up to date");
        } else {
            print_package_rows(&transitive_entries, args, console);
        }
    } else {
        let all_refs: Vec<&PackageEntry> = entries.iter().collect();
        print_package_rows(&all_refs, args, console);
    }

    Ok(has_outdated)
}

/// Print a row for each entry. Applies A5 (abandoned warning) and A6
/// (ASCII prefix markers in non-decorated mode).
fn print_package_rows(
    entries: &[&PackageEntry],
    args: &ShowArgs,
    console: &mozart_core::console::Console,
) {
    let show_latest = args.latest || args.outdated;

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

    for entry in entries {
        let version = format_version(&entry.version);
        let category = entry
            .latest_info
            .as_ref()
            .map(|li| classify_update_category(&entry.version_normalized, &li.version_normalized));

        let name_str = match category {
            Some(ListUpdateKind::Compatible) => {
                console_format!(
                    "<highlight>{:<width$}</highlight>",
                    entry.name,
                    width = name_width
                )
            }
            Some(ListUpdateKind::Incompatible) => {
                console_format!(
                    "<comment>{:<width$}</comment>",
                    entry.name,
                    width = name_width
                )
            }
            _ => {
                console_format!("<info>{:<width$}</info>", entry.name, width = name_width)
            }
        };

        let version_str = console_format!(
            "<comment>{:<width$}</comment>",
            version,
            width = version_width
        );

        // A6: ASCII prefix markers for non-decorated terminals (Composer 736/1438)
        let ascii_prefix = if !console.decorated && show_latest {
            match category {
                Some(ListUpdateKind::Compatible) => "! ",
                Some(ListUpdateKind::Incompatible) => "~ ",
                Some(ListUpdateKind::UpToDate) => "= ",
                None => "",
            }
        } else {
            ""
        };

        if show_latest {
            let latest_str = match entry.latest_info.as_ref() {
                Some(li) => {
                    let lv = format_version(&li.version);
                    match category {
                        Some(ListUpdateKind::Compatible) => {
                            console_format!(
                                "<highlight>{:<width$}</highlight>",
                                lv,
                                width = latest_width
                            )
                        }
                        Some(ListUpdateKind::Incompatible) => {
                            console_format!(
                                "<comment>{:<width$}</comment>",
                                lv,
                                width = latest_width
                            )
                        }
                        _ => {
                            console_format!("<info>{:<width$}</info>", lv, width = latest_width)
                        }
                    }
                }
                None => format!("{:<width$}", "", width = latest_width),
            };
            console_writeln!(
                console,
                "{}{} {} {} {}",
                ascii_prefix,
                name_str,
                version_str,
                latest_str,
                entry.description,
            );
        } else {
            console_writeln!(
                console,
                "{}{} {} {}",
                ascii_prefix,
                name_str,
                version_str,
                entry.description,
            );
        }

        // A5: Abandoned warning (mirrors Composer printPackages 778-780)
        if let Some(ref li) = entry.latest_info
            && let Some(ref replacement) = li.abandoned
        {
            let msg = if replacement.is_empty() {
                format!(
                    "Package {} is abandoned, you should avoid using it. No replacement was suggested.",
                    entry.name
                )
            } else {
                format!(
                    "Package {} is abandoned, you should avoid using it. Use {} instead.",
                    entry.name, replacement
                )
            };
            console_writeln_error!(console, "<warning>{}</warning>", msg);
        }
    }
}

/// Print the color legend before the list (A6, mirrors Composer 626-642).
fn print_color_legend(console: &mozart_core::console::Console) {
    if console.decorated {
        console_writeln!(console, "<info>Color legend:</info>");
        console_writeln!(
            console,
            "- {} release available - update recommended",
            console_format!("<highlight>patch or minor</highlight>"),
        );
        console_writeln!(
            console,
            "- {} release available - update possible",
            console_format!("<comment>major</comment>"),
        );
        console_writeln!(
            console,
            "- {} version",
            console_format!("<info>up to date</info>"),
        );
    } else {
        console_writeln!(console, "Legend:");
        console_writeln!(
            console,
            "! patch or minor release available - update recommended",
        );
        console_writeln!(console, "~ major release available - update possible");
        console_writeln!(console, "= up to date version");
    }
    console_writeln!(console, "");
}

/// Emit the JSON list output. Uses `section_key` as the top-level key
/// (A14: "installed" vs "locked" vs "platform" etc.).
fn render_list_json(
    entries: &[PackageEntry],
    section_key: &str,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
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

    let output = serde_json::json!({ section_key: json_entries });
    console_writeln!(console, "{}", &serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ============================================================================
// Detail view (unified — A15)
// ============================================================================

/// Build a `PackageDetail` from an installed package entry.
fn installed_to_detail(
    pkg: &mozart_registry::installed::InstalledPackageEntry,
    vendor_dir: &Path,
) -> PackageDetail {
    let install_path = vendor_dir.join(&pkg.name);
    let path_str = if install_path.exists() {
        Some(install_path.display().to_string())
    } else {
        None
    };

    let (source_type, source_url, source_ref) = match &pkg.source {
        Some(src) => (
            src.get("type").and_then(|v| v.as_str()).map(str::to_string),
            src.get("url").and_then(|v| v.as_str()).map(str::to_string),
            src.get("reference")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        ),
        None => (None, None, None),
    };

    let (dist_type, dist_url, dist_ref) = match &pkg.dist {
        Some(d) => (
            d.get("type").and_then(|v| v.as_str()).map(str::to_string),
            d.get("url").and_then(|v| v.as_str()).map(str::to_string),
            d.get("reference")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        ),
        None => (None, None, None),
    };

    let provide = get_installed_link_map(pkg, "provide");
    let replace = get_installed_link_map(pkg, "replace");

    let mut names = vec![pkg.name.clone()];
    names.extend(provide.keys().cloned());
    names.extend(replace.keys().cloned());

    PackageDetail {
        name: pkg.name.clone(),
        description: get_installed_description(pkg),
        keywords: get_installed_keywords_vec(pkg),
        version: pkg.version.clone(),
        package_type: pkg.package_type.clone(),
        licenses: get_installed_licenses(pkg),
        homepage: get_installed_homepage(pkg),
        source_type,
        source_url,
        source_ref,
        dist_type,
        dist_url,
        dist_ref,
        install_path: path_str,
        release_date: get_installed_release_date(pkg),
        names,
        support: pkg.extra_fields.get("support").cloned(),
        autoload: pkg.autoload.clone(),
        require: get_installed_link_map(pkg, "require"),
        require_dev: get_installed_link_map(pkg, "require-dev"),
        conflict: get_installed_link_map(pkg, "conflict"),
        provide,
        replace,
        suggest: get_installed_suggest_map(pkg),
    }
}

/// Build a `PackageDetail` from a locked package entry.
fn locked_to_detail(pkg: &mozart_registry::lockfile::LockedPackage) -> PackageDetail {
    let mut names = vec![pkg.name.clone()];
    names.extend(pkg.provide.keys().cloned());
    names.extend(pkg.replace.keys().cloned());

    let (source_type, source_url, source_ref) = match &pkg.source {
        Some(src) => (
            Some(src.source_type.clone()),
            Some(src.url.clone()),
            src.reference.clone(),
        ),
        None => (None, None, None),
    };

    let (dist_type, dist_url, dist_ref) = match &pkg.dist {
        Some(d) => (
            Some(d.dist_type.clone()),
            Some(d.url.clone()),
            d.reference.clone(),
        ),
        None => (None, None, None),
    };

    PackageDetail {
        name: pkg.name.clone(),
        description: pkg.description.as_deref().unwrap_or("").to_string(),
        keywords: pkg.keywords.as_deref().unwrap_or(&[]).to_vec(),
        version: pkg.version.clone(),
        package_type: pkg.package_type.clone(),
        licenses: pkg.license.as_deref().unwrap_or(&[]).to_vec(),
        homepage: pkg.homepage.clone(),
        source_type,
        source_url,
        source_ref,
        dist_type,
        dist_url,
        dist_ref,
        install_path: None,
        release_date: pkg.time.clone(),
        names,
        support: pkg.support.clone(),
        autoload: pkg.autoload.clone(),
        require: pkg.require.clone(),
        require_dev: pkg.require_dev.clone(),
        conflict: pkg.conflict.clone(),
        provide: pkg.provide.clone(),
        replace: pkg.replace.clone(),
        suggest: pkg.suggest.as_ref().cloned().unwrap_or_default(),
    }
}

/// Print single-package detail view. Mirrors Composer's `printPackageInfo` +
/// `printMeta` + `printLinks`. Shared by installed and locked paths (A15).
async fn print_package_detail(
    detail: &PackageDetail,
    args: &ShowArgs,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    if args.format == "json" {
        return print_package_detail_json(detail, args, repo_cache, console).await;
    }

    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>name</info>"),
        detail.name,
    );
    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>descrip.</info>"),
        detail.description,
    );
    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>keywords</info>"),
        detail.keywords.join(", "),
    );
    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>versions</info>"),
        format_version_highlight(&detail.version),
    );

    // A13: released
    if let Some(ref date) = detail.release_date {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>released</info>"),
            date,
        );
    }

    // A11: latest (when --latest is on)
    if args.latest || args.outdated {
        let version_normalized = normalize_version_simple(&detail.version);
        if let Ok(li) =
            fetch_latest_for_package(&detail.name, &version_normalized, args, repo_cache).await
        {
            let update_kind = classify_update_category(&version_normalized, &li.version_normalized);
            let latest_str = match update_kind {
                ListUpdateKind::Compatible => {
                    console_format!("<highlight>{}</highlight>", &li.version)
                }
                ListUpdateKind::Incompatible => {
                    console_format!("<comment>{}</comment>", &li.version)
                }
                ListUpdateKind::UpToDate => {
                    console_format!("<info>{}</info>", &li.version)
                }
            };
            console_writeln!(
                console,
                "{} : {}",
                console_format!("<info>latest</info>"),
                latest_str,
            );
        }
    }

    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>type</info>"),
        detail.package_type.as_deref().unwrap_or("library"),
    );

    for license_id in &detail.licenses {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>license</info>"),
            format_license_for_show(license_id),
        );
    }

    if let Some(ref homepage) = detail.homepage {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>homepage</info>"),
            homepage,
        );
    }

    if let Some(ref src_url) = detail.source_url {
        let src_type = detail.source_type.as_deref().unwrap_or("");
        let src_ref = detail.source_ref.as_deref().unwrap_or("");
        console_writeln!(
            console,
            "{} : [{}] {} {}",
            console_format!("<info>source</info>"),
            src_type,
            console_format!("<comment>{}</comment>", src_url),
            src_ref,
        );
    }

    if let Some(ref dist_url) = detail.dist_url {
        let dist_type = detail.dist_type.as_deref().unwrap_or("");
        let dist_ref = detail.dist_ref.as_deref().unwrap_or("");
        console_writeln!(
            console,
            "{} : [{}] {} {}",
            console_format!("<info>dist</info>"),
            dist_type,
            console_format!("<comment>{}</comment>", dist_url),
            dist_ref,
        );
    }

    if let Some(ref path) = detail.install_path {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>path</info>"),
            path,
        );
    }

    // A13: names (when multiple)
    if detail.names.len() > 1 {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>names</info>"),
            detail.names.join(", "),
        );
    }

    // A13: support
    if let Some(ref support) = detail.support
        && let Some(obj) = support.as_object()
        && !obj.is_empty()
    {
        console_writeln!(console, "");
        console_writeln!(console, "<info>support</info>");
        for (key, val) in obj {
            let v = val.as_str().unwrap_or("");
            console_writeln!(
                console,
                "{} {}",
                key,
                console_format!("<comment>{}</comment>", v),
            );
        }
    }

    // A13: autoload
    if let Some(ref autoload) = detail.autoload {
        console_writeln!(console, "");
        console_writeln!(console, "<info>autoload</info>");
        if let Some(obj) = autoload.as_object() {
            for (loader_type, config) in obj {
                match config {
                    serde_json::Value::Object(map) => {
                        for (k, v) in map {
                            let v_str = v.as_str().unwrap_or("");
                            console_writeln!(
                                console,
                                "{}: {} => {}",
                                loader_type,
                                k,
                                console_format!("<comment>{}</comment>", v_str),
                            );
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            let v_str = item.as_str().unwrap_or("");
                            console_writeln!(
                                console,
                                "{}: {}",
                                loader_type,
                                console_format!("<comment>{}</comment>", v_str),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Links: requires, requires-dev, conflict, provide, replace, suggests (A12)
    print_links_section("requires", &detail.require, console);
    print_links_section("requires (dev)", &detail.require_dev, console);
    print_links_section("conflict", &detail.conflict, console);
    print_links_section("provide", &detail.provide, console);
    print_links_section("replace", &detail.replace, console);
    print_links_section("suggests", &detail.suggest, console);

    Ok(())
}

/// Print a named section of package links (requires, conflict, etc.).
fn print_links_section(
    label: &str,
    links: &BTreeMap<String, String>,
    console: &mozart_core::console::Console,
) {
    if links.is_empty() {
        return;
    }
    console_writeln!(console, "");
    console_writeln!(console, "<info>{}</info>", label);
    for (name, constraint) in links {
        console_writeln!(
            console,
            "{} {}",
            name,
            console_format!("<comment>{}</comment>", constraint),
        );
    }
}

/// JSON output for single-package detail (mirrors Composer's
/// `printPackageInfoAsJson`).
async fn print_package_detail_json(
    detail: &PackageDetail,
    args: &ShowArgs,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let mut obj = serde_json::json!({
        "name": detail.name,
        "description": detail.description,
        "keywords": detail.keywords,
        "type": detail.package_type.as_deref().unwrap_or("library"),
        "homepage": detail.homepage,
        "license": detail.licenses,
        "versions": [format_version_highlight(&detail.version)],
    });

    if !detail.require.is_empty() {
        obj["require"] = serde_json::json!(detail.require);
    }
    if !detail.require_dev.is_empty() {
        obj["require-dev"] = serde_json::json!(detail.require_dev);
    }
    if !detail.conflict.is_empty() {
        obj["conflict"] = serde_json::json!(detail.conflict);
    }
    if !detail.provide.is_empty() {
        obj["provide"] = serde_json::json!(detail.provide);
    }
    if !detail.replace.is_empty() {
        obj["replace"] = serde_json::json!(detail.replace);
    }
    if !detail.suggest.is_empty() {
        obj["suggest"] = serde_json::json!(detail.suggest);
    }
    if let Some(ref date) = detail.release_date {
        obj["time"] = serde_json::Value::String(date.clone());
    }
    if let Some(ref support) = detail.support {
        obj["support"] = support.clone();
    }
    if let Some(ref autoload) = detail.autoload {
        obj["autoload"] = autoload.clone();
    }

    // A11: latest when --latest/--outdated
    if args.latest || args.outdated {
        let version_normalized = normalize_version_simple(&detail.version);
        if let Ok(li) =
            fetch_latest_for_package(&detail.name, &version_normalized, args, repo_cache).await
        {
            obj["latest"] = serde_json::Value::String(li.version.clone());
            let status = classify_update_category(&version_normalized, &li.version_normalized);
            obj["latest-status"] = serde_json::Value::String(match status {
                ListUpdateKind::UpToDate => "up-to-date".to_string(),
                ListUpdateKind::Compatible => "semver-safe-update".to_string(),
                ListUpdateKind::Incompatible => "update-possible".to_string(),
            });
        }
    }

    console_writeln!(console, "{}", &serde_json::to_string_pretty(&obj)?);
    Ok(())
}

// ============================================================================
// Installed mode
// ============================================================================

async fn execute_installed(
    args: &ShowArgs,
    working_dir: &Path,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    if installed.packages.is_empty() {
        let composer_json_path = working_dir.join("composer.json");
        if composer_json_path.exists() {
            let root = mozart_core::package::read_from_file(&composer_json_path)?;
            if !root.require.is_empty() || !root.require_dev.is_empty() {
                console_writeln_error!(
                    console,
                    "<warning>No dependencies installed. Try running mozart install or update.</warning>",
                );
            }
        }
        return Ok(());
    }

    // --path with a specific package name: show path and exit
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
                console_writeln!(console, "{} {}", p.name, path_str);
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

    let direct_names = compute_direct_names(working_dir, args.no_dev)?;

    // Filter packages (--no-dev, --direct)
    let mut packages = filter_installed_packages(&installed, args, &direct_names);

    // Apply wildcard or exact package filter
    if let Some(ref package_filter) = args.package {
        if package_filter.contains('*') {
            packages.retain(|p| matches_wildcard(&p.name, package_filter));
        } else {
            // Single package detail view
            let pkg = installed
                .packages
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(package_filter));
            let pkg = match pkg {
                Some(p) => p,
                None => {
                    anyhow::bail!(
                        "Package \"{}\" not found, try using --available (-a) to show all available packages",
                        package_filter
                    );
                }
            };
            let detail = installed_to_detail(pkg, &vendor_dir);
            return print_package_detail(&detail, args, repo_cache, console).await;
        }
    }

    // --path list mode
    if args.path {
        for pkg in &packages {
            let install_path = vendor_dir.join(&pkg.name);
            let path_str = resolve_path(&install_path);
            console_writeln!(console, "{} {}", pkg.name, path_str);
        }
        return Ok(());
    }

    // --name-only
    let show_latest = args.latest || args.outdated;
    if args.name_only && !show_latest {
        for pkg in &packages {
            console_writeln!(console, "{}", &pkg.name);
        }
        return Ok(());
    }

    if packages.is_empty() {
        return Ok(());
    }

    let mut entries = collect_installed_entries(&packages, args, &direct_names, repo_cache).await;

    if args.name_only {
        for e in &entries {
            console_writeln!(console, "{}", &e.name);
        }
        return Ok(());
    }

    // A10: --strict exit code
    let has_outdated = render_package_list(&mut entries, args, "installed", console)?;
    if args.strict && has_outdated {
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    Ok(())
}

fn filter_installed_packages<'a>(
    installed: &'a mozart_registry::installed::InstalledPackages,
    args: &ShowArgs,
    direct_names: &IndexSet<String>,
) -> Vec<&'a mozart_registry::installed::InstalledPackageEntry> {
    let mut packages: Vec<&mozart_registry::installed::InstalledPackageEntry> =
        installed.packages.iter().collect();

    // --no-dev: exclude dev packages
    if args.no_dev {
        let dev_names: IndexSet<String> = installed
            .dev_package_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        packages.retain(|p| !dev_names.contains(&p.name.to_lowercase()));
    }

    // --direct: only show packages directly required by root
    if args.direct {
        packages.retain(|p| direct_names.contains(&p.name.to_lowercase()));
    }

    packages.sort_by_key(|a| a.name.to_lowercase());
    packages
}

// ============================================================================
// Locked mode
// ============================================================================

async fn execute_locked(
    args: &ShowArgs,
    working_dir: &Path,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "A valid composer.json and composer.lock files is required to run this command with --locked"
        );
    }

    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();

    if let Some(ref pkgs_dev) = lock.packages_dev
        && !args.no_dev
    {
        packages.extend(pkgs_dev.iter());
    }

    let direct_names = compute_direct_names(working_dir, args.no_dev)?;

    // --direct filter
    if args.direct {
        packages.retain(|p| direct_names.contains(&p.name.to_lowercase()));
    }

    packages.sort_by_key(|a| a.name.to_lowercase());

    if let Some(ref package_filter) = args.package {
        if package_filter.contains('*') {
            packages.retain(|p| matches_wildcard(&p.name, package_filter));
        } else {
            // Single package detail view
            let pkg = lock
                .packages
                .iter()
                .chain(lock.packages_dev.iter().flatten())
                .find(|p| p.name.eq_ignore_ascii_case(package_filter));
            let pkg = match pkg {
                Some(p) => p,
                None => {
                    anyhow::bail!("Package \"{}\" not found in lock file", package_filter);
                }
            };
            let detail = locked_to_detail(pkg);
            return print_package_detail(&detail, args, repo_cache, console).await;
        }
    }

    // --path list mode
    if args.path {
        console_writeln_error!(
            console,
            "<warning>--path is not supported with --locked</warning>",
        );
        return Ok(());
    }

    // --name-only
    let show_latest = args.latest || args.outdated;
    if args.name_only && !show_latest {
        for pkg in &packages {
            console_writeln!(console, "{}", &pkg.name);
        }
        return Ok(());
    }

    if packages.is_empty() {
        return Ok(());
    }

    let mut entries = collect_locked_entries(&packages, args, &direct_names, repo_cache).await;

    if args.name_only {
        for e in &entries {
            console_writeln!(console, "{}", &e.name);
        }
        return Ok(());
    }

    // A10: --strict exit code; A14: use "locked" as the JSON key
    let has_outdated = render_package_list(&mut entries, args, "locked", console)?;
    if args.strict && has_outdated {
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    Ok(())
}

// ============================================================================
// Self mode
// ============================================================================

fn show_self(
    args: &ShowArgs,
    working_dir: &Path,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }
    let root = mozart_core::package::read_from_file(&composer_json_path)?;

    if args.name_only {
        console_writeln!(console, "{}", &root.name);
        return Ok(());
    }

    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>name</info>"),
        root.name,
    );
    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>descrip.</info>"),
        root.description.as_deref().unwrap_or(""),
    );
    console_writeln!(
        console,
        "{} : {}",
        console_format!("<info>type</info>"),
        root.package_type.as_deref().unwrap_or("project"),
    );
    if let Some(ref license) = root.license {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>license</info>"),
            format_license_for_show(license),
        );
    }
    if let Some(ref homepage) = root.homepage {
        console_writeln!(
            console,
            "{} : {}",
            console_format!("<info>homepage</info>"),
            homepage,
        );
    }

    // Requires
    if !root.require.is_empty() {
        console_writeln!(console, "");
        console_writeln!(console, "<info>requires</info>");
        for (name, constraint) in &root.require {
            console_writeln!(
                console,
                "{} {}",
                name,
                console_format!("<comment>{}</comment>", constraint),
            );
        }
    }

    // Requires (dev)
    if !root.require_dev.is_empty() {
        console_writeln!(console, "");
        console_writeln!(console, "<info>requires (dev)</info>");
        for (name, constraint) in &root.require_dev {
            console_writeln!(
                console,
                "{} {}",
                name,
                console_format!("<comment>{}</comment>", constraint),
            );
        }
    }

    Ok(())
}

// ============================================================================
// Tree mode
// ============================================================================

fn show_tree(
    args: &ShowArgs,
    working_dir: &Path,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let lock_path = working_dir.join("composer.lock");
    let composer_json_path = working_dir.join("composer.json");

    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }

    let root = mozart_core::package::read_from_file(&composer_json_path)?;

    let pkg_map: IndexMap<String, &mozart_registry::lockfile::LockedPackage>;
    let lock_storage;
    if lock_path.exists() {
        lock_storage = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;
        pkg_map = lock_storage
            .packages
            .iter()
            .chain(lock_storage.packages_dev.iter().flatten())
            .map(|p| (p.name.to_lowercase(), p))
            .collect();
    } else {
        pkg_map = IndexMap::new();
    }

    let root_reqs: Vec<(String, String)> = if let Some(ref pkg_filter) = args.package {
        vec![(pkg_filter.clone(), "*".to_string())]
    } else {
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

    console_writeln!(
        console,
        "<info>{}</info> <comment>{}</comment>",
        &root.name,
        root.description.as_deref().unwrap_or(""),
    );

    let mut visited_global: IndexSet<String> = IndexSet::new();
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
            console,
        );
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_tree_node(
    pkg_name: &str,
    constraint: &str,
    pkg_map: &IndexMap<String, &mozart_registry::lockfile::LockedPackage>,
    prefix: &str,
    child_prefix: &str,
    visited: &mut IndexSet<String>,
    depth: usize,
    console: &mozart_core::console::Console,
) {
    const MAX_DEPTH: usize = 10;

    let key = pkg_name.to_lowercase();

    if let Some(pkg) = pkg_map.get(&key) {
        let description = pkg.description.as_deref().unwrap_or("");
        let version = format_version(&pkg.version);

        console_writeln!(
            console,
            "{} {} {}",
            prefix,
            console_format!("<info>{}</info> <comment>{}</comment>", pkg_name, &version),
            description,
        );

        if visited.contains(&key) || depth >= MAX_DEPTH {
            if visited.contains(&key) {
                console_writeln!(
                    console,
                    "{}    {} (circular dependency)",
                    child_prefix,
                    pkg_name,
                );
            }
            return;
        }

        visited.insert(key.clone());

        let children: Vec<(&String, &String)> = pkg.require.iter().collect();
        let child_count = children.len();
        for (ci, (child_name, child_constraint)) in children.iter().enumerate() {
            let child_key = child_name.to_lowercase();
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
                console,
            );
        }

        visited.shift_remove(&key);
    } else {
        if !is_platform_package(&key) {
            console_writeln!(
                console,
                "{} {} {} (not installed)",
                prefix,
                console_format!("<comment>{}</comment>", pkg_name),
                constraint,
            );
        }
    }
}

// ============================================================================
// Platform mode
// ============================================================================

fn show_platform(
    args: &ShowArgs,
    working_dir: &Path,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let mut platform_packages: Vec<(String, String, String)> = Vec::new();

    let php_version = mozart_core::platform::detect_php_version();

    let lock_path = working_dir.join("composer.lock");
    if lock_path.exists() {
        let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

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
                if !platform_packages.iter().any(|(n, _, _)| n == name) {
                    platform_packages.push((name.clone(), version_str, "lock-dev".to_string()));
                }
            }
        }
    }

    if let Some(ref ver) = php_version
        && !platform_packages.iter().any(|(n, _, _)| n == "php")
    {
        platform_packages.push(("php".to_string(), ver.clone(), "detected".to_string()));
    }

    let extensions = mozart_core::platform::detect_php_extensions();
    for ext in &extensions {
        let ext_name = format!("ext-{ext}");
        if !platform_packages.iter().any(|(n, _, _)| *n == ext_name) {
            platform_packages.push((ext_name, "*".to_string(), "detected".to_string()));
        }
    }

    platform_packages.sort_by(|a, b| a.0.cmp(&b.0));

    if args.format == "json" {
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
        console_writeln!(
            console,
            "{}",
            &serde_json::to_string_pretty(&serde_json::json!({ "platform": json_entries }))?,
        );
        return Ok(());
    }

    if platform_packages.is_empty() {
        console.info(
            "No platform packages detected. Install PHP or add platform requirements to composer.json.",
        );
        return Ok(());
    }

    if args.name_only {
        for (name, _, _) in &platform_packages {
            console_writeln!(console, "{}", name);
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
        console_writeln!(
            console,
            "{} {}",
            console_format!("<info>{:<width$}</info>", name, width = name_width),
            console_format!(
                "<comment>{:<width$}</comment>",
                version,
                width = version_width
            ),
        );
    }

    Ok(())
}

// ============================================================================
// Available mode
// ============================================================================

async fn show_available(
    args: &ShowArgs,
    working_dir: &Path,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    if let Some(ref pkg_name) = args.package {
        return show_available_versions(pkg_name, repo_cache, args, console).await;
    }

    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir);

    let installed = match installed {
        Ok(i) if !i.packages.is_empty() => i,
        _ => {
            let lock_path = working_dir.join("composer.lock");
            if lock_path.exists() {
                let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;
                console_writeln!(
                    console,
                    "<info>Available versions for locked packages (from Packagist):</info>",
                );
                console_writeln!(console, "");

                let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
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
                    show_available_versions_inline(&pkg.name, repo_cache, console).await;
                }
                return Ok(());
            }

            console_writeln_error!(
                console,
                "<warning>No dependencies installed. Try running mozart install or update.</warning>",
            );
            return Ok(());
        }
    };

    console_writeln!(
        console,
        "<info>Available versions for installed packages (from Packagist):</info>",
    );
    console_writeln!(console, "");

    if args.format == "json" {
        let mut json_entries: Vec<serde_json::Value> = Vec::new();
        for pkg in &installed.packages {
            if is_platform_package(&pkg.name) {
                continue;
            }
            match mozart_registry::packagist::fetch_package_versions(&pkg.name, repo_cache).await {
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
        console_writeln!(console, "{}", &serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    for pkg in &installed.packages {
        if is_platform_package(&pkg.name) {
            continue;
        }
        show_available_versions_inline(&pkg.name, repo_cache, console).await;
    }

    Ok(())
}

async fn show_available_versions(
    pkg_name: &str,
    repo_cache: &mozart_registry::cache::Cache,
    args: &ShowArgs,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let versions = mozart_registry::packagist::fetch_package_versions(pkg_name, repo_cache).await?;
    if versions.is_empty() {
        console_writeln!(console, "No versions found for {pkg_name}");
        return Ok(());
    }

    if args.format == "json" {
        let version_strings: Vec<String> = versions.iter().map(|v| v.version.clone()).collect();
        let output = serde_json::json!({
            "name": pkg_name,
            "versions": version_strings,
        });
        console_writeln!(console, "{}", &serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    console_writeln!(console, "<info>Available versions for {pkg_name}:</info>");
    for v in &versions {
        console_writeln!(
            console,
            "  {}",
            console_format!("<comment>{}</comment>", &v.version),
        );
    }
    Ok(())
}

async fn show_available_versions_inline(
    pkg_name: &str,
    repo_cache: &mozart_registry::cache::Cache,
    console: &mozart_core::console::Console,
) {
    match mozart_registry::packagist::fetch_package_versions(pkg_name, repo_cache).await {
        Ok(versions) => {
            if versions.is_empty() {
                console_writeln!(
                    console,
                    "{}: no versions found",
                    console_format!("<info>{}</info>", pkg_name),
                );
                return;
            }
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
            console_writeln!(
                console,
                "{}: {}{}",
                console_format!("<info>{}</info>", pkg_name),
                console_format!("<comment>{}</comment>", &shown.join(", ")),
                rest,
            );
        }
        Err(_) => {
            console_writeln!(
                console,
                "{}: (could not fetch from Packagist)",
                console_format!("<comment>{}</comment>", pkg_name),
            );
        }
    }
}

// ============================================================================
// String / field extraction helpers
// ============================================================================

fn format_version(version: &str) -> String {
    version.strip_prefix('v').unwrap_or(version).to_string()
}

fn format_version_highlight(version: &str) -> String {
    format!("* {}", format_version(version))
}

fn get_installed_description(pkg: &mozart_registry::installed::InstalledPackageEntry) -> String {
    pkg.extra_fields
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn get_installed_keywords_vec(
    pkg: &mozart_registry::installed::InstalledPackageEntry,
) -> Vec<String> {
    pkg.extra_fields
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn get_installed_licenses(pkg: &mozart_registry::installed::InstalledPackageEntry) -> Vec<String> {
    pkg.extra_fields
        .get("license")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn get_installed_homepage(
    pkg: &mozart_registry::installed::InstalledPackageEntry,
) -> Option<String> {
    pkg.extra_fields
        .get("homepage")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_installed_release_date(
    pkg: &mozart_registry::installed::InstalledPackageEntry,
) -> Option<String> {
    pkg.extra_fields
        .get("time")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract a map of `{name: constraint}` from an installed package's
/// extra_fields for the given key (e.g. "require", "conflict", "provide").
fn get_installed_link_map(
    pkg: &mozart_registry::installed::InstalledPackageEntry,
    key: &str,
) -> BTreeMap<String, String> {
    pkg.extra_fields
        .get(key)
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract a map of `{package: reason}` from an installed package's suggest field.
fn get_installed_suggest_map(
    pkg: &mozart_registry::installed::InstalledPackageEntry,
) -> BTreeMap<String, String> {
    pkg.extra_fields
        .get("suggest")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Format a single license identifier for the `show` text output. Mirrors
/// Composer's `Command\ShowCommand::printLicenses()`:
///   * unknown id → just the id
///   * OSI-approved → `<full name> (<id>) (OSI approved) <url>`
///   * otherwise   → `<full name> (<id>) <url>`
fn format_license_for_show(license_id: &str) -> String {
    match mozart_spdx_licenses::spdx().get_license_by_identifier(license_id) {
        None => license_id.to_string(),
        Some(info) if info.osi_approved => format!(
            "{} ({}) (OSI approved) {}",
            info.full_name,
            license_id,
            info.url(),
        ),
        Some(info) => format!("{} ({}) {}", info.full_name, license_id, info.url()),
    }
}

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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_license_for_show_osi_approved() {
        let out = format_license_for_show("MIT");
        assert!(
            out.contains("MIT License") && out.contains("(MIT)") && out.contains("(OSI approved)"),
            "got: {out}",
        );
        assert!(
            out.contains("https://spdx.org/licenses/MIT.html#licenseText"),
            "got: {out}",
        );
    }

    #[test]
    fn test_format_license_for_show_non_osi() {
        let out = format_license_for_show("CC-BY-4.0");
        assert!(
            out.contains("(CC-BY-4.0)") && !out.contains("(OSI approved)"),
            "got: {out}",
        );
        assert!(
            out.contains("https://spdx.org/licenses/CC-BY-4.0.html#licenseText"),
            "got: {out}",
        );
    }

    #[test]
    fn test_format_license_for_show_unknown_falls_back_to_id() {
        assert_eq!(format_license_for_show("not-a-license"), "not-a-license");
    }

    #[test]
    fn test_format_license_for_show_url_uses_canonical_id_casing() {
        let out = format_license_for_show("mit");
        assert!(
            out.contains("https://spdx.org/licenses/MIT.html#licenseText"),
            "got: {out}",
        );
    }

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
        assert!(!matches_wildcard("psr/log", "psr/l"));
    }

    #[test]
    fn test_format_version_highlight() {
        assert_eq!(format_version_highlight("v3.0.0"), "* 3.0.0");
        assert_eq!(format_version_highlight("3.0.0"), "* 3.0.0");
    }

    #[test]
    fn test_get_installed_description_present() {
        use std::collections::BTreeMap;
        let mut extra = BTreeMap::new();
        extra.insert(
            "description".to_string(),
            serde_json::Value::String("A logging library".to_string()),
        );
        let pkg = mozart_registry::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: extra,
        };
        assert_eq!(get_installed_description(&pkg), "A logging library");
    }

    #[test]
    fn test_get_installed_description_absent() {
        use std::collections::BTreeMap;
        let pkg = mozart_registry::installed::InstalledPackageEntry {
            name: "psr/log".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        };
        assert_eq!(get_installed_description(&pkg), "");
    }

    #[test]
    fn test_get_installed_keywords() {
        use std::collections::BTreeMap;
        let mut extra = BTreeMap::new();
        extra.insert(
            "keywords".to_string(),
            serde_json::json!(["log", "psr3", "logging"]),
        );
        let pkg = mozart_registry::installed::InstalledPackageEntry {
            name: "psr/log".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: extra,
        };
        assert_eq!(
            get_installed_keywords_vec(&pkg).join(", "),
            "log, psr3, logging"
        );
    }

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

    #[test]
    fn test_extract_major_basic() {
        assert_eq!(extract_major("2.3.4.0"), 2);
        assert_eq!(extract_major("0.1.2.0"), 0);
    }

    #[test]
    fn test_extract_major_with_prerelease() {
        assert_eq!(extract_major("2.3.4.0-beta1"), 2);
    }

    #[test]
    fn test_extract_minor_basic() {
        assert_eq!(extract_minor("2.3.4.0"), 3);
        assert_eq!(extract_minor("1.0.0.0"), 0);
    }

    #[test]
    fn test_extract_minor_with_prerelease() {
        assert_eq!(extract_minor("2.3.4.0-rc1"), 3);
    }

    #[test]
    fn test_abandoned_info_bool_true() {
        let val = serde_json::Value::Bool(true);
        assert_eq!(abandoned_info(&val), Some(String::new()));
    }

    #[test]
    fn test_abandoned_info_string_replacement() {
        let val = serde_json::Value::String("other/package".to_string());
        assert_eq!(abandoned_info(&val), Some("other/package".to_string()));
    }

    #[test]
    fn test_abandoned_info_false() {
        let val = serde_json::Value::Bool(false);
        assert_eq!(abandoned_info(&val), None);
    }

    #[test]
    fn test_abandoned_info_string_false() {
        let val = serde_json::Value::String("false".to_string());
        assert_eq!(abandoned_info(&val), None);
    }
}
