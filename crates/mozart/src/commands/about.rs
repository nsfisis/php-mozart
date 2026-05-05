use clap::Args;
use mozart_core::MOZART_VERSION;
use mozart_core::console;
use mozart_core::console_format;
use mozart_core::console_writeln;

#[derive(Args)]
pub struct AboutArgs {}

pub async fn execute(
    _args: &AboutArgs,
    _cli: &super::Cli,
    console: &console::Console,
) -> anyhow::Result<()> {
    console_writeln!(
        console,
        &console_format!(
            r#"<info>Mozart - Dependency Manager for PHP - version {MOZART_VERSION}</info>
<comment>Mozart is a dependency manager tracking local dependencies of your projects and libraries.
See https://getcomposer.org/ for more information.</comment>"#
        ),
    );
    Ok(())
}
