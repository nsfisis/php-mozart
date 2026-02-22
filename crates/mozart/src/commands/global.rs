use clap::Args;

#[derive(Args)]
pub struct GlobalArgs {
    /// The command name to run
    pub command_name: Option<String>,

    /// Arguments to pass to the command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

// ─── Main entry point ────────────────────────────────────────────────────────

pub async fn execute(
    args: &GlobalArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
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

    let home = super::config_helpers::composer_home();

    fs::create_dir_all(&home)?;

    console.info(&format!("Changed current directory to {}", home.display()));

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

// ─── Helpers ─────────────────────────────────────────────────────────────────

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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Cli, Commands};
    use clap::Parser as _;

    fn default_cli() -> Cli {
        Cli::try_parse_from(["mozart", "about"]).unwrap()
    }

    // ── append_global_options tests ───────────────────────────────────────────

    #[test]
    fn test_append_global_options_empty() {
        let cli = default_cli();
        let opts = append_global_options(&cli);
        assert!(opts.is_empty());
    }

    #[test]
    fn test_append_global_options_verbose() {
        let cli = Cli::try_parse_from(["mozart", "-vv", "about"]).unwrap();
        let opts = append_global_options(&cli);
        assert_eq!(opts, vec!["--verbose", "--verbose"]);
    }

    #[test]
    fn test_append_global_options_all() {
        let cli = Cli::try_parse_from([
            "mozart",
            "--verbose",
            "--quiet",
            "--profile",
            "--no-plugins",
            "--no-scripts",
            "--no-cache",
            "--no-interaction",
            "--ansi",
            "about",
        ])
        .unwrap();
        let opts = append_global_options(&cli);
        assert!(opts.contains(&"--verbose".to_string()));
        assert!(opts.contains(&"--quiet".to_string()));
        assert!(opts.contains(&"--profile".to_string()));
        assert!(opts.contains(&"--no-plugins".to_string()));
        assert!(opts.contains(&"--no-scripts".to_string()));
        assert!(opts.contains(&"--no-cache".to_string()));
        assert!(opts.contains(&"--no-interaction".to_string()));
        assert!(opts.contains(&"--ansi".to_string()));
    }

    #[test]
    fn test_append_global_options_does_not_forward_working_dir() {
        let cli = Cli::try_parse_from(["mozart", "--working-dir", "/some/path", "about"]).unwrap();
        let opts = append_global_options(&cli);
        assert!(!opts.iter().any(|o| o.contains("working-dir")));
        assert!(!opts.iter().any(|o| o == "/some/path"));
    }

    #[test]
    fn test_global_args_has_correct_command() {
        // Verify GlobalArgs parses correctly through the CLI
        let cli = Cli::try_parse_from(["mozart", "global", "require", "vendor/package"]).unwrap();
        if let Some(Commands::Global(args)) = cli.command {
            assert_eq!(args.command_name, Some("require".to_string()));
            assert_eq!(args.args, vec!["vendor/package"]);
        } else {
            panic!("Expected Global command");
        }
    }

    #[test]
    fn test_global_args_hyphen_values() {
        // Verify hyphen values in trailing args are accepted
        let cli = Cli::try_parse_from(["mozart", "global", "require", "vendor/pkg", "--no-update"])
            .unwrap();
        if let Some(Commands::Global(args)) = cli.command {
            assert_eq!(args.command_name, Some("require".to_string()));
            assert!(args.args.contains(&"--no-update".to_string()));
        } else {
            panic!("Expected Global command");
        }
    }

    #[test]
    fn test_global_args_no_subcommand() {
        // Verify that no subcommand parses to None
        let cli = Cli::try_parse_from(["mozart", "global"]).unwrap();
        if let Some(Commands::Global(args)) = cli.command {
            assert_eq!(args.command_name, None);
        } else {
            panic!("Expected Global command");
        }
    }
}
