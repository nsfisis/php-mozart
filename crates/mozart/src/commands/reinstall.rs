use crate::composer::Composer;
use clap::Args;
use mozart_core::autoload::AutoloadGeneratorExt as _;
use mozart_core::composer::{AutoloadDumpOptions, LocalPackage};
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::validation::package_name_to_regexp;

#[derive(Args)]
pub struct ReinstallArgs {
    /// Package(s) to reinstall, can include a wildcard (*) to match any substring
    pub packages: Vec<String>,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long)]
    pub prefer_install: Option<String>,

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Optimizes PSR-0 and PSR-4 packages to be loaded with classmaps
    #[arg(short, long)]
    pub optimize_autoloader: bool,

    /// Autoload classes from the classmap only
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,

    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu_autoloader: bool,

    /// Use a custom prefix for the APCu autoloader cache
    #[arg(long)]
    pub apcu_autoloader_prefix: Option<String>,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Filter packages to reinstall by type
    #[arg(long, value_name = "TYPE")]
    pub r#type: Vec<String>,
}

pub async fn execute(
    args: &ReinstallArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let composer = Composer::require(&working_dir)?;
    let local_repo = composer.repository_manager().local_repository();

    // Selection: mirrors `ReinstallCommand::execute` lines 79-110.
    let mut packages_to_reinstall: Vec<&LocalPackage> = Vec::new();
    let mut package_names_to_reinstall: Vec<String> = Vec::new();

    if !args.r#type.is_empty() {
        if !args.packages.is_empty() {
            anyhow::bail!("You cannot specify package names and filter by type at the same time.");
        }
        let lower_types: Vec<String> = args.r#type.iter().map(|t| t.to_lowercase()).collect();
        for package in local_repo.get_canonical_packages() {
            // Composer compares against `getType()` — packages without a
            // `type` are normalised to `library` by the package loader.
            let pt = package.package_type().unwrap_or("library").to_lowercase();
            if lower_types.contains(&pt) {
                packages_to_reinstall.push(package);
                package_names_to_reinstall.push(package.pretty_name().to_string());
            }
        }
    } else {
        if args.packages.is_empty() {
            anyhow::bail!("You must pass one or more package names to be reinstalled.");
        }
        for pattern in &args.packages {
            let pattern_regexp = package_name_to_regexp(pattern);
            let mut matched = false;
            for package in local_repo.get_canonical_packages() {
                if pattern_regexp.is_match(package.pretty_name()) {
                    matched = true;
                    packages_to_reinstall.push(package);
                    package_names_to_reinstall.push(package.pretty_name().to_string());
                }
            }
            if !matched {
                io.lock().unwrap().error(&console_format!(
                    "<warning>Pattern \"{}\" does not match any currently installed packages.</warning>",
                    pattern
                ));
            }
        }
    }

    if packages_to_reinstall.is_empty() {
        io.lock().unwrap().error(&console_format!(
            "<warning>Found no packages to reinstall, aborting.</warning>"
        ));
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    // TODO(plugins): build `uninstall_operations` + `Transaction(present, result)`
    // and reverse-sort uninstalls by install order, mirroring PHP L118-143.
    // TODO(plugins): dispatch CommandEvent for `reinstall`.
    // TODO(plugins): apply prefer-source/prefer-dist via DownloadManager;
    // today the flags are accepted but not propagated.

    let dev_mode = local_repo.dev_mode().unwrap_or(true);

    // SAFETY: single-threaded at this point; no concurrent env access.
    unsafe {
        std::env::set_var("COMPOSER_DEV_MODE", if dev_mode { "1" } else { "0" });
    }

    // TODO(plugins): dispatchScript(PRE_INSTALL_CMD, dev_mode).

    // Reinstall loop. Composer delegates this to
    // `InstallationManager::execute(localRepo, ops, devMode)` twice; until
    // `mozart-registry::installer_executor` exposes the same shape, we
    // remove the install dir and re-download in place using each package's
    // recorded `dist` info.
    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let files_cache = mozart_core::repository::cache::Cache::files(&cache_config);
    let installation_manager = composer.installation_manager();

    for package in &packages_to_reinstall {
        let dist = match package.dist() {
            Some(d) => d,
            None => {
                io.lock().unwrap().info(&format!(
                    "  Warning: {} has no dist information; skipping.",
                    package.pretty_name()
                ));
                continue;
            }
        };

        io.lock().unwrap().info(&format!(
            "  - Reinstalling {} ({})",
            package.pretty_name(),
            package.pretty_version()
        ));

        if let Some(install_path) = installation_manager.get_install_path(package)
            && install_path.exists()
        {
            std::fs::remove_dir_all(&install_path)?;
        }

        let mut progress = mozart_core::repository::downloader::DownloadProgress::new(
            !args.no_progress,
            format!("{} ({})", package.pretty_name(), package.pretty_version()),
        );

        mozart_core::repository::downloader::install_package(
            &dist.url,
            &dist.kind,
            dist.shasum.as_deref(),
            installation_manager.vendor_dir(),
            package.pretty_name(),
            Some(&mut progress),
            &files_cache,
        )
        .await?;

        progress.finish();
    }

    if !args.no_autoloader {
        let optimize = args.optimize_autoloader || composer.config().optimize_autoloader;
        let class_map_authoritative =
            args.classmap_authoritative || composer.config().classmap_authoritative;
        let apcu_prefix = args.apcu_autoloader_prefix.clone();
        let apcu =
            apcu_prefix.is_some() || args.apcu_autoloader || composer.config().apcu_autoloader;

        let options = AutoloadDumpOptions {
            dev_mode: Some(dev_mode),
            class_map_authoritative,
            apcu,
            apcu_prefix,
            run_scripts: false,
            dry_run: false,
            platform_requirement_filter: super::get_platform_requirement_filter(
                args.ignore_platform_reqs,
                &args.ignore_platform_req,
            )?,
        };

        let _class_map = composer.autoload_generator().dump(
            &options,
            composer.config(),
            local_repo,
            composer.package(),
            installation_manager,
            "composer",
            optimize,
            None,
            composer.locker(),
            false,
        )?;
    }

    // TODO(plugins): dispatchScript(POST_INSTALL_CMD, dev_mode).

    Ok(())
}
