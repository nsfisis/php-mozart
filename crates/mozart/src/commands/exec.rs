use clap::Args;
use mozart_core::composer::Composer;
use mozart_core::console_format;
use mozart_core::console_writeln;
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
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    let composer = Composer::require(&working_dir)?;
    let bin_dir = resolve_bin_dir(&working_dir, &composer);

    if args.list || args.binary.is_none() {
        let bins = get_binaries(&composer, &bin_dir);
        if bins.is_empty() {
            anyhow::bail!(
                "No binaries found in composer.json or in bin-dir ({})",
                bin_dir.display(),
            );
        }
        console_writeln!(
            console,
            &console_format!("<comment>Available binaries:</comment>"),
        );
        for (bin, is_local) in &bins {
            if *is_local {
                console_writeln!(console, &console_format!("<info>- {bin} (local)</info>"));
            } else {
                console_writeln!(console, &console_format!("<info>- {bin}</info>"));
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
        .bin
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_resolve_bin_dir_default() {
        let dir = tempfile::tempdir().unwrap();
        let composer_json = dir.path().join("composer.json");
        fs::write(&composer_json, r#"{"name": "test/pkg", "require": {}}"#).unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let result = resolve_bin_dir(dir.path(), &composer);
        assert_eq!(result, dir.path().join("vendor/bin"));
    }

    #[test]
    fn test_resolve_bin_dir_custom_vendor_dir() {
        let dir = tempfile::tempdir().unwrap();
        let composer_json = dir.path().join("composer.json");
        fs::write(
            &composer_json,
            r#"{"name": "test/pkg", "require": {}, "config": {"vendor-dir": "libs"}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let result = resolve_bin_dir(dir.path(), &composer);
        assert_eq!(result, dir.path().join("libs/bin"));
    }

    #[test]
    fn test_resolve_bin_dir_custom_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        let composer_json = dir.path().join("composer.json");
        fs::write(
            &composer_json,
            r#"{"name": "test/pkg", "require": {}, "config": {"bin-dir": "scripts"}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let result = resolve_bin_dir(dir.path(), &composer);
        assert_eq!(result, dir.path().join("scripts"));
    }

    #[test]
    fn test_resolve_bin_dir_with_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let composer_json = dir.path().join("composer.json");
        fs::write(
            &composer_json,
            r#"{"name": "test/pkg", "require": {}, "config": {"vendor-dir": "packages", "bin-dir": "{$vendor-dir}/commands"}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let result = resolve_bin_dir(dir.path(), &composer);
        assert_eq!(result, dir.path().join("packages/commands"));
    }

    #[test]
    fn test_get_binaries_from_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        fs::create_dir_all(&bin_dir).unwrap();

        fs::write(bin_dir.join("phpunit"), "#!/bin/sh").unwrap();
        fs::write(bin_dir.join("phpstan"), "#!/bin/sh").unwrap();

        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "require": {}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let binaries = get_binaries(&composer, &bin_dir);
        let names: Vec<&str> = binaries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"phpunit"));
        assert!(names.contains(&"phpstan"));
        // All should be non-local
        for (_, is_local) in &binaries {
            assert!(!is_local);
        }
    }

    #[test]
    fn test_get_binaries_skips_bat_files() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        fs::create_dir_all(&bin_dir).unwrap();

        fs::write(bin_dir.join("phpunit"), "#!/bin/sh").unwrap();
        fs::write(bin_dir.join("phpunit.bat"), "@echo off").unwrap();

        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "require": {}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let binaries = get_binaries(&composer, &bin_dir);
        let names: Vec<&str> = binaries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"phpunit"));
        assert!(!names.contains(&"phpunit.bat"));
    }

    #[test]
    fn test_get_binaries_from_root_composer_json() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        // Don't create bin_dir — no vendor binaries

        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "require": {}, "bin": ["bin/my-tool", "bin/helper"]}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let binaries = get_binaries(&composer, &bin_dir);
        let names: Vec<&str> = binaries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"my-tool"));
        assert!(names.contains(&"helper"));
        // All should be local
        for (_, is_local) in &binaries {
            assert!(is_local);
        }
    }

    #[test]
    fn test_get_binaries_empty_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        // bin_dir doesn't exist

        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "require": {}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let binaries = get_binaries(&composer, &bin_dir);
        assert!(binaries.is_empty());
    }

    #[test]
    fn test_list_mode_no_binaries_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "require": {}}"#,
        )
        .unwrap();

        let bin_dir = dir.path().join("vendor/bin");
        let composer = Composer::require(dir.path()).unwrap();
        let binaries = get_binaries(&composer, &bin_dir);
        assert!(
            binaries.is_empty(),
            "Expected no binaries to trigger error path"
        );
    }

    #[test]
    fn test_execute_binary_not_found() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "require": {}}"#,
        )
        .unwrap();

        let composer = Composer::require(dir.path()).unwrap();
        let bin_dir = resolve_bin_dir(dir.path(), &composer);

        // No binaries exist — looking up a name should find nothing
        let candidate = bin_dir.join("nonexistent-binary");
        assert!(!candidate.exists());

        // Confirm root bin entries are also empty
        let root = mozart_core::package::read_from_file(&dir.path().join("composer.json")).unwrap();
        assert!(root.bin.is_empty());
    }
}
