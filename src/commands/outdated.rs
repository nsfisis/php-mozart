use clap::Args;

#[derive(Args)]
pub struct OutdatedArgs {
    /// Package to inspect
    pub package: Option<String>,

    /// Show only packages that are outdated
    #[arg(short, long)]
    pub outdated: bool,

    /// Show all installed packages
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
    #[arg(short, long)]
    pub format: Option<String>,

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

pub fn execute(_args: &OutdatedArgs) {
    todo!()
}
