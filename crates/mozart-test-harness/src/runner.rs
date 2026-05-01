use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

use crate::parser::ParsedTest;

/// Outcome of running a parsed `.test` against the `mozart` binary.
///
/// The temp directory is kept alive in this struct so callers can inspect
/// files written by the run; it is removed when `RunResult` is dropped.
pub struct RunResult {
    pub working_dir: TempDir,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub final_lock: Option<String>,
    pub final_installed: Option<String>,
}

/// Set up a temp project from the parsed test, invoke `mozart` with the
/// `--RUN--` command, and capture the result.
pub fn run_test(test: &ParsedTest, mozart_bin: &Path) -> Result<RunResult> {
    let working_dir = TempDir::new().context("failed to create tempdir")?;
    let root = working_dir.path();

    std::fs::write(root.join("composer.json"), &test.composer)
        .context("failed to write composer.json")?;

    if let Some(lock) = &test.lock {
        std::fs::write(root.join("composer.lock"), lock)
            .context("failed to write composer.lock")?;
    }

    if let Some(installed) = &test.installed {
        let vendor_composer = root.join("vendor").join("composer");
        std::fs::create_dir_all(&vendor_composer)
            .context("failed to create vendor/composer dir")?;
        std::fs::write(vendor_composer.join("installed.json"), installed)
            .context("failed to write installed.json")?;
    }

    let args: Vec<&str> = test.run.split_whitespace().collect();
    let output = Command::new(mozart_bin)
        .args(&args)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to invoke {}", mozart_bin.display()))?;

    let final_lock = std::fs::read_to_string(root.join("composer.lock")).ok();
    let final_installed =
        std::fs::read_to_string(root.join("vendor").join("composer").join("installed.json")).ok();

    Ok(RunResult {
        working_dir,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
        final_lock,
        final_installed,
    })
}
