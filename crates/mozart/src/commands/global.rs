use clap::Args;

#[derive(Args)]
pub struct GlobalArgs {
    /// The command name to run
    pub command_name: String,

    /// Arguments to pass to the command
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,
}

pub fn execute(_args: &GlobalArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
