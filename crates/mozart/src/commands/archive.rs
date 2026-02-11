use clap::Args;

#[derive(Args)]
pub struct ArchiveArgs {
    /// The package name
    pub package: Option<String>,

    /// A version constraint
    pub version: Option<String>,

    /// Format of the resulting archive (tar, tar.gz, tar.bz2, zip)
    #[arg(short, long)]
    pub format: Option<String>,

    /// Write the archive to this directory
    #[arg(long)]
    pub dir: Option<String>,

    /// Write the archive with the given file name
    #[arg(long)]
    pub file: Option<String>,

    /// Ignore filters when saving archive
    #[arg(long)]
    pub ignore_filters: bool,
}

pub fn execute(_args: &ArchiveArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
