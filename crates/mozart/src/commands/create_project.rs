use clap::Args;
use indexmap::IndexMap;
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::factory::create_config;
use mozart_core::package::{self, Stability};
use mozart_core::repository::downloader;
use mozart_core::repository::lockfile;
use mozart_core::repository::packagist;
use mozart_core::repository::resolver::{self, PlatformConfig, ResolveRequest};
use mozart_core::repository::version;
use mozart_core::validation;
use std::path::{Path, PathBuf};

use crate::factory::create_download_manager;

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
    #[arg(long, value_parser = ["source", "dist", "auto"])]
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
    #[arg(long, value_parser = ["table", "plain", "json", "summary"])]
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
    ".svn",
    "_svn",
    "CVS",
    "_darcs",
    ".arch-params",
    ".monotone",
    ".bzr",
    ".git",
    ".hg",
    ".fslckout",
    "_FOSSIL_",
];

/// Allowed stability values, ordered as `BasePackage::STABILITIES` keys.
const STABILITIES: &[&str] = &["stable", "RC", "beta", "alpha", "dev"];

/// Output of `install_root_package` — the bits that `install_project` needs back.
struct InstallRootPackageResult {
    installed_from_vcs: bool,
    target_dir: PathBuf,
    concrete_version: String,
}

/// Derive the target directory from a package name (the part after `/`).
fn dir_from_package_name(package_name: &str) -> &str {
    if let Some(slash) = package_name.rfind('/') {
        &package_name[slash + 1..]
    } else {
        package_name
    }
}

/// Remove VCS metadata directories from the target directory.
fn remove_vcs_metadata(
    target_dir: &Path,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    for vcs_dir in VCS_DIRS {
        let path = target_dir.join(vcs_dir);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
            io.lock().unwrap().info(&console_format!(
                "<comment>Removed VCS metadata directory: {vcs_dir}</comment>"
            ));
        }
    }
    Ok(())
}

/// Replace "self.version" constraints in a composer.json with a concrete version string.
fn replace_self_version(raw: &mut package::RawPackageData, concrete_version: &str) {
    for map in [
        &mut raw.require,
        &mut raw.require_dev,
        &mut raw.conflict,
        &mut raw.provide,
        &mut raw.replace,
    ] {
        for value in map.values_mut() {
            if value == "self.version" {
                *value = concrete_version.to_string();
            }
        }
    }
}

/// Check if a directory is non-empty (has any contents).
fn is_dir_non_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Render a path the same way Composer's `Filesystem::findShortestPath` does for
/// the `Creating a "..." project at "..."` line: relative when `dir` is contained
/// in `from`, otherwise the absolute path.
fn shortest_path(from: &Path, dir: &Path) -> String {
    if let Ok(rel) = dir.strip_prefix(from) {
        let s = rel.display().to_string();
        if s.is_empty() { ".".to_string() } else { s }
    } else {
        dir.display().to_string()
    }
}

/// Mirror of Composer's `installProject`/`installRootPackage` stability-inference
/// branch. Returns the canonical (mixed-case) stability string and the parsed enum.
fn resolve_stability(
    stability: Option<&str>,
    package_version: Option<&str>,
) -> anyhow::Result<(String, Stability)> {
    // Composer: when --stability is unset, infer from the package version.
    let raw = if let Some(s) = stability {
        s.to_string()
    } else if let Some(v) = package_version {
        // `^[^,\s]*?@(stable|RC|beta|alpha|dev)$` — pick out a trailing
        // `@stability` flag attached to a single (no comma/whitespace) version.
        if let Some(at_pos) = v.rfind('@') {
            let (head, rest) = v.split_at(at_pos);
            let suffix = &rest[1..];
            if !head.contains(',')
                && !head.contains(char::is_whitespace)
                && STABILITIES.iter().any(|k| suffix.eq_ignore_ascii_case(k))
            {
                suffix.to_string()
            } else {
                parse_stability_from_version(v)
            }
        } else {
            parse_stability_from_version(v)
        }
    } else {
        "stable".to_string()
    };

    // Normalize to the canonical `BasePackage::STABILITIES` casing.
    let normalized = STABILITIES
        .iter()
        .find(|k| k.eq_ignore_ascii_case(&raw))
        .copied();
    let normalized = match normalized {
        Some(s) => s.to_string(),
        None => anyhow::bail!(
            "Invalid stability provided ({raw}), must be one of: {}",
            STABILITIES.join(", ")
        ),
    };

    let stability = Stability::parse(&normalized);
    Ok((normalized, stability))
}

/// Mirror of `VersionParser::parseStability` — derive a stability flag from a
/// version constraint string (e.g. `"1.0.0-beta1"` → `"beta"`).
fn parse_stability_from_version(version: &str) -> String {
    let v = version.trim();
    if v.to_lowercase().starts_with("dev-") || v.to_lowercase().ends_with("-dev") {
        return "dev".to_string();
    }
    if let Some(pos) = v.rfind('-') {
        let suffix = v[pos + 1..].to_lowercase();
        let alpha: String = suffix.chars().take_while(|c| c.is_alphabetic()).collect();
        let stab = match alpha.as_str() {
            "alpha" | "a" => "alpha",
            "beta" | "b" => "beta",
            "rc" => "RC",
            "dev" => "dev",
            _ => return "stable".to_string(),
        };
        return stab.to_string();
    }
    "stable".to_string()
}

/// Match a Packagist version against a constraint string using `mozart_semver`.
fn version_satisfies_constraint(packagist_version: &str, constraint: &str) -> bool {
    let parsed_constraint = match mozart_semver::VersionConstraint::parse(constraint) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let parsed_version = match mozart_semver::Version::parse(packagist_version) {
        Ok(v) => v,
        Err(_) => return false,
    };
    parsed_constraint.matches(&parsed_version)
}

pub async fn execute(
    args: &CreateProjectArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    // --- Deprecated / aliased flags ---
    if args.dev {
        io.lock().unwrap().write_error(&console_format!(
            "<warning>You are using the deprecated option \"dev\". Dev packages are installed by default now.</warning>"
        ));
    }
    if args.no_custom_installers {
        io.lock().unwrap().write_error(&console_format!(
            "<warning>You are using the deprecated option \"no-custom-installers\". Use \"no-plugins\" instead.</warning>"
        ));
    }

    // --- --ask interactive prompt for the project directory ---
    let directory_arg: Option<String> = if io.lock().unwrap().is_interactive() && args.ask {
        let package = args
            .package
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Not enough arguments (missing: \"package\")."))?;
        let lower = package.to_lowercase();
        let basename = dir_from_package_name(&lower).to_string();
        let answer = io.lock().unwrap().ask(
            &console_format!("New project directory [<comment>{basename}</comment>]: "),
            &basename,
        );
        Some(answer)
    } else {
        args.directory.clone()
    };

    // --- Resolve --repository / --repository-url into a single Option<Vec<String>> ---
    let repositories: Option<Vec<String>> = if !args.repository.is_empty() {
        Some(args.repository.clone())
    } else {
        args.repository_url.as_ref().map(|u| vec![u.clone()])
    };

    let install_dev_packages = !args.no_dev;
    let prefer_install_source = args
        .prefer_install
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case("source"))
        .unwrap_or(false);
    let prefer_install_dist = args
        .prefer_install
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case("dist"))
        .unwrap_or(false);
    let prefer_source = args.prefer_source || prefer_install_source;
    let prefer_dist = args.prefer_dist || prefer_install_dist;
    let secure_http = !args.no_secure_http;

    install_project(
        io,
        cli,
        args,
        args.package.as_deref(),
        directory_arg.as_deref(),
        args.version.as_deref(),
        args.stability.as_deref(),
        prefer_source,
        prefer_dist,
        install_dev_packages,
        repositories,
        cli.no_plugins,
        cli.no_scripts || args.no_scripts,
        args.no_progress,
        args.no_install,
        secure_http,
        args.add_repository,
    )
    .await?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn install_project(
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    cli: &super::Cli,
    args: &CreateProjectArgs,
    package_name: Option<&str>,
    directory: Option<&str>,
    package_version: Option<&str>,
    stability: Option<&str>,
    prefer_source: bool,
    prefer_dist: bool,
    install_dev_packages: bool,
    repositories: Option<Vec<String>>,
    disable_plugins: bool,
    disable_scripts: bool,
    no_progress: bool,
    no_install: bool,
    secure_http: bool,
    add_repository: bool,
) -> anyhow::Result<()> {
    let _ = (disable_plugins, disable_scripts, prefer_dist, secure_http);

    // Mozart does not yet support custom repositories on the create-project
    // command — warn and ignore (deferred; tracked under priority 2).
    if repositories.is_some() || add_repository {
        io.lock().unwrap().write_error(&console_format!(
            "<warning>Custom repository options (--repository, --repository-url, --add-repository) \
             are not yet supported and will be ignored.</warning>"
        ));
    }

    // --- installRootPackage: download + extract the root pkg into the target dir ---
    let root_result = if let Some(name) = package_name {
        Some(
            install_root_package(
                io.clone(),
                cli,
                args,
                name,
                directory,
                package_version,
                stability,
                prefer_source,
                prefer_dist,
                install_dev_packages,
                repositories.as_deref(),
                disable_plugins,
                disable_scripts,
                no_progress,
                secure_http,
            )
            .await?,
        )
    } else {
        None
    };

    let Some(root) = root_result else {
        // Composer falls back to `composer install` semantics when no package
        // was given; Mozart does not yet support that mode.
        anyhow::bail!("Not enough arguments (missing: \"package\").");
    };

    let target_dir = root.target_dir.clone();
    let installed_from_vcs = root.installed_from_vcs;
    let concrete_version = root.concrete_version.clone();

    // --- VCS removal ---
    // Composer asks the user when interactive (and `installed_from_vcs`); when
    // non-interactive or `--remove-vcs` is set, it removes silently. With
    // `--keep-vcs`, never remove. Mozart additionally extends "remove" to the
    // dist-archive case (where there is no installed-from-vcs flag) so that
    // .git directories shipped inside an archive get scrubbed.
    let mut vcs_removed = false;
    if !args.keep_vcs {
        let should_remove = if installed_from_vcs {
            let remove_vcs_confirmed = io.lock().unwrap().confirm(&console_format!(
                "<info>Do you want to remove the existing VCS (.git, .svn..) history?</info> [<comment>y,n</comment>]? "
            ));
            args.remove_vcs || !io.lock().unwrap().is_interactive() || remove_vcs_confirmed
        } else {
            // Default for dist installs: scrub VCS metadata that may have been
            // shipped inside the archive (matches Mozart's pre-split behaviour).
            true
        };
        if should_remove {
            remove_vcs_metadata(&target_dir, io.clone())?;
            vcs_removed = true;
        }
    }

    // --- Read composer.json from the new project ---
    let composer_path = target_dir.join("composer.json");
    if !composer_path.exists() {
        io.lock().unwrap().write_error(&console_format!(
            "<warning>No composer.json found in {}. Skipping dependency installation.</warning>",
            target_dir.display()
        ));
        return Ok(());
    }

    let mut raw = package::read_from_file(&composer_path)?;

    // --- Replace self.version constraints once VCS metadata is gone ---
    if vcs_removed {
        replace_self_version(&mut raw, &concrete_version);
        package::write_to_file(&raw, &composer_path)?;
    }

    if no_install {
        io.lock().unwrap().info(&console_format!(
            "<comment>Skipping dependency installation (--no-install).</comment>"
        ));
        return Ok(());
    }

    // --- Resolve, lock, install dependencies ---
    let dev_mode = install_dev_packages;

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

    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_core::repository::cache::Cache::repo(&cache_config);

    let request = ResolveRequest {
        root_name: raw.name.clone(),
        root_version: raw.version.clone(),
        require,
        require_dev,
        include_dev: dev_mode,
        minimum_stability: proj_minimum_stability,
        stability_flags: IndexMap::new(),
        prefer_stable: composer_prefer_stable,
        prefer_lowest: false,
        platform: PlatformConfig::new(),
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repositories: std::sync::Arc::new(
            mozart_core::repository::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        temporary_constraints: IndexMap::new(),
        raw_repositories: raw.repositories.clone(),
        root_provide: raw
            .provide
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_replace: raw
            .replace
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_conflict: raw
            .conflict
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        locked_package_names: indexmap::IndexSet::new(),
        locked_packages: Vec::new(),
        block_abandoned: false,
        root_branch_alias: None,
        preferred_versions: indexmap::IndexMap::new(),
        block_insecure: false,
    };

    io.lock().unwrap().info("Resolving dependencies...");

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
        repositories: std::sync::Arc::new(
            mozart_core::repository::repository::RepositorySet::with_packagist(repo_cache.clone()),
        ),
        previous_lock: None,
        lock_pinned_names: indexmap::IndexSet::new(),
    })
    .await?;

    let changes = super::update::compute_update_changes(None, &new_lock, dev_mode);

    let installs: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, super::update::ChangeKind::Install { .. }))
        .collect();

    io.lock().unwrap().info(&console_format!(
        "<info>Package operations: {} install{}, 0 updates, 0 removals</info>",
        installs.len(),
        if installs.len() == 1 { "" } else { "s" }
    ));

    for change in &changes {
        if let super::update::ChangeKind::Install { new_version } = &change.kind {
            io.lock()
                .unwrap()
                .info(&format!("  - Installing {} ({})", change.name, new_version));
        }
    }

    io.lock().unwrap().info("Writing lock file");
    let lock_path = target_dir.join("composer.lock");
    new_lock.write_to_file(&lock_path)?;

    let vendor_dir = target_dir.join("vendor");

    if prefer_source {
        io.lock().unwrap().write_error(&console_format!(
            "<warning>Source installs are not yet supported. Falling back to dist.</warning>"
        ));
    }

    let project_config = raw.extra_fields.get("config");
    let optimize_autoloader = project_config
        .and_then(|c| c.get("optimize-autoloader"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let classmap_authoritative = project_config
        .and_then(|c| c.get("classmap-authoritative"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let apcu_autoloader = project_config
        .and_then(|c| c.get("apcu-autoloader"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let files_cache = mozart_core::repository::cache::Cache::files(&cache_config);
    let mut executor =
        mozart_core::repository::installer_executor::FilesystemExecutor::new(files_cache);
    super::install::install_from_lock(
        &new_lock,
        &target_dir,
        &vendor_dir,
        &super::install::InstallConfig {
            dev_mode,
            dry_run: false,
            no_autoloader: false,
            no_progress,
            ignore_platform_reqs: args.ignore_platform_reqs,
            ignore_platform_req: args.ignore_platform_req.clone(),
            optimize_autoloader,
            classmap_authoritative,
            apcu_autoloader,
            apcu_autoloader_prefix: None,
            download_only: false,
            prefer_source: args.prefer_source,
        },
        io.clone(),
        &mut executor,
    )
    .await?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn install_root_package(
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    cli: &super::Cli,
    _args: &CreateProjectArgs,
    package_name: &str,
    directory: Option<&str>,
    package_version: Option<&str>,
    stability: Option<&str>,
    prefer_source: bool,
    prefer_dist: bool,
    install_dev_packages: bool,
    repositories: Option<&[String]>,
    disable_plugins: bool,
    disable_scripts: bool,
    no_progress: bool,
    secure_http: bool,
) -> anyhow::Result<InstallRootPackageResult> {
    let _ = (
        prefer_dist,
        install_dev_packages,
        repositories,
        disable_scripts,
        secure_http,
    );

    // --- Parse name + version from the package argument ---
    let (parsed_name, parsed_version) = match validation::parse_require_string(package_name) {
        Ok((n, v)) => (n.to_lowercase(), Some(v)),
        Err(_) => (package_name.trim().to_lowercase(), None),
    };
    let name = parsed_name;
    let package_version: Option<String> = package_version.map(|s| s.to_string()).or(parsed_version);

    if !validation::validate_package_name(&name) {
        anyhow::bail!("Invalid package name: \"{name}\"");
    }

    // --- Determine target directory ---
    let working_dir = cli.working_dir()?;

    let mut directory_str: String = match directory {
        Some(d) => d.to_string(),
        None => {
            let basename = dir_from_package_name(&name);
            working_dir.join(basename).display().to_string()
        }
    };
    // rtrim('/' | '\\')
    while directory_str.ends_with('/') || directory_str.ends_with('\\') {
        directory_str.pop();
    }

    let mut target_dir = PathBuf::from(&directory_str);
    if !target_dir.is_absolute() {
        target_dir = working_dir.join(&target_dir);
    }

    if directory_str.is_empty() {
        anyhow::bail!("Got an empty target directory, something went wrong");
    }

    let short = shortest_path(&working_dir, &target_dir);
    io.lock().unwrap().write_error(&console_format!(
        "<info>Creating a \"{package_name}\" project at \"{short}\"</info>"
    ));

    if target_dir.exists() {
        if !target_dir.is_dir() {
            anyhow::bail!(
                "Cannot create project directory at \"{}\", it exists as a file.",
                target_dir.display()
            );
        }
        if is_dir_non_empty(&target_dir) {
            anyhow::bail!(
                "Project directory \"{}\" is not empty.",
                target_dir.display()
            );
        }
    }

    // --- Stability inference + validation ---
    let (_, minimum_stability) = resolve_stability(stability, package_version.as_deref())?;

    // --- Find the best candidate matching constraint + stability ---
    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_core::repository::cache::Cache::repo(&cache_config);

    let versions = packagist::fetch_package_versions(&name, &repo_cache).await?;

    let best = if let Some(ref constraint) = package_version {
        versions
            .iter()
            .filter(|v| version::stability_of(&v.version_normalized) <= minimum_stability)
            .filter(|v| version_satisfies_constraint(&v.version, constraint))
            .max_by(|a, b| {
                version::compare_normalized_versions(&a.version_normalized, &b.version_normalized)
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Could not find package {name} with version {constraint}.")
            })?
    } else {
        let stability_label = match minimum_stability {
            Stability::Stable => "stable",
            Stability::RC => "RC",
            Stability::Beta => "beta",
            Stability::Alpha => "alpha",
            Stability::Dev => "dev",
        };
        version::find_best_candidate(&versions, minimum_stability).ok_or_else(|| {
            anyhow::anyhow!("Could not find package {name} with stability {stability_label}.")
        })?
    };

    let concrete_version = best.version.clone();

    // --- Print "Installing" line + plugin notice ---
    io.lock().unwrap().write_error(&console_format!(
        "<info>Installing {name} ({concrete_version})</info>"
    ));
    if disable_plugins {
        io.lock()
            .unwrap()
            .write_error(&console_format!("<info>Plugins have been disabled.</info>"));
    }

    // --- Create the target directory and download + extract the dist archive ---
    std::fs::create_dir_all(&target_dir)?;

    let dist = best.dist.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Package {name} ({concrete_version}) has no dist information — \
             source installs are not yet supported."
        )
    })?;

    let mut progress =
        downloader::DownloadProgress::new(!no_progress, format!("{name} ({concrete_version})"));

    let config = create_config()?;
    let download_manager = create_download_manager(io.clone(), &config);
    let bytes = download_manager
        .download_legacy(&dist.url, dist.shasum.as_deref(), Some(&mut progress))
        .await?;

    progress.finish();

    match dist.dist_type.as_str() {
        "zip" => downloader::extract_zip(&bytes, &target_dir)?,
        "tar" | "tar.gz" | "tgz" => downloader::extract_tar_gz(&bytes, &target_dir)?,
        other => anyhow::bail!("Unsupported dist type: {other}"),
    }

    // Composer's `installRootPackage` reports `installation_source === 'source'`;
    // Mozart only supports dist downloads today, so this is always false.
    let installed_from_vcs = false;

    io.lock().unwrap().write_error(&console_format!(
        "<info>Created project in {}</info>",
        target_dir.display()
    ));

    // Mirror Composer's `Platform::putEnv('COMPOSER_ROOT_VERSION', ...)` so that
    // any subprocesses (or in-process logic) that look up the env var see the
    // freshly installed root version.
    // SAFETY: setting an env var here races with multi-threaded readers in
    // theory, but `create-project` only runs once in process and no concurrent
    // env-mutating code exists.
    unsafe {
        std::env::set_var("COMPOSER_ROOT_VERSION", &concrete_version);
    }

    // Also clear `COMPOSER` if a composer.json exists at the new project — the
    // env var is meant for the launching project, not the freshly installed one.
    if target_dir.join("composer.json").exists() && std::env::var_os("COMPOSER").is_some() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("COMPOSER");
        }
    }

    let _ = prefer_source;

    Ok(InstallRootPackageResult {
        installed_from_vcs,
        target_dir,
        concrete_version,
    })
}
