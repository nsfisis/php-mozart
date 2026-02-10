use clap::Args;

#[derive(Args)]
pub struct ExecArgs {
    /// The binary to run
    pub binary: Option<String>,

    /// Arguments to pass to the binary
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,

    /// List the available binaries
    #[arg(short, long)]
    pub list: bool,
}

pub fn execute(_args: &ExecArgs) {
    todo!()
}
