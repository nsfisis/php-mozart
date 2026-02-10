use clap::Args;

#[derive(Args)]
pub struct CheckPlatformReqsArgs {
    /// Disables checking of require-dev packages requirements
    #[arg(long)]
    pub no_dev: bool,

    /// Check packages from the lock file
    #[arg(long)]
    pub lock: bool,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

pub fn execute(_args: &CheckPlatformReqsArgs) {
    todo!()
}
