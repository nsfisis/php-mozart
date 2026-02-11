use clap::Args;

#[derive(Args)]
pub struct UpdateArgs {
    /// Package(s) to update
    pub packages: Vec<String>,

    /// Temporary version constraint overrides
    #[arg(long)]
    pub with: Vec<String>,

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

    /// [Deprecated] Enables installation of require-dev packages
    #[arg(long)]
    pub dev: bool,

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Only updates the lock file hash
    #[arg(long)]
    pub lock: bool,

    /// Skip the install step
    #[arg(long)]
    pub no_install: bool,

    /// Skip the audit step
    #[arg(long)]
    pub no_audit: bool,

    /// Audit output format
    #[arg(long)]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Update also dependencies of packages in the argument list
    #[arg(short = 'w', long)]
    pub with_dependencies: bool,

    /// Update also all dependencies including root requirements
    #[arg(short = 'W', long)]
    pub with_all_dependencies: bool,

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

    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,

    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Only allow patch version updates
    #[arg(long)]
    pub patch_only: bool,

    /// Interactive package selection
    #[arg(short, long)]
    pub interactive: bool,

    /// Only update packages that are root requirements
    #[arg(long)]
    pub root_reqs: bool,

    /// Bump version constraints after update (dev, no-dev, all)
    #[arg(long)]
    pub bump_after_update: Option<Option<String>>,
}

pub fn execute(_args: &UpdateArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
