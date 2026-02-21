use clap::Args;
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

// ─── Main entry point ────────────────────────────────────────────────────────

pub fn execute(
    args: &ExecArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let bin_dir = resolve_bin_dir(&working_dir);

    if args.list || args.binary.is_none() {
        let binaries = get_binaries(&working_dir, &bin_dir);
        if binaries.is_empty() {
            anyhow::bail!(
                "No binaries found in composer.json or in bin-dir ({})",
                bin_dir.display()
            );
        }
        println!("Available binaries:");
        for (name, is_local) in &binaries {
            if *is_local {
                println!("- {} (local)", name);
            } else {
                println!("- {}", name);
            }
        }
        return Ok(());
    }

    let binary_name = args.binary.as_deref().unwrap();

    // Resolve binary path: check bin_dir first, then root package bin entries
    let bin_path = {
        let candidate = bin_dir.join(binary_name);
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
                    if stem == binary_name && p.exists() {
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
            binary_name
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
        std::process::exit(code);
    }

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn resolve_bin_dir(working_dir: &Path) -> PathBuf {
    let composer_json_path = working_dir.join("composer.json");
    if let Ok(content) = std::fs::read_to_string(&composer_json_path)
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content)
    {
        let vendor_dir = parsed["config"]["vendor-dir"].as_str().unwrap_or("vendor");
        let bin_dir = parsed["config"]["bin-dir"].as_str().unwrap_or_default();
        if !bin_dir.is_empty() {
            let resolved = bin_dir.replace("{$vendor-dir}", vendor_dir);
            return working_dir.join(resolved);
        }
        return working_dir.join(vendor_dir).join("bin");
    }
    working_dir.join("vendor/bin")
}

/// Returns a vec of (name, is_local) tuples for all available binaries.
/// Vendor binaries come first (is_local=false), then root package binaries
/// not already present (is_local=true). Result is sorted alphabetically.
fn get_binaries(working_dir: &Path, bin_dir: &Path) -> Vec<(String, bool)> {
    let mut binaries: Vec<(String, bool)> = Vec::new();

    // Collect from bin_dir (vendor binaries)
    if let Ok(entries) = std::fs::read_dir(bin_dir) {
        let mut vendor_names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.is_file() {
                    let name = path.file_name()?.to_str()?.to_string();
                    // Skip .bat files if a same-stem non-.bat file exists
                    if name.ends_with(".bat") {
                        let stem = &name[..name.len() - 4];
                        if bin_dir.join(stem).exists() {
                            return None;
                        }
                    }
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        vendor_names.sort();
        for name in vendor_names {
            binaries.push((name, false));
        }
    }

    // Collect from root composer.json bin entries
    let composer_json_path = working_dir.join("composer.json");
    if let Ok(root) = mozart_core::package::read_from_file(&composer_json_path) {
        let existing: std::collections::HashSet<&str> =
            binaries.iter().map(|(n, _)| n.as_str()).collect();
        let mut local: Vec<String> = root
            .bin
            .iter()
            .filter_map(|entry| {
                Path::new(entry)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .filter(|name| !existing.contains(name.as_str()))
            .collect();
        local.sort();
        for name in local {
            binaries.push((name, true));
        }
    }

    binaries.sort_by(|a, b| a.0.cmp(&b.0));
    binaries
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── resolve_bin_dir ───────────────────────────────────────────────────────

    #[test]
    fn test_resolve_bin_dir_default() {
        let dir = tempfile::tempdir().unwrap();
        let composer_json = dir.path().join("composer.json");
        fs::write(&composer_json, r#"{"name": "test/pkg", "require": {}}"#).unwrap();

        let result = resolve_bin_dir(dir.path());
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

        let result = resolve_bin_dir(dir.path());
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

        let result = resolve_bin_dir(dir.path());
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

        let result = resolve_bin_dir(dir.path());
        assert_eq!(result, dir.path().join("packages/commands"));
    }

    // ── get_binaries ──────────────────────────────────────────────────────────

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

        let binaries = get_binaries(dir.path(), &bin_dir);
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

        let binaries = get_binaries(dir.path(), &bin_dir);
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

        let binaries = get_binaries(dir.path(), &bin_dir);
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

        let binaries = get_binaries(dir.path(), &bin_dir);
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
        let binaries = get_binaries(dir.path(), &bin_dir);
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

        let bin_dir = resolve_bin_dir(dir.path());

        // No binaries exist — looking up a name should find nothing
        let candidate = bin_dir.join("nonexistent-binary");
        assert!(!candidate.exists());

        // Confirm root bin entries are also empty
        let root = mozart_core::package::read_from_file(&dir.path().join("composer.json")).unwrap();
        assert!(root.bin.is_empty());
    }
}
