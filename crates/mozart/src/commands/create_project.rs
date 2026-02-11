use clap::Args;

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
    #[arg(long)]
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
    #[arg(long)]
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

pub fn execute(_args: &CreateProjectArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
