use clap::Args;
use clap::CommandFactory;
use clap_complete::aot::Shell;

#[derive(Args)]
pub struct CompletionArgs {
    /// The shell to generate completions for (auto-detected from $SHELL if omitted)
    #[arg(value_enum)]
    pub shell: Option<Shell>,
}

pub async fn execute(
    args: &CompletionArgs,
    _cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let shell = match args.shell {
        Some(s) => s,
        None => detect_shell()?,
    };
    let mut cmd = super::Cli::command();
    clap_complete::aot::generate(shell, &mut cmd, "mozart", &mut std::io::stdout());
    Ok(())
}

fn detect_shell() -> anyhow::Result<Shell> {
    let shell_env = std::env::var("SHELL")
        .map_err(|_| anyhow::anyhow!("Could not auto-detect shell. Please specify one of: bash, elvish, fish, powershell, zsh"))?;
    let basename = std::path::Path::new(&shell_env)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match basename {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        "elvish" => Ok(Shell::Elvish),
        "pwsh" | "powershell" => Ok(Shell::PowerShell),
        _ => anyhow::bail!(
            "Unrecognized shell '{}'. Please specify one of: bash, elvish, fish, powershell, zsh",
            basename
        ),
    }
}
