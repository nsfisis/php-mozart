use clap::Args;

#[derive(Args)]
pub struct StatusArgs {}

pub fn execute(_args: &StatusArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
