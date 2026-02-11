use clap::Args;

#[derive(Args)]
pub struct DependsArgs {
    /// Package to inspect
    pub package: String,

    /// Recursively resolve up to the root package
    #[arg(short, long)]
    pub recursive: bool,

    /// Prints the results as a nested tree
    #[arg(short, long)]
    pub tree: bool,

    /// Read dependency information from the lock file
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(_args: &DependsArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
