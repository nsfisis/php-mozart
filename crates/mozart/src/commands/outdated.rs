use clap::Args;

#[derive(Args)]
pub struct OutdatedArgs {
    /// Package to inspect
    pub package: Option<String>,

    /// Show all installed packages including up-to-date ones
    #[arg(short, long)]
    pub all: bool,

    /// Show packages from the lock file
    #[arg(long)]
    pub locked: bool,

    /// Shows only packages that are directly required by the root package
    #[arg(short = 'D', long)]
    pub direct: bool,

    /// Return a non-zero exit code when there are outdated packages
    #[arg(long)]
    pub strict: bool,

    /// Only show packages that have major SemVer-compatible updates
    #[arg(short = 'M', long)]
    pub major_only: bool,

    /// Only show packages that have minor SemVer-compatible updates
    #[arg(short = 'm', long)]
    pub minor_only: bool,

    /// Only show packages that have patch SemVer-compatible updates
    #[arg(short = 'p', long)]
    pub patch_only: bool,

    /// Sort packages by age of the last update
    #[arg(short = 'A', long)]
    pub sort_by_age: bool,

    /// Output format (text, json)
    #[arg(short, long, default_value = "text")]
    pub format: String,

    /// Ignore specified package(s)
    #[arg(long)]
    pub ignore: Vec<String>,

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

/// `outdated` is a proxy command — it mirrors Composer's `OutdatedCommand::execute`
/// (see `composer/src/Composer/Command/OutdatedCommand.php` 68–126), which remaps
/// its options into a `show --latest [--outdated]` invocation. Keeping the logic
/// in one place means every behavioral aspect (rendering, JSON shape, --strict,
/// mutual-exclusion checks, ignore warnings, etc.) has a single source of truth.
pub async fn execute(
    args: &OutdatedArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let show_args = super::show::ShowArgs {
        package: args.package.clone(),
        version: None,
        all: false,
        locked: args.locked,
        installed: false,
        platform: false,
        available: false,
        self_info: false,
        name_only: false,
        path: false,
        tree: false,
        latest: true,
        // Composer: `if (!--all) $args['--outdated'] = true;`
        outdated: !args.all,
        ignore: args.ignore.clone(),
        major_only: args.major_only,
        minor_only: args.minor_only,
        patch_only: args.patch_only,
        sort_by_age: args.sort_by_age,
        direct: args.direct,
        strict: args.strict,
        format: Some(args.format.clone()),
        no_dev: args.no_dev,
        ignore_platform_req: args.ignore_platform_req.clone(),
        ignore_platform_reqs: args.ignore_platform_reqs,
    };

    super::show::execute(&show_args, cli, console).await
}
