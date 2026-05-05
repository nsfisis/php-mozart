use clap::Args;

#[derive(Args)]
pub struct DumpAutoloadArgs {
    /// Optimizes PSR-0 and PSR-4 packages to be loaded with classmaps
    #[arg(short, long)]
    pub optimize: bool,

    /// Autoload classes from the classmap only
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,

    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu: bool,

    /// Use a custom prefix for the APCu autoloader cache
    #[arg(long)]
    pub apcu_prefix: Option<String>,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Enables autoload-dev rules
    #[arg(long)]
    pub dev: bool,

    /// Disables autoload-dev rules
    #[arg(long)]
    pub no_dev: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Return a failed status code if there are PSR mapping errors
    #[arg(long)]
    pub strict_psr: bool,

    /// Return a failed status code if there are ambiguous class mappings
    #[arg(long)]
    pub strict_ambiguous: bool,
}

pub async fn execute(
    args: &DumpAutoloadArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let composer = mozart_core::composer::Composer::require(&working_dir)?;

    let composer_config = composer.config();

    let vendor_dir = working_dir.join("vendor");

    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;
    let vendor_composer_dir = vendor_dir.join("composer");
    let mut missing_dependencies = false;
    for pkg in &installed.packages {
        if let Some(rel) = &pkg.install_path {
            let install_path = vendor_composer_dir.join(rel);
            if !install_path.exists() {
                missing_dependencies = true;
                console.info(&format!(
                    "{}",
                    mozart_core::console::warning(
                        "Not all dependencies are installed. Make sure to run a \"composer install\" to install missing dependencies"
                    )
                ));
                break;
            }
        }
    }

    let optimize = args.optimize || composer_config.optimize_autoloader;
    let classmap_authoritative =
        args.classmap_authoritative || composer_config.classmap_authoritative;
    let apcu_prefix = args.apcu_prefix.clone();
    let apcu = apcu_prefix.is_some() || args.apcu || composer_config.apcu_autoloader;

    let do_optimize = optimize || classmap_authoritative;
    if args.strict_psr && !do_optimize {
        anyhow::bail!(
            "--strict-psr mode only works with optimized autoloader, use --optimize or --classmap-authoritative."
        );
    }
    if args.strict_ambiguous && !do_optimize {
        anyhow::bail!(
            "--strict-ambiguous mode only works with optimized autoloader, use --optimize or --classmap-authoritative."
        );
    }

    if classmap_authoritative {
        console.info("Generating optimized autoload files (authoritative)");
    } else if optimize {
        console.info("Generating optimized autoload files");
    } else {
        console.info("Generating autoload files");
    }

    if args.dev && args.no_dev {
        anyhow::bail!("You can not use both --no-dev and --dev as they conflict with each other.");
    }

    let suffix = mozart_autoload::autoload::determine_suffix(&working_dir, &vendor_dir)?;

    if args.dry_run {
        console.info("Dry run: would generate autoload files");
        return Ok(());
    }

    let autoload_config = mozart_autoload::autoload::AutoloadConfig {
        project_dir: working_dir,
        vendor_dir,
        dev_mode: !args.no_dev,
        suffix,
        classmap_authoritative,
        optimize,
        apcu,
        apcu_prefix,
        strict_psr: args.strict_psr,
        strict_ambiguous: args.strict_ambiguous,
        platform_check: mozart_autoload::autoload::PlatformCheckMode::Full,
        ignore_platform_reqs: args.ignore_platform_reqs,
    };

    let result = mozart_autoload::autoload::generate(&autoload_config)?;

    if classmap_authoritative {
        console.info(&format!(
            "Generated optimized autoload files (authoritative) containing {} classes",
            result.class_count
        ));
    } else if optimize {
        console.info(&format!(
            "Generated optimized autoload files containing {} classes",
            result.class_count
        ));
    } else {
        console.info("Generated autoload files");
    }

    if missing_dependencies {
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    if args.strict_ambiguous && result.has_ambiguous_classes {
        return Err(mozart_core::exit_code::bail_silent(2));
    }

    Ok(())
}
