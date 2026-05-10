use crate::composer::Composer;
use clap::Args;
use mozart_core::package::PackageInterface as _;
use mozart_core::{console::IoInterface, console_writeln};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ExecArgs {
    /// The binary to run
    pub binary: Option<String>,

    /// Arguments to pass to the binary
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,

    /// List the available binaries
    #[arg(short, long)]
    pub list: bool,
}

pub async fn execute(
    args: &ExecArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    let composer = Composer::require(io.clone(), &working_dir)?;
    let bin_dir = resolve_bin_dir(&working_dir, &composer);

    if args.list || args.binary.is_none() {
        let bins = get_binaries(&composer, &bin_dir);
        if bins.is_empty() {
            anyhow::bail!(
                "No binaries found in composer.json or in bin-dir ({})",
                bin_dir.display(),
            );
        }
        console_writeln!(io, "<comment>Available binaries:</comment>");
        for (bin, is_local) in &bins {
            if *is_local {
                console_writeln!(io, "<info>- {bin} (local)</info>");
            } else {
                console_writeln!(io, "<info>- {bin}</info>");
            }
        }
        return Ok(());
    }

    let binary = args.binary.as_deref().unwrap();

    // Resolve binary path: check bin_dir first, then root package bin entries
    let bin_path = {
        let candidate = bin_dir.join(binary);
        if candidate.exists() {
            Some(candidate)
        } else {
            // Check root composer.json bin entries
            let composer_json_path = working_dir.join("composer.json");
            if let Ok(root) = mozart_core::package::read_from_file(&composer_json_path) {
                root.bin.into_iter().find_map(|entry| {
                    let p = working_dir.join(&entry);
                    let stem = Path::new(&entry)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&entry);
                    if stem == binary && p.exists() {
                        Some(p)
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        }
    };

    let bin_path = bin_path.ok_or_else(|| {
        anyhow::anyhow!(
            "Binary \"{}\" not found. Use --list to see available binaries.",
            binary
        )
    })?;

    // Build PATH with bin_dir prepended
    let path_env = {
        let current_path = std::env::var_os("PATH").unwrap_or_default();
        let mut parts: Vec<PathBuf> = vec![bin_dir.clone()];
        parts.extend(std::env::split_paths(&current_path));
        std::env::join_paths(parts)?
    };

    let status = std::process::Command::new(&bin_path)
        .args(&args.args)
        .env("PATH", path_env)
        .current_dir(&working_dir)
        .status()?;

    let code = status.code().unwrap_or(1);
    if code != 0 {
        return Err(mozart_core::exit_code::bail_silent(code));
    }

    Ok(())
}

fn resolve_bin_dir(working_dir: &Path, composer: &Composer) -> PathBuf {
    working_dir.join(&composer.config().bin_dir)
}

/// Returns a vec of (name, is_local) tuples for all available binaries.
/// Vendor binaries come first (is_local=false), then root package binaries.
fn get_binaries(composer: &Composer, bin_dir: &Path) -> Vec<(String, bool)> {
    let bins: Vec<(String, bool)> = if let Ok(entries) = std::fs::read_dir(bin_dir) {
        let mut bins: Vec<(String, bool)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| Some(e.path().file_name()?.to_string_lossy().into_owned()))
            .map(|e| (e, false))
            .collect();
        bins.sort();
        bins
    } else {
        Vec::new()
    };

    let local_bins: Vec<(String, bool)> = composer
        .package()
        .binaries()
        .iter()
        .filter_map(|e| Some(PathBuf::from(e).file_name()?.to_string_lossy().into_owned()))
        .map(|e| (e, true))
        .collect();

    let mut binaries = Vec::new();
    let mut previous_bin: Option<&String> = None;
    for (name, is_local) in bins.iter().chain(&local_bins) {
        if let Some(prev) = previous_bin
            && *name == format!("{prev}.bat")
        {
            continue;
        }
        previous_bin = Some(name);
        binaries.push((name.clone(), *is_local));
    }

    binaries
}
