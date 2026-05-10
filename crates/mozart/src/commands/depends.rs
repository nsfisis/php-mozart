use clap::Args;
use mozart_core::console::IoInterface;

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

pub async fn execute(
    args: &DependsArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    super::dependency::do_execute(
        cli,
        io,
        super::dependency::DoExecuteArgs {
            package: &args.package,
            version: None,
            recursive: args.recursive,
            tree: args.tree,
            locked: args.locked,
            inverted: false,
        },
    )
}
