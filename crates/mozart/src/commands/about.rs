use clap::Args;
use mozart_core::console;
use mozart_core::console_format;

#[derive(Args)]
pub struct AboutArgs {}

pub async fn execute(
    _args: &AboutArgs,
    _cli: &super::Cli,
    console: &console::Console,
) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    console.write_stdout(
        &console_format!("<info>Mozart - Dependency Manager for PHP - version {version}</info>"),
        console::Verbosity::Normal,
    );
    console.write_stdout(
        &console_format!("<comment>Mozart is a dependency manager tracking local dependencies of your projects and libraries.
See https://getcomposer.org/ for more information.</comment>"),
        console::Verbosity::Normal,
    );
    Ok(())
}
