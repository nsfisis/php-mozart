use clap::Args;

#[derive(Args)]
pub struct AboutArgs {}

pub fn execute(_args: &AboutArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
