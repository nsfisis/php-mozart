use clap::Args;

#[derive(Args)]
pub struct InstallArgs {
    /// Package(s) to install
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

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Only download packages, do not install
    #[arg(long)]
    pub download_only: bool,

    /// [Deprecated] Enables installation of require-dev packages
    #[arg(long)]
    pub dev: bool,

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Skip the install step
    #[arg(long)]
    pub no_install: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

    /// Run audit after installation
    #[arg(long)]
    pub audit: bool,

    /// Audit output format
    #[arg(long)]
    pub audit_format: Option<String>,

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
}

pub fn execute(_args: &InstallArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
