use crate::composer::Composer;
use clap::Args;
use colored::Colorize;
use mozart_core::MOZART_VERSION;
use mozart_core::config::Config;
use mozart_core::config_validator::{ValidatorOptions, validate_manifest};
use mozart_core::console::Console;
use mozart_core::console_writeln;
use mozart_core::factory::create_config;
use mozart_core::http::HttpDownloader;
use mozart_core::package::CompletePackage;
use std::borrow::Cow;
use std::path::Path;

#[derive(Args)]
pub struct DiagnoseArgs {}

/// Result of a single check, mirroring the `string|true|string[]|\Exception`
/// shape of `Composer\Command\DiagnoseCommand`'s private `checkX` methods.
enum CheckResult {
    /// `<info>OK</info>` with optional detail string. Equivalent to PHP `true`.
    Ok(Option<String>),
    /// `<warning>WARNING</warning>` + message lines.
    Warning(Vec<String>),
    /// `<error>FAIL</error>` + message lines.
    Fail(Vec<String>),
    /// `<info>SKIP</info>` + reason. Composer emits this inline for the
    /// `allow_url_fopen` / `COMPOSER_DISABLE_NETWORK` cases via the same
    /// `outputResult` path.
    Skip(String),
}

impl CheckResult {
    fn ok() -> Self {
        CheckResult::Ok(None)
    }

    fn ok_with(detail: impl Into<String>) -> Self {
        CheckResult::Ok(Some(detail.into()))
    }

    fn warn(msg: impl Into<String>) -> Self {
        CheckResult::Warning(vec![msg.into()])
    }

    fn fail(msg: impl Into<String>) -> Self {
        CheckResult::Fail(vec![msg.into()])
    }
}

/// Mirror of `DiagnoseCommand::outputResult`. Writes the leading
/// `Checking <label>: ` and then `<info>OK</>`, `<warning>WARNING</>` +
/// messages, `<error>FAIL</>` + messages, or `<info>SKIP</>` + reason.
///
/// Ratchets `exit_code`: `Warning` → 1 (if currently 0), `Fail` → 2 (always).
fn output_result(label: &str, result: &CheckResult, exit_code: &mut i32, console: &Console) {
    let prefix = format!("Checking {label}: ");
    match result {
        CheckResult::Ok(detail) => {
            let ok = "OK".green().bold();
            match detail {
                Some(d) => {
                    console_writeln!(console, "{prefix}{ok} {}", format!("({d})").bright_black())
                }
                None => console_writeln!(console, "{prefix}{ok}"),
            }
        }
        CheckResult::Warning(msgs) => {
            console_writeln!(console, "{prefix}{}", "WARNING".yellow().bold());
            for msg in msgs {
                console_writeln!(console, "{}", msg.yellow());
            }
            if *exit_code < 1 {
                *exit_code = 1;
            }
        }
        CheckResult::Fail(msgs) => {
            console_writeln!(console, "{prefix}{}", "FAIL".red().bold());
            for msg in msgs {
                console_writeln!(console, "{}", msg.red());
            }
            *exit_code = 2;
        }
        CheckResult::Skip(reason) => {
            console_writeln!(
                console,
                "{prefix}{} {}",
                "SKIP".cyan().bold(),
                format!("({reason})").bright_black(),
            );
        }
    }
}

// -----------------------------------------------------------------------
// Connectivity preflight (mirrors checkConnectivity / checkComposerNetworkHttpEnablement)
// -----------------------------------------------------------------------

/// Mirrors `DiagnoseCommand::checkComposerNetworkHttpEnablement` — returns a
/// `Skip` result when `COMPOSER_DISABLE_NETWORK` is set.
fn check_composer_network_http_enablement() -> Option<CheckResult> {
    if std::env::var("COMPOSER_DISABLE_NETWORK").is_ok_and(|v| !v.is_empty()) {
        return Some(CheckResult::Skip(
            "Network is disabled by COMPOSER_DISABLE_NETWORK.".to_string(),
        ));
    }
    None
}

/// Mirrors `DiagnoseCommand::checkConnectivityAndComposerNetworkHttpEnablement`.
///
/// Mozart has no `allow_url_fopen` analogue (we use reqwest directly), so the
/// upstream `checkConnectivity` half is a no-op here — only the network-disabled
/// gate fires.
fn check_connectivity_and_network_http_enablement() -> Option<CheckResult> {
    check_composer_network_http_enablement()
}

// -----------------------------------------------------------------------
// Individual checks
// -----------------------------------------------------------------------

/// Mirrors `DiagnoseCommand::checkComposerSchema`. Both Composer's
/// `ValidateCommand` and `DiagnoseCommand` instantiate the same
/// `Composer\Util\ConfigValidator`; we mirror that by calling
/// [`validate_manifest`] directly. Publish errors are intentionally
/// elided — Composer's diagnose discards them too via
/// `[$errors, , $warnings] = $validator->validate(...)`.
fn check_composer_schema(working_dir: &Path) -> CheckResult {
    let composer_json = working_dir.join("composer.json");
    let content = match std::fs::read_to_string(&composer_json) {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::fail(format!("could not read {}: {e}", composer_json.display()));
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return CheckResult::fail(format!(
                "{} does not contain valid JSON: {e}",
                composer_json.display()
            ));
        }
    };

    let result = validate_manifest(&value, &ValidatorOptions::default());
    if result.errors.is_empty() && result.warnings.is_empty() {
        CheckResult::ok()
    } else if !result.errors.is_empty() {
        let mut msgs = result.errors;
        msgs.extend(result.warnings);
        CheckResult::Fail(msgs)
    } else {
        CheckResult::Warning(result.warnings)
    }
}

/// Mirrors `DiagnoseCommand::checkComposerLockSchema`. Mozart does not have
/// a JSON-schema validator for `composer.lock` yet, so this currently emits
/// a `SKIP` placeholder rather than asserting compliance.
fn check_composer_lock_schema(_lock_path: &Path) -> CheckResult {
    CheckResult::Skip(
        "composer.lock schema validation is not yet implemented in Mozart".to_string(),
    )
}

/// Mirrors `DiagnoseCommand::checkGit`.
fn check_git() -> CheckResult {
    let output = match std::process::Command::new("git").arg("--version").output() {
        Ok(o) => o,
        Err(_) => return CheckResult::warn("No git process found"),
    };

    if !output.status.success() {
        return CheckResult::warn("git --version returned a non-zero exit code");
    }

    if let Ok(color_output) = std::process::Command::new("git")
        .args(["config", "color.ui"])
        .output()
    {
        let color_val = String::from_utf8_lossy(&color_output.stdout);
        if color_val.trim().eq_ignore_ascii_case("always") {
            return CheckResult::warn(
                "Your git color.ui setting is set to always, this is known to create issues. \
                 Use \"git config --global color.ui true\" to set it correctly.",
            );
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw = stdout.trim();
    let version_only = raw.strip_prefix("git version ").unwrap_or(raw);

    match parse_git_version(raw) {
        Some((major, minor, _patch)) => {
            if major < 2 || (major == 2 && minor < 24) {
                CheckResult::warn(format!(
                    "Your git version ({version_only}) is too old and possibly will cause issues. \
                     Please upgrade to git 2.24 or above"
                ))
            } else {
                CheckResult::ok_with(format!("git version {version_only}"))
            }
        }
        None => CheckResult::ok_with(version_only.to_string()),
    }
}

fn parse_git_version(output: &str) -> Option<(u64, u64, u64)> {
    let version_part = output.strip_prefix("git version ").unwrap_or(output);
    let first_part = version_part.split_whitespace().next()?;
    let mut parts = first_part.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Mirrors `DiagnoseCommand::checkHttp(proto, $config)`.
async fn check_http(proto: &str, http_downloader: &HttpDownloader, config: &Config) -> CheckResult {
    if let Some(skip) = check_connectivity_and_network_http_enablement() {
        return skip;
    }

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if proto == "https" && !config.secure_http {
        warnings.push(
            "Composer is configured to disable SSL/TLS protection. \
             This will leave remote HTTPS requests vulnerable to Man-In-The-Middle attacks."
                .to_string(),
        );
    }

    let url = format!("{proto}://repo.packagist.org/packages.json");
    if let Err(err) = http_downloader.get(&url).await {
        for hint in mozart_core::http::exception_hints(&err) {
            errors.push(hint);
        }
        errors.push(format!("[reqwest] {err}"));
    }

    if !errors.is_empty() {
        errors.extend(warnings);
        CheckResult::Fail(errors)
    } else if !warnings.is_empty() {
        CheckResult::Warning(warnings)
    } else {
        CheckResult::ok()
    }
}

/// Mirrors `DiagnoseCommand::checkComposerRepo`.
async fn check_composer_repo(
    url: &str,
    http_downloader: &HttpDownloader,
    config: &Config,
) -> CheckResult {
    if let Some(skip) = check_connectivity_and_network_http_enablement() {
        return skip;
    }

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if url.starts_with("https://") && !config.secure_http {
        warnings.push(
            "Composer is configured to disable SSL/TLS protection. \
             This will leave remote HTTPS requests vulnerable to Man-In-The-Middle attacks."
                .to_string(),
        );
    }

    if let Err(err) = http_downloader.get(url).await {
        for hint in mozart_core::http::exception_hints(&err) {
            errors.push(hint);
        }
        errors.push(format!("[reqwest] {err}"));
    }

    if !errors.is_empty() {
        errors.extend(warnings);
        CheckResult::Fail(errors)
    } else if !warnings.is_empty() {
        CheckResult::Warning(warnings)
    } else {
        CheckResult::ok()
    }
}

/// Mirrors `DiagnoseCommand::checkDiskSpace($config)`. Single check that
/// flags the first of `home` / `vendor-dir` to fall under 1MiB free.
fn check_disk_space(config: &Config) -> CheckResult {
    let home = config
        .get("home")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let vendor = config
        .get("vendor-dir")
        .and_then(|v| v.as_str().map(|s| s.to_string()));

    let min_bytes: u64 = 1024 * 1024;

    for dir in [home, vendor].into_iter().flatten() {
        let path = Path::new(&dir);
        if !path.exists() {
            continue;
        }
        if let Some(b) = disk_free_bytes(path)
            && b < min_bytes
        {
            return CheckResult::fail(format!("The disk hosting {} is full", path.display()));
        }
    }

    CheckResult::ok()
}

/// Returns free space in bytes for the filesystem hosting `path`. `None` when
/// the platform's `df` is unavailable or its output cannot be parsed.
fn disk_free_bytes(path: &Path) -> Option<u64> {
    let output = std::process::Command::new("df")
        .arg("-P")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let kib = parse_df_available_kib(&stdout)?;
    Some(kib.saturating_mul(1024))
}

/// Parse the "Available" column of `df -P` output (KiB).
fn parse_df_available_kib(df_output: &str) -> Option<u64> {
    let data_line = df_output.lines().nth(1)?;
    let mut cols = data_line.split_whitespace();
    cols.next()?; // Filesystem
    cols.next()?; // 1024-blocks
    cols.next()?; // Used
    cols.next()?.parse::<u64>().ok()
}

// -----------------------------------------------------------------------
// Orchestrator
// -----------------------------------------------------------------------

pub async fn execute(
    _args: &DiagnoseArgs,
    cli: &super::Cli,
    console: &Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    let mut exit_code: i32 = 0;

    let composer = Composer::try_load(&working_dir)?;
    let config: Cow<'_, Config> = if let Some(c) = &composer {
        Cow::Borrowed(c.config())
    } else {
        Cow::Owned(create_config()?)
    };

    let http_downloader = HttpDownloader::with_timeout(std::time::Duration::from_secs(10))?;

    // Step 4 (pubkey check) is phar-only — Mozart is not distributed as a phar.
    // Step 4b (`checkVersion`) is deferred until self-update lands.

    // Step 5: Mozart version line.
    console_writeln!(console, "Mozart version {MOZART_VERSION}");

    // Step 6: Mozart and its dependencies for vulnerabilities. Deferred — needs
    // a Mozart Auditor port.
    output_result(
        "Mozart and its dependencies for vulnerabilities",
        &CheckResult::Skip("audit is not yet implemented in Mozart".to_string()),
        &mut exit_code,
        console,
    );

    // Steps 7-8 (PHP/OpenSSL/curl/zip detection) are PHP-runtime concerns
    // and do not apply to Mozart. Composer's "Active plugins" line is also
    // omitted (Mozart has no plugin system).

    if composer.is_some() {
        output_result(
            "composer.json",
            &check_composer_schema(&working_dir),
            &mut exit_code,
            console,
        );

        let lock_path = working_dir.join("composer.lock");
        if lock_path.exists() {
            output_result(
                "composer.lock",
                &check_composer_lock_schema(&lock_path),
                &mut exit_code,
                console,
            );
        }
    }

    // Step 10: platform settings — PHP-runtime probe; deferred.
    output_result(
        "platform settings",
        &CheckResult::Skip("platform settings checks are not applicable to Mozart".to_string()),
        &mut exit_code,
        console,
    );

    // Step 11: git settings.
    output_result("git settings", &check_git(), &mut exit_code, console);

    // Step 12: HTTP / HTTPS connectivity to packagist.
    output_result(
        "http connectivity to packagist",
        &check_http("http", &http_downloader, &config).await,
        &mut exit_code,
        console,
    );
    output_result(
        "https connectivity to packagist",
        &check_http("https", &http_downloader, &config).await,
        &mut exit_code,
        console,
    );

    // Step 13: every additional `composer`-type repo.
    if let Some(composer) = &composer {
        for repo in composer.package().repositories().iter() {
            if repo.get("type").and_then(|v| v.as_str()) != Some("composer") {
                continue;
            }
            let Some(url) = repo.get("url").and_then(|v| v.as_str()) else {
                continue;
            };
            if !url.starts_with("http") {
                continue;
            }
            if url.starts_with("https://repo.packagist.org") {
                continue;
            }
            output_result(
                &format!("connectivity to {url}"),
                &check_composer_repo(url, &http_downloader, &config).await,
                &mut exit_code,
                console,
            );
        }
    }

    // Step 14: HTTP proxy probe — Mozart does not yet have a ProxyManager
    // port. Deferred.

    // Step 15: GitHub OAuth + rate limit — deferred until auth subsystem lands.

    // Step 16: disk free space.
    output_result(
        "disk free space",
        &check_disk_space(&config),
        &mut exit_code,
        console,
    );

    // Mirrors the `COMPOSER_IPRESOLVE` warning emitted by `checkPlatform`.
    if let Ok(val) = std::env::var("COMPOSER_IPRESOLVE")
        && (val == "4" || val == "6")
    {
        console_writeln!(
            console,
            "{}",
            format!("The COMPOSER_IPRESOLVE env var is set to {val} which may result in network failures below.").yellow(),
        );
    }

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_parse_git_version() {
        assert_eq!(parse_git_version("git version 2.39.1"), Some((2, 39, 1)));
        assert_eq!(parse_git_version("git version 2.24.0"), Some((2, 24, 0)));
        assert_eq!(parse_git_version("git version 1.9.5"), Some((1, 9, 5)));
        assert_eq!(
            parse_git_version("git version 2.40.1.windows.1"),
            Some((2, 40, 1))
        );
        assert_eq!(parse_git_version("git version 2.39"), Some((2, 39, 0)));
        assert_eq!(parse_git_version("3.0.0"), Some((3, 0, 0)));
    }

    #[test]
    fn test_check_composer_schema_valid() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/project", "license": "MIT", "require": {}}"#,
        )
        .unwrap();
        let result = check_composer_schema(dir.path());
        assert!(matches!(result, CheckResult::Ok(_)));
    }

    #[test]
    fn test_check_composer_schema_invalid_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("composer.json"), b"{ this is not json ").unwrap();
        let result = check_composer_schema(dir.path());
        assert!(matches!(result, CheckResult::Fail(_)));
    }

    #[test]
    fn test_check_composer_schema_warns_on_missing_license() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/project"}"#,
        )
        .unwrap();
        let result = check_composer_schema(dir.path());
        assert!(matches!(result, CheckResult::Warning(_)));
    }

    #[test]
    fn test_output_result_exit_code_ratcheting() {
        let console = Console::new(0, false, false, false, false);
        let mut exit_code = 0i32;

        output_result("label", &CheckResult::ok(), &mut exit_code, &console);
        assert_eq!(exit_code, 0);

        output_result(
            "label",
            &CheckResult::warn("warn"),
            &mut exit_code,
            &console,
        );
        assert_eq!(exit_code, 1);

        output_result("label", &CheckResult::ok(), &mut exit_code, &console);
        assert_eq!(exit_code, 1);

        output_result(
            "label",
            &CheckResult::fail("fail"),
            &mut exit_code,
            &console,
        );
        assert_eq!(exit_code, 2);

        output_result(
            "label",
            &CheckResult::warn("another warn"),
            &mut exit_code,
            &console,
        );
        assert_eq!(exit_code, 2);
    }

    #[test]
    fn test_check_composer_network_http_enablement_skips_when_disabled() {
        // SAFETY: tests that mutate env vars are inherently process-wide.
        unsafe { std::env::set_var("COMPOSER_DISABLE_NETWORK", "1") };
        let result = check_composer_network_http_enablement();
        assert!(matches!(result, Some(CheckResult::Skip(_))));
        unsafe { std::env::remove_var("COMPOSER_DISABLE_NETWORK") };
    }
}
