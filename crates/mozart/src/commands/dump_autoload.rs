use clap::Args;
use std::path::PathBuf;

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
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let vendor_dir = working_dir.join("vendor");
    let dev_mode = !args.no_dev;

    // Determine suffix: read from existing autoload.php, or from lock file, or generate
    let suffix = mozart_autoload::autoload::determine_suffix(&working_dir, &vendor_dir)?;

    if args.dry_run {
        console.info("Dry run: would generate autoload files");
        return Ok(());
    }

    mozart_autoload::autoload::generate(&mozart_autoload::autoload::AutoloadConfig {
        project_dir: working_dir,
        vendor_dir,
        dev_mode,
        suffix,
        classmap_authoritative: args.classmap_authoritative,
        optimize: args.optimize,
        apcu: args.apcu,
        apcu_prefix: args.apcu_prefix.clone(),
        strict_psr: args.strict_psr,
        platform_check: mozart_autoload::autoload::PlatformCheckMode::Full,
        ignore_platform_reqs: args.ignore_platform_reqs,
    })?;

    console.info("Generated autoload files");

    Ok(())
}
