use clap::Args;

#[derive(Args)]
pub struct ProhibitsArgs {
    /// Package to inspect
    pub package: String,

    /// Version constraint
    pub version: String,

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

pub async fn execute(
    args: &ProhibitsArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    super::dependency::do_execute(
        cli,
        console,
        super::dependency::DoExecuteArgs {
            package: &args.package,
            version: Some(&args.version),
            recursive: args.recursive,
            tree: args.tree,
            locked: args.locked,
            inverted: true,
        },
    )
}
