use clap::Args;
use clap::CommandFactory;
use clap_complete::aot::Shell;

#[derive(Args)]
pub struct CompletionArgs {
    /// The shell to generate completions for
    #[arg(value_enum)]
    pub shell: Shell,
}

pub async fn execute(
    args: &CompletionArgs,
    _cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let mut cmd = super::Cli::command();
    clap_complete::aot::generate(args.shell, &mut cmd, "mozart", &mut std::io::stdout());
    Ok(())
}
