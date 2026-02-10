use clap::Args;

#[derive(Args)]
pub struct RemoveArgs {
    /// Package(s) to remove
    pub packages: Vec<String>,

    /// Remove from require-dev
    #[arg(long)]
    pub dev: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Disables the automatic update of the lock file
    #[arg(long)]
    pub no_update: bool,

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

    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'w', long)]
    pub update_with_dependencies: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'W', long)]
    pub update_with_all_dependencies: bool,

    /// Update also dependencies of the removed packages
    #[arg(long)]
    pub with_all_dependencies: bool,

    /// Skip updating dependencies
    #[arg(long)]
    pub no_update_with_dependencies: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Remove unused packages
    #[arg(long)]
    pub unused: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

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
}

pub fn execute(_args: &RemoveArgs) {
    todo!()
}
