use clap::Args;
use std::collections::HashSet;
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
    // Handle unsupported options first
    if args.tree {
        eprintln!("The --tree option is not yet implemented");
        return Ok(());
    }
    if args.available {
        eprintln!("The --available option is not yet implemented");
        return Ok(());
    }
    if args.platform {
        eprintln!("The --platform option is not yet implemented");
        return Ok(());
    }
    if args.outdated {
        eprintln!("The --outdated option is not yet implemented. See `mozart outdated`.");
        return Ok(());
    }

    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // --self: show root package info (unless --installed or --locked override)
    if args.self_info && !args.installed && !args.locked {
        return show_self(args, &working_dir);
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
    if args.name_only {
        for pkg in packages {
            println!("{}", pkg.name);
        }
        return Ok(());
    }

    if packages.is_empty() {
        return Ok(());
    }

    // Calculate column widths for alignment
    let name_width = packages.iter().map(|p| p.name.len()).max().unwrap_or(0);
    let version_width = packages
        .iter()
        .map(|p| format_version(&p.version).len())
        .max()
        .unwrap_or(0);

    for pkg in packages {
        let version = format_version(&pkg.version);
        let description = get_installed_description(pkg);

        println!(
            "{} {} {}",
            crate::console::info(&format!("{:<width$}", pkg.name, width = name_width)),
            crate::console::comment(&format!("{:<width$}", version, width = version_width)),
            description
        );
    }

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
    if args.name_only {
        for pkg in packages {
            println!("{}", pkg.name);
        }
        return Ok(());
    }

    if packages.is_empty() {
        return Ok(());
    }

    let name_width = packages.iter().map(|p| p.name.len()).max().unwrap_or(0);
    let version_width = packages
        .iter()
        .map(|p| format_version(&p.version).len())
        .max()
        .unwrap_or(0);

    for pkg in packages {
        let version = format_version(&pkg.version);
        let description = pkg.description.as_deref().unwrap_or("");

        println!(
            "{} {} {}",
            crate::console::info(&format!("{:<width$}", pkg.name, width = name_width)),
            crate::console::comment(&format!("{:<width$}", version, width = version_width)),
            description
        );
    }

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
}
