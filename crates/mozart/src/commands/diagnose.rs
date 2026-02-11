use clap::Args;

#[derive(Args)]
pub struct DiagnoseArgs {}

pub fn execute(_args: &DiagnoseArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
