use crate::console;
use clap::Args;

#[derive(Args)]
pub struct AboutArgs {}

pub fn execute(_args: &AboutArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        "{}",
        console::info(&format!(
            "Mozart - Dependency Manager for PHP - version {version}"
        ))
    );
    println!(
        "{}",
        console::comment(
            "Mozart is a dependency manager tracking local dependencies of your projects and libraries.
See https://getcomposer.org/ for more information."
        )
    );
    Ok(())
}
