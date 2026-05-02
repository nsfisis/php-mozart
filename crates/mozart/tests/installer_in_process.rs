//! In-process installer fixture runner.
//!
//! Mirrors Composer's PHPUnit-driven `InstallerTest`: parses the same
//! `.test` fixture files, sets up a tempdir with `composer.json` /
//! `composer.lock` / `vendor/composer/installed.json`, then invokes
//! `mozart::commands::{install,update}::run` directly with an empty
//! `RepositorySet` (Composer's `'packagist' => false` test config) and a
//! `TraceRecorderExecutor` (Composer's `InstallationManagerMock`).
//!
//! Step F will move every fixture in `installer.rs` over to this harness;
//! for now this file just demonstrates the path on a single fixture
//! (`suggest_replaced` — the original CI failure that motivated the whole
//! DI refactor).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use mozart::commands::{Cli, Commands, install, update};
use mozart_core::console::Console;
use mozart_core::exit_code::MozartError;
use mozart_registry::installer_executor::TraceRecorderExecutor;
use mozart_registry::repository::RepositorySet;
use mozart_test_harness::{ParsedTest, parse_test_file};
use tempfile::TempDir;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composer/tests/Composer/Test/Fixtures/installer")
}

/// Outcome of a single in-process fixture run.
struct InProcessRunResult {
    /// Kept alive so the caller can inspect on-disk artifacts; dropped
    /// (and removed) when this struct goes out of scope.
    _working_dir: TempDir,
    /// Composer-shape operation trace from `TraceRecorderExecutor`.
    /// Compare against the fixture's `--EXPECT--` section.
    trace: Vec<String>,
    /// Final `composer.lock` JSON, as written to disk by the runner.
    final_lock: Option<String>,
    /// Final `vendor/composer/installed.json`, as written to disk.
    final_installed: Option<String>,
    /// Mapped exit code: 0 for success, otherwise the carried
    /// [`MozartError::exit_code`] (or 1 for unclassified errors).
    exit_code: i32,
}

async fn run_fixture_in_process(test: &ParsedTest) -> anyhow::Result<InProcessRunResult> {
    let working_dir = TempDir::new()?;
    let root = working_dir.path();

    std::fs::write(root.join("composer.json"), &test.composer)?;
    if let Some(lock) = &test.lock {
        std::fs::write(root.join("composer.lock"), lock)?;
    }
    if let Some(installed) = &test.installed {
        let vendor_composer = root.join("vendor").join("composer");
        std::fs::create_dir_all(&vendor_composer)?;
        std::fs::write(vendor_composer.join("installed.json"), installed)?;
    }

    // Parse the `--RUN--` line through clap so we get the same arg semantics
    // the real CLI does — including default flags, validators, etc.
    let argv: Vec<String> = std::iter::once("mozart".to_string())
        .chain(test.run.split_whitespace().map(String::from))
        .collect();
    let cli = Cli::try_parse_from(&argv)?;

    // Quiet console: tests assert on `trace` / lock / installed, not on
    // captured stdout/stderr (Console doesn't yet support buffered sinks).
    let console = Console::new(0, true, false, true, true);
    let repositories = Arc::new(RepositorySet::empty());
    let mut executor = TraceRecorderExecutor::new();

    let outcome: anyhow::Result<()> = match &cli.command {
        Some(Commands::Install(args)) => {
            install::run(root, args, &console, repositories, &mut executor).await
        }
        Some(Commands::Update(args)) => {
            update::run(root, args, &console, repositories, &mut executor).await
        }
        other => anyhow::bail!(
            "unsupported run command in fixture: {:?}",
            other.is_some()
        ),
    };

    let exit_code = match &outcome {
        Ok(()) => 0,
        Err(e) => e
            .downcast_ref::<MozartError>()
            .map(|m| m.exit_code)
            .unwrap_or(1),
    };

    let final_lock = std::fs::read_to_string(root.join("composer.lock")).ok();
    let final_installed =
        std::fs::read_to_string(root.join("vendor").join("composer").join("installed.json")).ok();

    Ok(InProcessRunResult {
        _working_dir: working_dir,
        trace: executor.into_trace(),
        final_lock,
        final_installed,
        exit_code,
    })
}

fn run_fixture(ident: &str) {
    let filename = format!("{}.test", ident.replace('_', "-"));
    let path = fixtures_dir().join(&filename);
    let parsed = parse_test_file(&path)
        .unwrap_or_else(|e| panic!("failed to parse {}: {:#}", path.display(), e));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let result = runtime
        .block_on(run_fixture_in_process(&parsed))
        .unwrap_or_else(|e| panic!("failed to run {}: {:#}", path.display(), e));

    let expected_exit = parsed.expect_exit_code.unwrap_or(0);
    assert_eq!(
        result.exit_code,
        expected_exit,
        "exit code mismatch for {}\n--- trace ---\n{}",
        path.display(),
        result.trace.join("\n"),
    );

    // EXPECT (the trace) is the load-bearing assertion in Composer's
    // PHPUnit harness — every line of the operation log must match
    // byte-for-byte against `(string) $operation` after `strip_tags`.
    let expected_trace = parsed.expect.trim();
    let actual_trace = result.trace.join("\n");
    assert_eq!(
        actual_trace.trim(),
        expected_trace,
        "EXPECT trace mismatch for {}\n--- expected ---\n{}\n--- actual ---\n{}\n--- final lock ---\n{}\n--- final installed ---\n{}",
        path.display(),
        expected_trace,
        actual_trace,
        result.final_lock.as_deref().unwrap_or("(absent)"),
        result.final_installed.as_deref().unwrap_or("(absent)"),
    );
}

// ────────────────────────────────────────────────────────────────────────────
// In-process fixtures
//
// Step F will migrate every fixture from `installer.rs` to this harness.
// For now this file holds just the proof-of-concept: `suggest_replaced`,
// the original CI failure (the spawn runner can't reach Packagist for
// `b/b`, even though `c/c` replaces it).
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn suggest_replaced_in_process() {
    run_fixture("suggest_replaced");
}
