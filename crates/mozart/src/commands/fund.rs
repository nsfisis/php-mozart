use clap::Args;

#[derive(Args)]
pub struct FundArgs {
    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

pub fn execute(_args: &FundArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
