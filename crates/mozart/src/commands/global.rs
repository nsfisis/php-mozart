use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct GlobalArgs {
    /// The command name to run
    pub command_name: String,

    /// Arguments to pass to the command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

// ─── Main entry point ────────────────────────────────────────────────────────

pub fn execute(
    args: &GlobalArgs,
    cli: &super::Cli,
    console: &crate::console::Console,
) -> anyhow::Result<()> {
    use clap::Parser as _;
    use std::fs;

    let home = composer_home_dir()?;

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
    argv.push(args.command_name.clone());
    argv.extend(args.args.iter().cloned());

    let new_cli = super::Cli::try_parse_from(&argv)?;
    crate::commands::execute(&new_cli)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn composer_home_dir() -> anyhow::Result<PathBuf> {
    if let Ok(val) = std::env::var("COMPOSER_HOME")
        && !val.is_empty()
    {
        return Ok(PathBuf::from(val));
    }

    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("composer"));
    }

    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("Cannot determine home directory: $HOME is not set"))?;

    Ok(home.join(".config").join("composer"))
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Cli, Commands};
    use clap::Parser as _;

    fn default_cli() -> Cli {
        Cli::try_parse_from(["mozart", "about"]).unwrap()
    }

    // ── composer_home_dir tests ───────────────────────────────────────────────

    #[test]
    fn test_composer_home_dir_from_env() {
        // SAFETY: test-only; single-threaded env mutation
        unsafe {
            std::env::set_var("COMPOSER_HOME", "/tmp/test-composer-home");
        }
        let result = composer_home_dir().unwrap();
        assert_eq!(result, PathBuf::from("/tmp/test-composer-home"));
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var("COMPOSER_HOME");
        }
    }

    #[test]
    fn test_composer_home_dir_xdg() {
        // SAFETY: test-only; single-threaded env mutation
        unsafe {
            std::env::remove_var("COMPOSER_HOME");
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg-config");
        }
        let result = composer_home_dir().unwrap();
        assert_eq!(result, PathBuf::from("/tmp/test-xdg-config/composer"));
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn test_composer_home_dir_default() {
        // SAFETY: test-only; single-threaded env mutation
        unsafe {
            std::env::remove_var("COMPOSER_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let result = composer_home_dir().unwrap();
        let home = std::env::var("HOME").map(PathBuf::from).unwrap();
        assert_eq!(result, home.join(".config").join("composer"));
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
        if let Commands::Global(args) = cli.command {
            assert_eq!(args.command_name, "require");
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
        if let Commands::Global(args) = cli.command {
            assert_eq!(args.command_name, "require");
            assert!(args.args.contains(&"--no-update".to_string()));
        } else {
            panic!("Expected Global command");
        }
    }
}
