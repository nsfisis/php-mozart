use clap::Args;

#[derive(Args)]
pub struct RequireArgs {
    /// Package(s) to require
    pub packages: Vec<String>,

    /// Add requirement to require-dev
    #[arg(long)]
    pub dev: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long)]
    pub prefer_install: Option<String>,

    /// Pin the exact version instead of a range
    #[arg(long)]
    pub fixed: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

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

    /// [Deprecated] Use --with-dependencies instead
    #[arg(short = 'w', long)]
    pub update_with_dependencies: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'W', long)]
    pub update_with_all_dependencies: bool,

    /// Update also dependencies of newly required packages
    #[arg(long)]
    pub with_dependencies: bool,

    /// Update all dependencies including root requirements
    #[arg(long)]
    pub with_all_dependencies: bool,

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

    /// Sort packages in composer.json
    #[arg(long)]
    pub sort_packages: bool,

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

pub fn execute(_args: &RequireArgs) {
    todo!()
}
