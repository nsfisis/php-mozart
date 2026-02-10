use clap::Args;

#[derive(Args)]
pub struct RunScriptArgs {
    /// Script name to run
    pub script: Option<String>,

    /// Arguments to pass to the script
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,

    /// Set the script timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Sets the dev mode
    #[arg(long)]
    pub dev: bool,

    /// Disables the dev mode
    #[arg(long)]
    pub no_dev: bool,

    /// List the available scripts
    #[arg(short, long)]
    pub list: bool,
}

pub fn execute(_args: &RunScriptArgs) {
    todo!()
}
