use clap::Args;
use colored::Colorize;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct DiagnoseArgs {}

// ─── Check result ─────────────────────────────────────────────────────────────

enum CheckResult {
    /// OK, with optional detail string.
    Ok(Option<String>),
    /// WARNING + message.
    Warning(String),
    /// FAIL + message.
    Fail(String),
    /// SKIP + reason.
    Skip(String),
    /// Informational line (no pass/fail prefix).
    Info(String),
}

// ─── Output helpers ───────────────────────────────────────────────────────────

/// Print "Checking {label}: OK/WARNING/FAIL/SKIP" and ratchet exit_code.
///
/// Exit code ratchet: Warning → 1 (if currently 0), Fail → 2 (always overrides 1).
fn print_check(label: &str, result: &CheckResult, exit_code: &mut i32) {
    match result {
        CheckResult::Ok(detail) => {
            let ok_str = "OK".green().bold();
            match detail {
                Some(d) => println!("Checking {label}: {ok_str} ({d})"),
                None => println!("Checking {label}: {ok_str}"),
            }
        }
        CheckResult::Warning(msg) => {
            let warn_str = "WARNING".yellow().bold();
            println!("Checking {label}: {warn_str}");
            println!("  {}", msg.yellow());
            if *exit_code < 1 {
                *exit_code = 1;
            }
        }
        CheckResult::Fail(msg) => {
            let fail_str = "FAIL".red().bold();
            println!("Checking {label}: {fail_str}");
            println!("  {}", msg.red());
            *exit_code = 2;
        }
        CheckResult::Skip(reason) => {
            let skip_str = "SKIP".cyan().bold();
            println!("Checking {label}: {skip_str} ({reason})");
        }
        CheckResult::Info(_) => {
            // Info results are not "checked" — use print_info_line instead.
        }
    }
}

/// Print an informational line (not a check result).
fn print_info_line(result: &CheckResult) {
    if let CheckResult::Info(msg) = result {
        println!("{msg}");
    }
}

// ─── Individual checks ────────────────────────────────────────────────────────

/// Check 1: Mozart version info (informational).
fn check_version() -> CheckResult {
    let version = env!("CARGO_PKG_VERSION");
    CheckResult::Info(format!("Mozart version {version}"))
}

/// Check 2 & 3: HTTP/HTTPS connectivity to Packagist.
///
/// Returns Ok if reachable, Fail if not, Skip if network is disabled.
async fn check_http_connectivity(url: &str) -> CheckResult {
    if std::env::var("COMPOSER_DISABLE_NETWORK").is_ok() {
        return CheckResult::Skip("COMPOSER_DISABLE_NETWORK is set".to_string());
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("mozart/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => return CheckResult::Fail(format!("Could not build HTTP client: {e}")),
    };

    match client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.is_redirection() {
                CheckResult::Ok(Some(format!("HTTP {}", status.as_u16())))
            } else {
                CheckResult::Warning(format!("Received HTTP {} from {url}", status.as_u16()))
            }
        }
        Err(e) => CheckResult::Fail(format!("Could not reach {url}: {e}")),
    }
}

/// Check 4: GitHub API connectivity.
async fn check_github_api() -> CheckResult {
    if std::env::var("COMPOSER_DISABLE_NETWORK").is_ok() {
        return CheckResult::Skip("COMPOSER_DISABLE_NETWORK is set".to_string());
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("mozart/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => return CheckResult::Fail(format!("Could not build HTTP client: {e}")),
    };

    let url = "https://api.github.com/";
    match client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.is_redirection() {
                CheckResult::Ok(Some(format!("HTTP {}", status.as_u16())))
            } else {
                CheckResult::Warning(format!("Received HTTP {} from GitHub API", status.as_u16()))
            }
        }
        Err(e) => CheckResult::Fail(format!("Could not reach GitHub API: {e}")),
    }
}

/// Check 5: HTTP proxy configuration.
///
/// Reports any configured proxy environment variables as informational.
fn check_http_proxy() -> CheckResult {
    let proxy_vars = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
    ];

    let mut found: Vec<String> = Vec::new();
    for var in &proxy_vars {
        if let Ok(val) = std::env::var(var) {
            found.push(format!("{var}={val}"));
        }
    }

    if found.is_empty() {
        CheckResult::Ok(Some("no proxy configured".to_string()))
    } else {
        CheckResult::Ok(Some(found.join(", ")))
    }
}

/// Check 6: composer.json validation.
///
/// Checks that it exists, is valid JSON, and has a `name` field.
fn check_composer_json(working_dir: &Path) -> CheckResult {
    let path = working_dir.join("composer.json");

    if !path.exists() {
        return CheckResult::Warning(format!(
            "composer.json not found in {}",
            working_dir.display()
        ));
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return CheckResult::Fail(format!("Could not read composer.json: {e}")),
    };

    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return CheckResult::Fail(format!("composer.json is not valid JSON: {e}"));
        }
    };

    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            return CheckResult::Fail(
                "composer.json must be a JSON object at the top level".to_string(),
            );
        }
    };

    if !obj.contains_key("name") {
        return CheckResult::Warning("composer.json is missing the \"name\" field".to_string());
    }

    CheckResult::Ok(None)
}

/// Check 7: composer.lock freshness.
///
/// If composer.lock exists, verify its content-hash matches the current composer.json.
fn check_composer_lock(working_dir: &Path) -> CheckResult {
    let lock_path = working_dir.join("composer.lock");

    if !lock_path.exists() {
        return CheckResult::Skip("composer.lock not found".to_string());
    }

    let composer_json_path = working_dir.join("composer.json");
    let composer_json_content = match std::fs::read_to_string(&composer_json_path) {
        Ok(c) => c,
        Err(_) => {
            return CheckResult::Skip(
                "could not read composer.json to compare against lock file".to_string(),
            );
        }
    };

    let lock = match mozart_registry::lockfile::LockFile::read_from_file(&lock_path) {
        Ok(l) => l,
        Err(e) => return CheckResult::Fail(format!("composer.lock is invalid: {e}")),
    };

    if lock.is_fresh(&composer_json_content) {
        CheckResult::Ok(None)
    } else {
        CheckResult::Warning(
            "composer.lock is out of date; run \"mozart update\" or \"mozart install\" to refresh it".to_string(),
        )
    }
}

/// Check 8: Git availability and minimum version.
///
/// Warns if git is not found or is older than 2.24.0.
fn check_git() -> CheckResult {
    let output = match std::process::Command::new("git").arg("--version").output() {
        Ok(o) => o,
        Err(_) => {
            return CheckResult::Warning(
                "git not found in PATH; some features may not work".to_string(),
            );
        }
    };

    if !output.status.success() {
        return CheckResult::Warning("git --version returned a non-zero exit code".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version_str = stdout.trim();

    // Parse version from output like "git version 2.39.1"
    match parse_git_version(version_str) {
        Some((major, minor, _patch)) => {
            // Require >= 2.24.0
            if major < 2 || (major == 2 && minor < 24) {
                CheckResult::Warning(format!(
                    "git {version_str} is older than the recommended minimum 2.24.0"
                ))
            } else {
                CheckResult::Ok(Some(version_str.to_string()))
            }
        }
        None => CheckResult::Ok(Some(version_str.to_string())),
    }
}

/// Parse git version output (e.g. "git version 2.39.1") into (major, minor, patch).
fn parse_git_version(output: &str) -> Option<(u64, u64, u64)> {
    // Extract the version number portion after "git version "
    let version_part = output.strip_prefix("git version ").unwrap_or(output);
    // Take only the first part before any space (e.g. "2.39.1.windows.1" → "2.39.1")
    let first_part = version_part.split_whitespace().next()?;
    let mut parts = first_part.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Check 9: Disk free space for a path.
///
/// Warns if < 1 MiB free. Uses `df -P` for portable output.
fn check_disk_space(path: &Path, label: &str) -> CheckResult {
    // Ensure the path exists before calling df
    if !path.exists() {
        return CheckResult::Skip(format!("{} does not exist", path.display()));
    }

    let output = match std::process::Command::new("df")
        .arg("-P")
        .arg(path)
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            return CheckResult::Skip("df not available on this platform".to_string());
        }
    };

    if !output.status.success() {
        return CheckResult::Skip(format!("df -P failed for {}", path.display()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_df_available_kib(&stdout) {
        Some(avail_kib) => {
            let avail_mib = avail_kib / 1024;
            let one_mib_kib = 1024u64;
            if avail_kib < one_mib_kib {
                CheckResult::Warning(format!(
                    "Low disk space on {label}: only {}KiB available",
                    avail_kib
                ))
            } else {
                CheckResult::Ok(Some(format!("{avail_mib}MiB free on {label}")))
            }
        }
        None => CheckResult::Skip("could not parse df output".to_string()),
    }
}

/// Parse the "Available" column (4th column) of `df -P` output, returning KiB.
///
/// The -P (POSIX) format guarantees 1024-byte blocks in the "Available" column.
fn parse_df_available_kib(df_output: &str) -> Option<u64> {
    // Skip the header line, then read the first data line
    let data_line = df_output.lines().nth(1)?;
    let mut cols = data_line.split_whitespace();
    // Columns: Filesystem, 1024-blocks, Used, Available, Capacity%, Mounted
    cols.next()?; // Filesystem
    cols.next()?; // 1024-blocks
    cols.next()?; // Used
    let available = cols.next()?;
    available.parse::<u64>().ok()
}

/// Check 10: Cache directory status.
///
/// Checks that the cache directory exists and is writable.
fn check_cache_dir(cache_dir: &Path) -> CheckResult {
    if !cache_dir.exists() {
        // Try to create it
        if let Err(e) = std::fs::create_dir_all(cache_dir) {
            return CheckResult::Fail(format!(
                "Cache directory {} does not exist and could not be created: {e}",
                cache_dir.display()
            ));
        }
        return CheckResult::Ok(Some(format!("created {}", cache_dir.display())));
    }

    // Check writability by attempting to create a temp file
    let test_file = cache_dir.join(".mozart_write_test");
    match std::fs::write(&test_file, b"test") {
        Ok(()) => {
            let _ = std::fs::remove_file(&test_file);
            CheckResult::Ok(Some(cache_dir.display().to_string()))
        }
        Err(e) => CheckResult::Fail(format!(
            "Cache directory {} is not writable: {e}",
            cache_dir.display()
        )),
    }
}

// ─── Main execute function ─────────────────────────────────────────────────────

pub async fn execute(
    _args: &DiagnoseArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let mut exit_code: i32 = 0;

    // Determine cache directory (same logic as build_cache_config)
    let cache_dir = if let Ok(dir) = std::env::var("COMPOSER_CACHE_DIR") {
        PathBuf::from(dir)
    } else {
        let base = if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            PathBuf::from(xdg)
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".cache")
        } else {
            PathBuf::from("/tmp")
        };
        base.join("mozart")
    };

    // 1. Mozart version info
    print_info_line(&check_version());
    println!();

    // 2. HTTPS connectivity to Packagist
    let https_result = check_http_connectivity("https://repo.packagist.org/packages.json").await;
    print_check(
        "https connectivity to packagist",
        &https_result,
        &mut exit_code,
    );

    // 3. HTTP connectivity to Packagist
    let http_result = check_http_connectivity("http://repo.packagist.org/packages.json").await;
    print_check(
        "http connectivity to packagist",
        &http_result,
        &mut exit_code,
    );

    // 4. GitHub API connectivity
    let github_result = check_github_api().await;
    print_check("github.com connectivity", &github_result, &mut exit_code);

    // 5. HTTP proxy config
    let proxy_result = check_http_proxy();
    print_check("http proxy", &proxy_result, &mut exit_code);

    // 6. composer.json validation
    let composer_json_result = check_composer_json(&working_dir);
    print_check("composer.json", &composer_json_result, &mut exit_code);

    // 7. composer.lock freshness
    let lock_result = check_composer_lock(&working_dir);
    print_check("composer.lock", &lock_result, &mut exit_code);

    // 8. Git availability
    let git_result = check_git();
    print_check("git", &git_result, &mut exit_code);

    // 9. Disk space — working directory
    let disk_wd_result = check_disk_space(&working_dir, "working directory");
    print_check(
        "disk free space (working directory)",
        &disk_wd_result,
        &mut exit_code,
    );

    // 9b. Disk space — cache directory
    let disk_cache_result = check_disk_space(&cache_dir, "cache directory");
    print_check(
        "disk free space (cache directory)",
        &disk_cache_result,
        &mut exit_code,
    );

    // 10. Cache directory status
    let cache_result = check_cache_dir(&cache_dir);
    print_check("cache directory", &cache_result, &mut exit_code);

    println!();
    if exit_code == 0 {
        println!("{}", "No issues found.".green());
    } else if exit_code == 1 {
        println!(
            "{}",
            "Some warnings were found. See above for details.".yellow()
        );
    } else {
        println!("{}", "Some errors were found. See above for details.".red());
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ── test_parse_git_version ────────────────────────────────────────────────

    #[test]
    fn test_parse_git_version() {
        assert_eq!(parse_git_version("git version 2.39.1"), Some((2, 39, 1)));
        assert_eq!(parse_git_version("git version 2.24.0"), Some((2, 24, 0)));
        assert_eq!(parse_git_version("git version 1.9.5"), Some((1, 9, 5)));
        // Windows-style suffix
        assert_eq!(
            parse_git_version("git version 2.40.1.windows.1"),
            Some((2, 40, 1))
        );
        // No patch component
        assert_eq!(parse_git_version("git version 2.39"), Some((2, 39, 0)));
        // Bare version (no "git version" prefix)
        assert_eq!(parse_git_version("3.0.0"), Some((3, 0, 0)));
    }

    // ── test_check_composer_json_valid ────────────────────────────────────────

    #[test]
    fn test_check_composer_json_valid() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();

        let result = check_composer_json(dir.path());
        assert!(
            matches!(result, CheckResult::Ok(_)),
            "expected Ok for valid composer.json"
        );
    }

    // ── test_check_composer_json_missing ─────────────────────────────────────

    #[test]
    fn test_check_composer_json_missing() {
        let dir = tempdir().unwrap();
        // Do not write a composer.json

        let result = check_composer_json(dir.path());
        assert!(
            matches!(result, CheckResult::Warning(_)),
            "expected Warning when composer.json is missing"
        );
    }

    // ── test_check_composer_json_invalid_json ─────────────────────────────────

    #[test]
    fn test_check_composer_json_invalid_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("composer.json"), b"{ this is not json ").unwrap();

        let result = check_composer_json(dir.path());
        assert!(
            matches!(result, CheckResult::Fail(_)),
            "expected Fail for invalid JSON"
        );
    }

    // ── test_check_composer_lock_fresh ────────────────────────────────────────

    #[test]
    fn test_check_composer_lock_fresh() {
        use mozart_registry::lockfile::LockFile;

        let dir = tempdir().unwrap();

        let composer_json = r#"{"name": "test/project", "require": {"php": ">=8.1"}}"#;
        fs::write(dir.path().join("composer.json"), composer_json).unwrap();

        let hash = LockFile::compute_content_hash(composer_json).unwrap();
        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: hash,
            packages: vec![],
            packages_dev: None,
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: None,
        };
        lock.write_to_file(&dir.path().join("composer.lock"))
            .unwrap();

        let result = check_composer_lock(dir.path());
        assert!(
            matches!(result, CheckResult::Ok(_)),
            "expected Ok for fresh lock file"
        );
    }

    // ── test_check_composer_lock_stale ────────────────────────────────────────

    #[test]
    fn test_check_composer_lock_stale() {
        use mozart_registry::lockfile::LockFile;

        let dir = tempdir().unwrap();

        let composer_json = r#"{"name": "test/project", "require": {"php": ">=8.1"}}"#;
        fs::write(dir.path().join("composer.json"), composer_json).unwrap();

        // Deliberately use a stale/wrong hash
        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "stale_hash_that_does_not_match".to_string(),
            packages: vec![],
            packages_dev: None,
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: None,
        };
        lock.write_to_file(&dir.path().join("composer.lock"))
            .unwrap();

        let result = check_composer_lock(dir.path());
        assert!(
            matches!(result, CheckResult::Warning(_)),
            "expected Warning for stale lock file"
        );
    }

    // ── test_check_composer_lock_missing ─────────────────────────────────────

    #[test]
    fn test_check_composer_lock_missing() {
        let dir = tempdir().unwrap();
        // Do not write a composer.lock

        let result = check_composer_lock(dir.path());
        assert!(
            matches!(result, CheckResult::Skip(_)),
            "expected Skip when composer.lock is missing"
        );
    }

    // ── test_check_disk_space_ok ──────────────────────────────────────────────

    #[test]
    fn test_check_disk_space_ok() {
        let dir = tempdir().unwrap();
        // Temp directories should always have plenty of free space
        let result = check_disk_space(dir.path(), "temp");
        // Accept Ok or Skip (on platforms where df isn't available)
        assert!(
            matches!(result, CheckResult::Ok(_) | CheckResult::Skip(_)),
            "expected Ok or Skip for disk space check on temp directory"
        );
    }

    // ── test_check_result_exit_code_ratcheting ────────────────────────────────

    #[test]
    fn test_check_result_exit_code_ratcheting() {
        let mut exit_code = 0i32;

        // Ok does not change exit code
        print_check("label", &CheckResult::Ok(None), &mut exit_code);
        assert_eq!(exit_code, 0);

        // Warning raises to 1
        print_check(
            "label",
            &CheckResult::Warning("warn".to_string()),
            &mut exit_code,
        );
        assert_eq!(exit_code, 1);

        // Another Ok does not lower from 1
        print_check("label", &CheckResult::Ok(None), &mut exit_code);
        assert_eq!(exit_code, 1);

        // Fail raises to 2
        print_check(
            "label",
            &CheckResult::Fail("fail".to_string()),
            &mut exit_code,
        );
        assert_eq!(exit_code, 2);

        // Warning does not lower from 2
        print_check(
            "label",
            &CheckResult::Warning("another warn".to_string()),
            &mut exit_code,
        );
        assert_eq!(exit_code, 2);
    }

    // ── test_check_http_proxy_none_set ───────────────────────────────────────

    #[test]
    fn test_check_http_proxy_none_set() {
        // Remove all proxy vars for this test
        let proxy_vars = [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "NO_PROXY",
            "no_proxy",
        ];
        for var in &proxy_vars {
            // SAFETY: tests run single-threaded for env mutation purposes.
            // We save and restore values to avoid polluting other tests.
            unsafe { std::env::remove_var(var) };
        }

        let result = check_http_proxy();
        match &result {
            CheckResult::Ok(detail) => {
                let detail_str = detail.as_deref().unwrap_or("");
                assert!(
                    detail_str.contains("no proxy"),
                    "expected 'no proxy configured' detail, got: {detail_str:?}"
                );
            }
            other => panic!(
                "expected Ok for proxy check with no proxy set, got: {:?}",
                std::mem::discriminant(other)
            ),
        }
    }

    // ── network tests (ignored by default) ───────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn test_check_https_packagist_connectivity() {
        let result = check_http_connectivity("https://repo.packagist.org/packages.json").await;
        assert!(
            matches!(result, CheckResult::Ok(_)),
            "expected Ok for HTTPS Packagist connectivity"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_check_http_packagist_connectivity() {
        let result = check_http_connectivity("http://repo.packagist.org/packages.json").await;
        assert!(
            matches!(result, CheckResult::Ok(_) | CheckResult::Warning(_)),
            "expected Ok or Warning for HTTP Packagist connectivity"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_check_github_api_connectivity() {
        let result = check_github_api().await;
        assert!(
            matches!(result, CheckResult::Ok(_)),
            "expected Ok for GitHub API connectivity"
        );
    }
}
