use clap::Parser;
use mozart::commands;

fn main() -> anyhow::Result<()> {
    let cli = commands::Cli::parse();
    commands::execute(&cli)
}
