use clap::Args;
use mozart_core::console;
use mozart_core::package::{self, Stability};
use mozart_core::validation;
use mozart_registry::downloader;
use mozart_registry::lockfile;
use mozart_registry::packagist;
use mozart_registry::resolver::{self, PlatformConfig, ResolveRequest};
use mozart_registry::version;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct CreateProjectArgs {
    /// Package name to install
    pub package: Option<String>,

    /// Directory to create the project in
    pub directory: Option<String>,

    /// Version constraint
    pub version: Option<String>,

    /// Minimum stability (stable, RC, beta, alpha, dev)
    #[arg(short, long)]
    pub stability: Option<String>,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long)]
    pub prefer_install: Option<String>,

    /// Add a custom repository to discover the package
    #[arg(long)]
    pub repository: Vec<String>,

    /// [Deprecated] Use --repository instead
    #[arg(long)]
    pub repository_url: Option<String>,

    /// Add the repository to the composer.json
    #[arg(long)]
    pub add_repository: bool,

    /// Install require-dev packages
    #[arg(long)]
    pub dev: bool,

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// [Deprecated] Use --no-plugins instead
    #[arg(long)]
    pub no_custom_installers: bool,

    /// Skips execution of scripts defined in composer.json
    #[arg(long)]
    pub no_scripts: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Disable HTTPS and allow HTTP
    #[arg(long)]
    pub no_secure_http: bool,

    /// Keep the VCS metadata
    #[arg(long)]
    pub keep_vcs: bool,

    /// Force removal of the VCS metadata
    #[arg(long)]
    pub remove_vcs: bool,

    /// Skip the install step after project creation
    #[arg(long)]
    pub no_install: bool,

    /// Skip the audit step after installation
    #[arg(long)]
    pub no_audit: bool,

    /// Audit output format
    #[arg(long)]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Interactive package resolution
    #[arg(long)]
    pub ask: bool,
}

/// VCS metadata directories to remove.
const VCS_DIRS: &[&str] = &[
    ".git",
    ".svn",
    "_svn",
    "CVS",
    "_darcs",
    ".arch-params",
    ".monotone",
    ".bzr",
    ".hg",
    ".fslckout",
    "_FOSSIL_",
];

/// Derive the target directory from a package name (the part after `/`).
fn dir_from_package_name(package_name: &str) -> &str {
    if let Some(slash) = package_name.rfind('/') {
        &package_name[slash + 1..]
    } else {
        package_name
    }
}

/// Remove VCS metadata directories from the target directory.
fn remove_vcs_metadata(target_dir: &Path) -> anyhow::Result<()> {
    for vcs_dir in VCS_DIRS {
        let path = target_dir.join(vcs_dir);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
            eprintln!(
                "{}",
                console::comment(&format!("Removed VCS metadata directory: {vcs_dir}"))
            );
        }
    }
    Ok(())
}

/// Replace "self.version" constraints in a composer.json with a concrete version string.
fn replace_self_version(raw: &mut package::RawPackageData, concrete_version: &str) {
    for value in raw.require.values_mut() {
        if value == "self.version" {
            *value = concrete_version.to_string();
        }
    }
    for value in raw.require_dev.values_mut() {
        if value == "self.version" {
            *value = concrete_version.to_string();
        }
    }
}

/// Check if a directory is non-empty (has any contents).
fn is_dir_non_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

pub async fn execute(
    args: &CreateProjectArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // --- Handle deprecated / no-op flags ---
    if args.prefer_source {
        console.info(&format!(
            "{}",
            console::warning("Source installs not yet supported, falling back to dist.")
        ));
    }

    if args.dev {
        console.info(&format!(
            "{}",
            console::warning(
                "The --dev flag is deprecated. Dev packages are installed by default."
            )
        ));
    }

    if args.no_custom_installers {
        console.info(&format!(
            "{}",
            console::warning(
                "The --no-custom-installers flag is deprecated. Use --no-plugins instead."
            )
        ));
    }

    if !args.repository.is_empty() || args.repository_url.is_some() || args.add_repository {
        console.info(&format!(
            "{}",
            console::warning(
                "Custom repository options (--repository, --repository-url, --add-repository) \
                 are not yet supported and will be ignored."
            )
        ));
    }

    // --- Step 1: Parse package argument ---
    let package_arg = match &args.package {
        Some(p) => p.clone(),
        None => anyhow::bail!("Not enough arguments (missing: \"package\")."),
    };

    // Split on `:` or `=` to extract name and optional version from arg
    let (package_name, version_from_arg) = match validation::parse_require_string(&package_arg) {
        Ok((name, ver)) => (name.to_lowercase(), Some(ver)),
        Err(_) => (package_arg.trim().to_lowercase(), None),
    };

    // Validate the package name
    if !validation::validate_package_name(&package_name) {
        anyhow::bail!("Invalid package name: \"{package_name}\"");
    }

    // Determine version: from arg string, then from --version flag
    let version_constraint: Option<String> = version_from_arg.or_else(|| args.version.clone());

    // --- Step 2: Determine target directory ---
    let working_dir = super::install::resolve_working_dir(cli);

    let target_dir: PathBuf = {
        let dir_name = args
            .directory
            .as_deref()
            .unwrap_or_else(|| dir_from_package_name(&package_name));
        let p = PathBuf::from(dir_name);
        if p.is_absolute() {
            p
        } else {
            working_dir.join(p)
        }
    };

    // Validate target directory
    if target_dir.is_file() {
        anyhow::bail!(
            "Target directory \"{}\" exists as a file.",
            target_dir.display()
        );
    }
    if target_dir.is_dir() && is_dir_non_empty(&target_dir) {
        anyhow::bail!(
            "Target directory \"{}\" is not empty.",
            target_dir.display()
        );
    }

    // --- Step 3: Determine minimum stability ---
    let minimum_stability: Stability = if let Some(ref s) = args.stability {
        Stability::parse(s)
    } else if let Some(ref v) = version_constraint {
        // Infer from version string
        version::stability_of(v)
    } else {
        Stability::Stable
    };

    // --- Step 4: Fetch package versions and find best match ---
    console.info(&format!(
        "{}",
        console::info(&format!("Creating project from package {package_name}"))
    ));
    console.info("Loading composer repositories with package information");

    let versions = packagist::fetch_package_versions(&package_name, None).await?;

    // Find the best candidate matching the version constraint and stability
    let best = if let Some(ref constraint) = version_constraint {
        // Filter versions matching the constraint
        versions
            .iter()
            .filter(|v| version::stability_of(&v.version_normalized) <= minimum_stability)
            .filter(|v| {
                // Simple version matching: check if version satisfies constraint
                version_matches_constraint(&v.version, &v.version_normalized, constraint)
            })
            .max_by(|a, b| {
                version::compare_normalized_versions(&a.version_normalized, &b.version_normalized)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find package \"{package_name}\" with constraint \"{constraint}\" \
                     matching your minimum-stability ({minimum_stability:?})."
                )
            })?
    } else {
        version::find_best_candidate(&versions, minimum_stability).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not find a version of package \"{package_name}\" matching your \
                 minimum-stability ({minimum_stability:?})."
            )
        })?
    };

    let concrete_version = best.version.clone();

    console.info(&format!(
        "{}",
        console::info(&format!("Installing {package_name} ({concrete_version})"))
    ));

    // --- Step 5: Create target directory and download+extract ---
    std::fs::create_dir_all(&target_dir)?;

    let dist = best.dist.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Package {package_name} ({concrete_version}) has no dist information — \
             source installs are not yet supported."
        )
    })?;

    let mut progress = downloader::DownloadProgress::new(
        !args.no_progress,
        format!("{package_name} ({concrete_version})"),
    );

    let bytes =
        downloader::download_dist(&dist.url, dist.shasum.as_deref(), Some(&mut progress), None)
            .await?;

    progress.finish();

    match dist.dist_type.as_str() {
        "zip" => downloader::extract_zip(&bytes, &target_dir)?,
        "tar" | "tar.gz" | "tgz" => downloader::extract_tar_gz(&bytes, &target_dir)?,
        other => anyhow::bail!("Unsupported dist type: {other}"),
    }

    console.info(&format!(
        "{}",
        console::info(&format!("Created project in {}", target_dir.display()))
    ));

    // --- Step 7: VCS removal ---
    // Remove VCS metadata unless --keep-vcs is set.
    // If --remove-vcs is set, always remove. If --keep-vcs is set, always keep.
    // Default (neither flag): remove.
    if args.remove_vcs || !args.keep_vcs {
        remove_vcs_metadata(&target_dir)?;
    }

    // --- Step 6: Read composer.json and optionally install dependencies ---
    let composer_path = target_dir.join("composer.json");

    if !composer_path.exists() {
        console.info(&format!(
            "{}",
            console::warning(&format!(
                "No composer.json found in {}. Skipping dependency installation.",
                target_dir.display()
            ))
        ));
        return Ok(());
    }

    let mut raw = package::read_from_file(&composer_path)?;

    // --- Step 8: Replace self.version constraints ---
    replace_self_version(&mut raw, &concrete_version);
    package::write_to_file(&raw, &composer_path)?;

    // --- Step 6 continued: dependency resolution and install ---
    if args.no_install {
        console.info(&format!(
            "{}",
            console::comment("Skipping dependency installation (--no-install).")
        ));
        return Ok(());
    }

    let dev_mode = !args.no_dev;

    let require: Vec<(String, String)> = raw
        .require
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let require_dev: Vec<(String, String)> = raw
        .require_dev
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let proj_minimum_stability_str = raw.minimum_stability.as_deref().unwrap_or("stable");
    let proj_minimum_stability = Stability::parse(proj_minimum_stability_str);

    let composer_prefer_stable = raw
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let request = ResolveRequest {
        root_name: raw.name.clone(),
        require,
        require_dev,
        include_dev: dev_mode,
        minimum_stability: proj_minimum_stability,
        stability_flags: HashMap::new(),
        prefer_stable: composer_prefer_stable,
        prefer_lowest: false,
        platform: PlatformConfig::new(),
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repo_cache: None,
    };

    console.info("Resolving dependencies...");

    let resolved = resolver::resolve(&request).await.map_err(|e| {
        mozart_core::exit_code::bail(
            mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
            e.to_string(),
        )
    })?;

    let composer_json_content = std::fs::read_to_string(&composer_path)?;

    let new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content: composer_json_content.clone(),
        composer_json: raw.clone(),
        include_dev: dev_mode,
        repo_cache: None,
    })
    .await?;

    // Print change report (all will be installs for a new project)
    let changes = super::update::compute_update_changes(None, &new_lock, dev_mode);

    let installs: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, super::update::ChangeKind::Install { .. }))
        .collect();

    console.info(&format!(
        "{}",
        console::info(&format!(
            "Package operations: {} install{}, 0 updates, 0 removals",
            installs.len(),
            if installs.len() == 1 { "" } else { "s" },
        ))
    ));

    for change in &changes {
        if let super::update::ChangeKind::Install { new_version } = &change.kind {
            console.info(&format!("  - Installing {} ({})", change.name, new_version));
        }
    }

    console.info("Writing lock file");
    let lock_path = target_dir.join("composer.lock");
    new_lock.write_to_file(&lock_path)?;

    let vendor_dir = target_dir.join("vendor");

    // Warn about prefer-source
    let prefer_source = args.prefer_source
        || args
            .prefer_install
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("source"))
            .unwrap_or(false);
    if prefer_source {
        console.info(&format!(
            "{}",
            console::warning("Source installs are not yet supported. Falling back to dist.")
        ));
    }

    super::install::install_from_lock(
        &new_lock,
        &target_dir,
        &vendor_dir,
        &super::install::InstallConfig {
            dev_mode,
            dry_run: false,
            no_autoloader: false,
            no_progress: args.no_progress,
            ignore_platform_reqs: args.ignore_platform_reqs,
            ignore_platform_req: args.ignore_platform_req.clone(),
            optimize_autoloader: false,
            classmap_authoritative: false,
            apcu_autoloader: false,
            apcu_autoloader_prefix: None,
        },
    )
    .await?;

    Ok(())
}

/// Check if a version satisfies a simple version constraint.
///
/// Supports:
/// - Exact: "1.2.3", "v1.2.3"
/// - Caret: "^1.2.3"
/// - Tilde: "~1.2"
/// - Wildcard: "1.2.*"
/// - Comparison: ">=1.0", ">1.0", "<=2.0", "<2.0", "!=1.0"
/// - Stability flags: "^1.0@beta"
/// - Dev branches: "dev-master"
///
/// Falls back to returning `true` for unrecognized constraints to avoid
/// incorrectly filtering packages.
fn version_matches_constraint(version: &str, version_normalized: &str, constraint: &str) -> bool {
    // Strip stability flag from constraint (e.g. "^1.0@beta" → "^1.0")
    let constraint = if let Some(pos) = constraint.find('@') {
        &constraint[..pos]
    } else {
        constraint
    };

    let constraint = constraint.trim();

    // Handle dev-branch constraints
    if constraint.starts_with("dev-") {
        return version == constraint || version_normalized == constraint;
    }

    // Handle wildcard constraints like "1.2.*"
    if constraint.contains('*') {
        let prefix = constraint.trim_end_matches('*').trim_end_matches('.');
        return version.starts_with(prefix) || version_normalized.starts_with(prefix);
    }

    // Handle comparison operators
    for op in &[">=", "<=", "!=", ">", "<"] {
        if let Some(rest) = constraint.strip_prefix(op) {
            let rest = rest.trim().trim_start_matches('v');
            let cmp = version::compare_normalized_versions(version_normalized, rest);
            return match *op {
                ">=" => cmp != std::cmp::Ordering::Less,
                "<=" => cmp != std::cmp::Ordering::Greater,
                "!=" => cmp != std::cmp::Ordering::Equal,
                ">" => cmp == std::cmp::Ordering::Greater,
                "<" => cmp == std::cmp::Ordering::Less,
                _ => true,
            };
        }
    }

    // Handle caret constraint "^1.2.3"
    if let Some(rest) = constraint.strip_prefix('^') {
        let rest = rest.trim().trim_start_matches('v');
        return caret_matches(version_normalized, rest);
    }

    // Handle tilde constraint "~1.2.3"
    if let Some(rest) = constraint.strip_prefix('~') {
        let rest = rest.trim().trim_start_matches('v');
        return tilde_matches(version_normalized, rest);
    }

    // Exact match (possibly with "v" prefix)
    let clean_constraint = constraint.trim_start_matches('v');
    version == constraint
        || version == clean_constraint
        || version_normalized.starts_with(clean_constraint)
}

/// Check if a normalized version satisfies a caret constraint `^MAJOR.MINOR.PATCH`.
///
/// Rules:
/// - If MAJOR > 0: any version in `[MAJOR.MINOR.PATCH, (MAJOR+1).0.0.0)`
/// - If MAJOR == 0 and MINOR > 0: any version in `[0.MINOR.PATCH, 0.(MINOR+1).0.0)`
/// - If MAJOR == 0 and MINOR == 0: any version in `[0.0.PATCH, 0.0.(PATCH+1))`
fn caret_matches(version_normalized: &str, constraint_base: &str) -> bool {
    // Strip pre-release suffix from version for numeric comparison
    let v_base = if let Some(pos) = version_normalized.find('-') {
        &version_normalized[..pos]
    } else {
        version_normalized
    };

    let parse_parts =
        |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };

    let v_parts = parse_parts(v_base);
    let c_parts = parse_parts(constraint_base);

    let v_major = v_parts.first().copied().unwrap_or(0);
    let v_minor = v_parts.get(1).copied().unwrap_or(0);
    let v_patch = v_parts.get(2).copied().unwrap_or(0);
    let v_build = v_parts.get(3).copied().unwrap_or(0);

    let c_major = c_parts.first().copied().unwrap_or(0);
    let c_minor = c_parts.get(1).copied().unwrap_or(0);
    let c_patch = c_parts.get(2).copied().unwrap_or(0);
    let c_build = c_parts.get(3).copied().unwrap_or(0);

    // Must be >= constraint version
    let ge = (v_major, v_minor, v_patch, v_build) >= (c_major, c_minor, c_patch, c_build);

    // Upper bound depends on first non-zero segment
    let lt = if c_major > 0 {
        v_major < c_major + 1
    } else if c_minor > 0 {
        v_major == 0 && v_minor < c_minor + 1
    } else {
        v_major == 0 && v_minor == 0 && v_patch < c_patch + 1
    };

    ge && lt
}

/// Check if a normalized version satisfies a tilde constraint `~MAJOR.MINOR`.
///
/// Rules:
/// - `~1.2` means `>=1.2.0 <2.0.0`
/// - `~1.2.3` means `>=1.2.3 <1.3.0`
fn tilde_matches(version_normalized: &str, constraint_base: &str) -> bool {
    let v_base = if let Some(pos) = version_normalized.find('-') {
        &version_normalized[..pos]
    } else {
        version_normalized
    };

    let parse_parts =
        |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };

    let v_parts = parse_parts(v_base);
    let c_parts = parse_parts(constraint_base);

    let v_major = v_parts.first().copied().unwrap_or(0);
    let v_minor = v_parts.get(1).copied().unwrap_or(0);
    let v_patch = v_parts.get(2).copied().unwrap_or(0);

    let c_major = c_parts.first().copied().unwrap_or(0);
    let c_minor = c_parts.get(1).copied().unwrap_or(0);
    let c_patch = c_parts.get(2).copied().unwrap_or(0);

    let ge = if c_parts.len() >= 3 {
        (v_major, v_minor, v_patch) >= (c_major, c_minor, c_patch)
    } else {
        (v_major, v_minor) >= (c_major, c_minor)
    };

    let lt = if c_parts.len() >= 3 {
        // ~1.2.3 → <1.3.0
        v_major == c_major && v_minor < c_minor + 1
    } else {
        // ~1.2 → <2.0
        v_major < c_major + 1
    };

    ge && lt
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────────────
    // dir_from_package_name tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_directory_from_package_name() {
        assert_eq!(dir_from_package_name("vendor/package"), "package");
        assert_eq!(dir_from_package_name("monolog/monolog"), "monolog");
        assert_eq!(dir_from_package_name("symfony/console"), "console");
        // No slash: use entire string
        assert_eq!(dir_from_package_name("novendor"), "novendor");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Target directory validation tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_non_empty_directory_rejected() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file inside so the dir is non-empty
        std::fs::write(dir.path().join("some-file.txt"), b"content").unwrap();

        assert!(
            is_dir_non_empty(dir.path()),
            "Directory with a file should be detected as non-empty"
        );
    }

    #[test]
    fn test_empty_directory_accepted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !is_dir_non_empty(dir.path()),
            "Empty directory should not be detected as non-empty"
        );
    }

    #[test]
    fn test_existing_file_as_directory_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("myfile");
        std::fs::write(&file_path, b"data").unwrap();

        // Verify that is_file() returns true (so the execute() function would bail)
        assert!(
            file_path.is_file(),
            "A created file should be detected as a file, not a directory"
        );
        assert!(
            !file_path.is_dir(),
            "A regular file should not be detected as a directory"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // self.version replacement tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_self_version_replacement() {
        let mut raw = package::RawPackageData::new("vendor/pkg".to_string());
        raw.require
            .insert("vendor/dep-a".to_string(), "self.version".to_string());
        raw.require
            .insert("vendor/dep-b".to_string(), "^1.0".to_string());
        raw.require_dev
            .insert("vendor/dep-c".to_string(), "self.version".to_string());

        replace_self_version(&mut raw, "2.3.4");

        assert_eq!(raw.require.get("vendor/dep-a").unwrap(), "2.3.4");
        assert_eq!(raw.require.get("vendor/dep-b").unwrap(), "^1.0");
        assert_eq!(raw.require_dev.get("vendor/dep-c").unwrap(), "2.3.4");
    }

    #[test]
    fn test_self_version_replacement_no_self_version() {
        let mut raw = package::RawPackageData::new("vendor/pkg".to_string());
        raw.require
            .insert("vendor/dep-a".to_string(), "^1.0".to_string());

        replace_self_version(&mut raw, "2.3.4");

        assert_eq!(raw.require.get("vendor/dep-a").unwrap(), "^1.0");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Version constraint matching tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_version_matches_caret() {
        assert!(version_matches_constraint("1.2.0", "1.2.0.0", "^1.0"));
        assert!(version_matches_constraint("1.9.9", "1.9.9.0", "^1.0"));
        assert!(!version_matches_constraint("2.0.0", "2.0.0.0", "^1.0"));
        assert!(!version_matches_constraint("0.9.0", "0.9.0.0", "^1.0"));
    }

    #[test]
    fn test_version_matches_exact() {
        assert!(version_matches_constraint("1.2.3", "1.2.3.0", "1.2.3"));
        assert!(!version_matches_constraint("1.2.4", "1.2.4.0", "1.2.3"));
    }

    #[test]
    fn test_version_matches_gte() {
        assert!(version_matches_constraint("1.2.0", "1.2.0.0", ">=1.0.0"));
        assert!(version_matches_constraint("2.0.0", "2.0.0.0", ">=1.0.0"));
        assert!(!version_matches_constraint("0.9.0", "0.9.0.0", ">=1.0.0"));
    }

    #[test]
    fn test_version_matches_stability_flag() {
        // "@beta" suffix in constraint should be stripped for matching
        assert!(version_matches_constraint("2.0.0", "2.0.0.0", "^2.0@beta"));
    }

    #[test]
    fn test_caret_matches() {
        // ^1.0 → >=1.0.0.0 <2.0.0.0
        assert!(caret_matches("1.0.0.0", "1.0"));
        assert!(caret_matches("1.9.9.0", "1.0"));
        assert!(!caret_matches("2.0.0.0", "1.0"));
        assert!(!caret_matches("0.9.9.0", "1.0"));

        // ^0.3 → >=0.3.0.0 <0.4.0.0
        assert!(caret_matches("0.3.0.0", "0.3"));
        assert!(caret_matches("0.3.9.0", "0.3"));
        assert!(!caret_matches("0.4.0.0", "0.3"));

        // ^0.0.3 → >=0.0.3.0 <0.0.4.0
        assert!(caret_matches("0.0.3.0", "0.0.3"));
        assert!(!caret_matches("0.0.4.0", "0.0.3"));
    }

    #[test]
    fn test_tilde_matches() {
        // ~1.2 → >=1.2 <2.0
        assert!(tilde_matches("1.2.0.0", "1.2"));
        assert!(tilde_matches("1.9.9.0", "1.2"));
        assert!(!tilde_matches("2.0.0.0", "1.2"));

        // ~1.2.3 → >=1.2.3 <1.3.0
        assert!(tilde_matches("1.2.3.0", "1.2.3"));
        assert!(tilde_matches("1.2.9.0", "1.2.3"));
        assert!(!tilde_matches("1.3.0.0", "1.2.3"));
    }
}
