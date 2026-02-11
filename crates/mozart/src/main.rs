use clap::Parser;
use mozart::commands;

fn main() {
    let cli = commands::Cli::parse();
    commands::execute(&cli.command);
}
