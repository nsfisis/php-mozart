use clap::Args;

#[derive(Args)]
pub struct BumpArgs {
    /// Package(s) to bump
    pub packages: Vec<String>,

    /// Only bump packages in require-dev
    #[arg(short = 'D', long)]
    pub dev_only: bool,

    /// Only bump packages in require
    #[arg(short = 'R', long)]
    pub no_dev_only: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,
}

pub fn execute(_args: &BumpArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
