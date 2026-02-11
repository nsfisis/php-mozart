use clap::Args;

#[derive(Args)]
pub struct ShowArgs {
    /// Package to inspect
    pub package: Option<String>,

    /// Version constraint
    pub version: Option<String>,

    /// List all packages
    #[arg(long)]
    pub all: bool,

    /// List packages from the lock file
    #[arg(long)]
    pub locked: bool,

    /// Show only installed packages (enabled by default)
    #[arg(short, long)]
    pub installed: bool,

    /// List platform packages only
    #[arg(short, long)]
    pub platform: bool,

    /// List available packages only
    #[arg(short = 'a', long)]
    pub available: bool,

    /// Show information about the root package
    #[arg(short, long, name = "self")]
    pub self_info: bool,

    /// Show package names only
    #[arg(short = 'N', long)]
    pub name_only: bool,

    /// Show package paths only
    #[arg(short = 'P', long)]
    pub path: bool,

    /// List the dependencies as a tree
    #[arg(short, long)]
    pub tree: bool,

    /// Show the latest version
    #[arg(short, long)]
    pub latest: bool,

    /// Show only packages that are outdated
    #[arg(short, long)]
    pub outdated: bool,

    /// Ignore specified package(s)
    #[arg(long)]
    pub ignore: Vec<String>,

    /// Only show packages that have major SemVer-compatible updates
    #[arg(short = 'M', long)]
    pub major_only: bool,

    /// Only show packages that have minor SemVer-compatible updates
    #[arg(short = 'm', long)]
    pub minor_only: bool,

    /// Only show packages that have patch SemVer-compatible updates
    #[arg(long)]
    pub patch_only: bool,

    /// Sort packages by age of the last update
    #[arg(short = 'A', long)]
    pub sort_by_age: bool,

    /// Shows only packages that are directly required by the root package
    #[arg(short = 'D', long)]
    pub direct: bool,

    /// Return a non-zero exit code when there are outdated packages
    #[arg(long)]
    pub strict: bool,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,

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

pub fn execute(_args: &ShowArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
