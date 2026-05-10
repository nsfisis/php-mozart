use clap::Args;
use mozart_core::{composer::composer_home, console::IoInterface};

#[derive(Args)]
pub struct GlobalArgs {
    /// The command name to run
    pub command_name: Option<String>,

    /// Arguments to pass to the command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub async fn execute(
    args: &GlobalArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    use clap::Parser as _;
    use std::fs;

    let command_name = match &args.command_name {
        Some(name) => name.clone(),
        None => {
            anyhow::bail!(
                "The global command requires a subcommand, e.g. `mozart global require package/name`"
            );
        }
    };

    let home = composer_home();

    fs::create_dir_all(&home)?;

    io.lock()
        .unwrap()
        .info(&format!("Changed current directory to {}", home.display()));

    // SAFETY: single-threaded at this point; no concurrent env access
    unsafe {
        std::env::remove_var("COMPOSER");
    }

    let mut argv: Vec<String> = vec!["mozart".to_string()];
    argv.extend(append_global_options(cli));
    argv.push("--working-dir".to_string());
    argv.push(home.to_string_lossy().into_owned());
    argv.push(command_name);
    argv.extend(args.args.iter().cloned());

    let new_cli = super::Cli::try_parse_from(&argv)?;
    Box::pin(crate::commands::execute(&new_cli)).await
}

fn append_global_options(cli: &super::Cli) -> Vec<String> {
    let mut opts: Vec<String> = Vec::new();

    for _ in 0..cli.verbose {
        opts.push("--verbose".to_string());
    }

    if cli.quiet {
        opts.push("--quiet".to_string());
    }

    if cli.profile {
        opts.push("--profile".to_string());
    }

    if cli.no_plugins {
        opts.push("--no-plugins".to_string());
    }

    if cli.no_scripts {
        opts.push("--no-scripts".to_string());
    }

    if cli.no_cache {
        opts.push("--no-cache".to_string());
    }

    if cli.no_interaction {
        opts.push("--no-interaction".to_string());
    }

    if cli.ansi {
        opts.push("--ansi".to_string());
    }

    if cli.no_ansi {
        opts.push("--no-ansi".to_string());
    }

    opts
}
