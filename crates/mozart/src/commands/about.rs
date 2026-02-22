use clap::Args;
use mozart_core::console;

#[derive(Args)]
pub struct AboutArgs {}

pub async fn execute(
    _args: &AboutArgs,
    _cli: &super::Cli,
    console: &console::Console,
) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    console.write_stdout(
        &console::info(&format!(
            "Mozart - Dependency Manager for PHP - version {version}"
        ))
        .to_string(),
        console::Verbosity::Normal,
    );
    console.write_stdout(
        &console::comment(
            "Mozart is a dependency manager tracking local dependencies of your projects and libraries.
See https://getcomposer.org/ for more information.",
        )
        .to_string(),
        console::Verbosity::Normal,
    );
    Ok(())
}
