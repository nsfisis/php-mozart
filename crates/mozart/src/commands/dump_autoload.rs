use clap::Args;
use mozart_autoload::AutoloadGeneratorExt;
use mozart_core::composer::{AutoloadDumpOptions, Composer, PlatformRequirementFilter};
use mozart_core::{console_format, console_writeln};

#[derive(Args, Default)]
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
    let composer = Composer::require(&working_dir)?;

    let missing_dependencies = {
        let installation_manager = composer.installation_manager();
        let local_repo = composer.repository_manager().local_repository();
        let mut missing = false;
        for local_pkg in local_repo.canonical_packages() {
            if let Some(install_path) = installation_manager.get_install_path(local_pkg)
                && !install_path.exists()
            {
                missing = true;
                console_writeln!(
                    console,
                    &console_format!(
                        r#"<warning>Not all dependencies are installed. Make sure to run a "composer install" to install missing dependencies</warning>"#
                    ),
                );
                break;
            }
        }
        missing
    };

    let optimize = args.optimize || composer.config().optimize_autoloader;
    let class_map_authoritative =
        args.classmap_authoritative || composer.config().classmap_authoritative;
    let apcu_prefix = args.apcu_prefix.clone();
    let apcu = apcu_prefix.is_some() || args.apcu || composer.config().apcu_autoloader;

    let do_optimize = optimize || class_map_authoritative;
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

    console_writeln!(
        console,
        &console_format!(
            "<info>{}</info>",
            if class_map_authoritative {
                "Generating optimized autoload files (authoritative)"
            } else if optimize {
                "Generating optimized autoload files"
            } else {
                "Generating autoload files"
            }
        ),
    );

    let dev_mode = if args.dev {
        Some(true)
    } else if args.no_dev {
        Some(false)
    } else {
        None
    };
    if args.dev && args.no_dev {
        anyhow::bail!("You can not use both --no-dev and --dev as they conflict with each other.");
    }
    let options = AutoloadDumpOptions {
        dev_mode,
        class_map_authoritative,
        apcu,
        apcu_prefix,
        run_scripts: true,
        dry_run: args.dry_run,
        platform_requirement_filter: get_platform_requirement_filter(args)?,
    };

    let class_map = composer.autoload_generator().dump(
        &options,
        composer.config(),
        composer.repository_manager().local_repository(),
        composer.package(),
        composer.installation_manager(),
        "composer",
        optimize,
        None,
        composer.locker(),
        args.strict_ambiguous,
    )?;
    let number_of_classes = class_map.count();

    if class_map_authoritative {
        console_writeln!(
            console,
            &console_format!(
                "<info>Generated optimized autoload files (authoritative) containing {number_of_classes} classes</info>",
            ),
        );
    } else if optimize {
        console_writeln!(
            console,
            &console_format!(
                "<info>Generated optimized autoload files containing {number_of_classes} classes</info>",
            ),
        );
    } else {
        console_writeln!(
            console,
            &console_format!("<info>Generated autoload files</info>"),
        );
    }

    if missing_dependencies || args.strict_psr && class_map.has_psr_violations() {
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    if args.strict_ambiguous && class_map.has_ambiguous_classes(false) {
        return Err(mozart_core::exit_code::bail_silent(2));
    }

    Ok(())
}

/// Mirror of `BaseCommand::getPlatformRequirementFilter` for the
/// `dump-autoload` command. Priority:
/// 1. `--ignore-platform-reqs` → ignore every platform requirement
/// 2. `--ignore-platform-req <name>...` (non-empty) → ignore the listed
///    names (with `*` glob support)
/// 3. neither → ignore nothing
fn get_platform_requirement_filter(
    args: &DumpAutoloadArgs,
) -> anyhow::Result<PlatformRequirementFilter> {
    if args.ignore_platform_reqs {
        return Ok(PlatformRequirementFilter::ignore_all());
    }
    if !args.ignore_platform_req.is_empty() {
        return PlatformRequirementFilter::from_list(&args.ignore_platform_req);
    }
    Ok(PlatformRequirementFilter::ignore_nothing())
}
