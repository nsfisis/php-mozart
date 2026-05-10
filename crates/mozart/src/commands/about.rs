use clap::Args;
use mozart_core::MOZART_VERSION;
use mozart_core::console::IoInterface;
use mozart_core::console_writeln;

#[derive(Args)]
pub struct AboutArgs {}

pub async fn execute(
    _args: &AboutArgs,
    _cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    console_writeln!(
        io,
        r#"<info>Mozart - Dependency Manager for PHP - version {MOZART_VERSION}</info>
<comment>Mozart is a dependency manager tracking local dependencies of your projects and libraries.
See https://getcomposer.org/ for more information.</comment>"#,
    );
    Ok(())
}
