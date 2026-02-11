use clap::Args;

#[derive(Args)]
pub struct LicensesArgs {
    /// Output format (text, json, summary)
    #[arg(short, long)]
    pub format: Option<String>,

    /// Disables listing of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// List packages from the lock file
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(_args: &LicensesArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
