use clap::Args;

#[derive(Args)]
pub struct SearchArgs {
    /// Search tokens
    #[arg(required = true)]
    pub tokens: Vec<String>,

    /// Search only in name
    #[arg(short = 'N', long)]
    pub only_name: bool,

    /// Search only for vendor / organization
    #[arg(short = 'O', long)]
    pub only_vendor: bool,

    /// Filter by package type
    #[arg(short, long, value_name = "TYPE")]
    pub r#type: Option<String>,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

pub fn execute(_args: &SearchArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
