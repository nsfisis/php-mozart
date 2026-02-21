use clap::Args;

#[derive(Args)]
pub struct RepositoryArgs {
    /// Action (list, add, remove, set-url, get-url, enable, disable)
    pub action: Option<String>,

    /// Repository name
    pub name: Option<String>,

    /// Argument 1 (URL or type depending on action)
    pub arg1: Option<String>,

    /// Argument 2
    pub arg2: Option<String>,

    /// Apply to the global config file
    #[arg(short, long)]
    pub global: bool,

    /// Use a specific config file
    #[arg(short, long)]
    pub file: Option<String>,

    /// Append the repository instead of prepending it
    #[arg(long)]
    pub append: bool,

    /// Add before a specific repository
    #[arg(long)]
    pub before: Option<String>,

    /// Add after a specific repository
    #[arg(long)]
    pub after: Option<String>,
}

pub fn execute(
    _args: &RepositoryArgs,
    _cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    todo!()
}
