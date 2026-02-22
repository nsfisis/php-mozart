use clap::Args;
use mozart_core::console_format;
use std::io::Write;
use std::path::{Path, PathBuf};

// ─── CLI args ─────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct SelfUpdateArgs {
    /// Version to update to (e.g., "0.2.0"). Defaults to latest.
    pub version: Option<String>,

    /// Revert to the previously installed version
    #[arg(short, long)]
    pub rollback: bool,

    /// Allow updating to pre-release versions
    #[arg(long)]
    pub preview: bool,

    /// Delete old backups during self-update
    #[arg(long)]
    pub clean_backups: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,
}

// ─── GitHub API types ─────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

// ─── Constants ────────────────────────────────────────────────────────────────

const GITHUB_REPO: &str = "kenpfowler/mozart";
const GITHUB_API_BASE: &str = "https://api.github.com/repos";
const BACKUP_EXTENSION: &str = ".old";

// ─── Public entry point ───────────────────────────────────────────────────────

pub async fn execute(
    args: &SelfUpdateArgs,
    _cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("Could not determine current executable path: {e}"))?;

    let data_dir = get_data_dir()?;
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        anyhow::anyhow!(
            "Could not create data directory {}: {e}",
            data_dir.display()
        )
    })?;

    if args.rollback {
        rollback(&current_exe, &data_dir)
    } else {
        update(args, &current_exe, &data_dir).await
    }
}

// ─── Data directory ───────────────────────────────────────────────────────────

fn get_data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("MOZART_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("Could not determine home directory (HOME not set)"))?;

    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("mozart"))
}

// ─── Version helpers ──────────────────────────────────────────────────────────

fn get_current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Returns the platform-specific binary asset name for the current build target.
///
/// Examples: `mozart-linux-x86_64`, `mozart-macos-aarch64`, `mozart-windows-x86_64.exe`
fn platform_asset_name() -> anyhow::Result<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        anyhow::bail!("Unsupported operating system for self-update");
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        anyhow::bail!("Unsupported architecture for self-update");
    };

    if cfg!(target_os = "windows") {
        Ok(format!("mozart-{os}-{arch}.exe"))
    } else {
        Ok(format!("mozart-{os}-{arch}"))
    }
}

// ─── Channel helper ───────────────────────────────────────────────────────────

fn effective_channel(preview: bool) -> &'static str {
    if preview { "preview" } else { "stable" }
}

// ─── Backup version helper ────────────────────────────────────────────────────

fn version_from_backup(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("mozart-"))
        .and_then(|n| n.strip_suffix(BACKUP_EXTENSION))
        .unwrap_or("unknown")
        .to_string()
}

// ─── GitHub fetching ──────────────────────────────────────────────────────────

async fn fetch_releases(include_prerelease: bool) -> anyhow::Result<Vec<GitHubRelease>> {
    let url = format!("{GITHUB_API_BASE}/{GITHUB_REPO}/releases");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(mozart_core::http::user_agent())
        .build()
        .map_err(|e| anyhow::anyhow!("Could not build HTTP client: {e}"))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Could not fetch releases from GitHub: {e}"))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "GitHub API returned HTTP {} when fetching releases",
            response.status().as_u16()
        );
    }

    let mut releases: Vec<GitHubRelease> = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Could not parse GitHub releases response: {e}"))?;

    if !include_prerelease {
        releases.retain(|r| !r.prerelease);
    }

    Ok(releases)
}

fn find_target_release<'a>(
    releases: &'a [GitHubRelease],
    target_version: Option<&str>,
) -> anyhow::Result<&'a GitHubRelease> {
    if releases.is_empty() {
        anyhow::bail!("No releases found");
    }

    match target_version {
        None => {
            // Return the first (latest) release
            Ok(&releases[0])
        }
        Some(version) => {
            // Normalize: strip leading 'v' from both the requested version and tag names
            let normalized = version.strip_prefix('v').unwrap_or(version);

            releases
                .iter()
                .find(|r| {
                    let tag = r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name);
                    tag == normalized
                })
                .ok_or_else(|| anyhow::anyhow!("Release version \"{version}\" not found"))
        }
    }
}

fn find_asset<'a>(release: &'a GitHubRelease, asset_name: &str) -> anyhow::Result<&'a GitHubAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No asset named \"{asset_name}\" found in release {}",
                release.tag_name
            )
        })
}

// ─── Download ─────────────────────────────────────────────────────────────────

async fn download_asset(
    asset: &GitHubAsset,
    dest: &Path,
    show_progress: bool,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(mozart_core::http::user_agent())
        .build()
        .map_err(|e| anyhow::anyhow!("Could not build HTTP client: {e}"))?;

    let response = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Could not download asset: {e}"))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Download request returned HTTP {}",
            response.status().as_u16()
        );
    }

    let mut file = std::fs::File::create(dest).map_err(|e| {
        anyhow::anyhow!("Could not create destination file {}: {e}", dest.display())
    })?;

    let total_bytes = asset.size;
    let mut downloaded: u64 = 0;
    let mut stream = response;

    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("Error reading download stream: {e}"))?
    {
        file.write_all(&chunk)
            .map_err(|e| anyhow::anyhow!("Error writing to destination file: {e}"))?;
        downloaded += chunk.len() as u64;

        if show_progress && total_bytes > 0 {
            let pct = (downloaded * 100) / total_bytes;
            eprint!("\r    Downloading... {pct}% ({downloaded}/{total_bytes} bytes)");
            let _ = std::io::stderr().flush();
        }
    }

    if show_progress && total_bytes > 0 {
        eprintln!(); // newline after progress
    }

    Ok(())
}

// ─── Core update flow ─────────────────────────────────────────────────────────

async fn update(args: &SelfUpdateArgs, current_exe: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let current_version = get_current_version();
    let channel = effective_channel(args.preview);

    // Fetch releases
    let releases = fetch_releases(args.preview).await?;

    // Find target release
    let target_release = find_target_release(&releases, args.version.as_deref())?;

    // Normalize tag version for comparison
    let target_version = target_release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&target_release.tag_name);

    // If no explicit version was requested and we're already up-to-date, bail early
    if args.version.is_none() && target_version == current_version {
        println!(
            "{}",
            console_format!(
                "<info>You are already using the latest available Mozart version {current_version} ({channel} channel).</info>"
            )
        );

        if args.clean_backups {
            // Preserve the most recent backup
            let latest = find_latest_backup(data_dir).ok();
            clean_backups(data_dir, latest.as_deref())?;
            println!(
                "{}",
                console_format!("<comment>Old backups removed.</comment>")
            );
        }

        return Ok(());
    }

    println!("Upgrading to version {target_version} ({channel} channel).");

    // Find the platform asset
    let asset_name = platform_asset_name()?;
    let asset = find_asset(target_release, &asset_name)?;

    println!("Downloading {} ({} bytes)...", asset.name, asset.size);

    // Download to a tempfile
    let tmp = tempfile::Builder::new()
        .prefix("mozart-download-")
        .tempfile_in(data_dir)
        .map_err(|e| anyhow::anyhow!("Could not create temporary file: {e}"))?;
    let tmp_path = tmp.path().to_path_buf();

    download_asset(asset, &tmp_path, !args.no_progress).await?;

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)
            .map_err(|e| anyhow::anyhow!("Could not set executable permissions: {e}"))?;
    }

    // Backup current binary to data_dir
    let backup_name = format!("mozart-{current_version}{BACKUP_EXTENSION}");
    let backup_path = data_dir.join(&backup_name);
    std::fs::copy(current_exe, &backup_path).map_err(|e| {
        anyhow::anyhow!(
            "Could not backup current binary to {}: {e}",
            backup_path.display()
        )
    })?;

    // Atomically replace current binary
    self_replace::self_replace(&tmp_path)
        .map_err(|e| anyhow::anyhow!("Could not replace binary: {e}"))?;

    // tmp is still in scope and will be cleaned up; the replace succeeded
    drop(tmp);

    println!(
        "{}",
        console_format!(
            "<info>Mozart updated successfully from {current_version} to {target_version}</info>"
        )
    );
    println!("Use `mozart self-update --rollback` to return to version {current_version}");

    if args.clean_backups {
        clean_backups(data_dir, Some(&backup_path))?;
        println!(
            "{}",
            console_format!("<comment>Old backups removed.</comment>")
        );
    }

    Ok(())
}

// ─── Rollback ─────────────────────────────────────────────────────────────────

fn rollback(current_exe: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let backup = find_latest_backup(data_dir)?;
    let backup_version = version_from_backup(&backup);

    println!("Rolling back to version {backup_version}...");

    // Set executable permission on Unix before replacing
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&backup)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&backup, perms)
            .map_err(|e| anyhow::anyhow!("Could not set permissions on backup: {e}"))?;
    }

    self_replace::self_replace(&backup)
        .map_err(|e| anyhow::anyhow!("Could not restore backup: {e}"))?;

    println!(
        "{}",
        console_format!("<info>Rollback successful. Restored version {backup_version}</info>")
    );

    let _ = current_exe; // suppress unused warning
    Ok(())
}

fn find_latest_backup(data_dir: &Path) -> anyhow::Result<PathBuf> {
    let entries = std::fs::read_dir(data_dir).map_err(|e| {
        anyhow::anyhow!("Could not read data directory {}: {e}", data_dir.display())
    })?;

    let mut backups: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(BACKUP_EXTENSION))
                .unwrap_or(false)
        })
        .collect();

    if backups.is_empty() {
        anyhow::bail!("No backup found. Cannot rollback.");
    }

    // Sort by file name — the version string embedded in the name gives a stable order.
    // Use modification time as tiebreaker when available.
    backups.sort_by(|a, b| {
        let mtime_a = a.metadata().and_then(|m| m.modified()).ok();
        let mtime_b = b.metadata().and_then(|m| m.modified()).ok();
        match (mtime_a, mtime_b) {
            (Some(ta), Some(tb)) => tb.cmp(&ta), // newest first
            _ => b.file_name().cmp(&a.file_name()),
        }
    });

    Ok(backups.into_iter().next().unwrap())
}

fn clean_backups(data_dir: &Path, except: Option<&Path>) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(data_dir).map_err(|e| {
        anyhow::anyhow!("Could not read data directory {}: {e}", data_dir.display())
    })?;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_backup = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(BACKUP_EXTENSION))
            .unwrap_or(false);

        if is_backup {
            if let Some(exc) = except
                && path == exc {
                    continue;
                }
            std::fs::remove_file(&path)
                .map_err(|e| anyhow::anyhow!("Could not remove backup {}: {e}", path.display()))?;
        }
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_release(tag: &str, prerelease: bool, assets: Vec<GitHubAsset>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            prerelease,
            assets,
        }
    }

    fn make_asset(name: &str, url: &str) -> GitHubAsset {
        GitHubAsset {
            name: name.to_string(),
            browser_download_url: url.to_string(),
            size: 1024,
        }
    }

    // ── test_platform_asset_name ──────────────────────────────────────────────

    #[test]
    fn test_platform_asset_name() {
        let name = platform_asset_name().expect("platform_asset_name should succeed");
        assert!(!name.is_empty(), "asset name must not be empty");
        assert!(
            name.starts_with("mozart-"),
            "asset name should start with 'mozart-', got: {name}"
        );
        // Verify the name matches the expected pattern: mozart-<os>-<arch>[.exe]
        assert!(
            name.contains("linux") || name.contains("macos") || name.contains("windows"),
            "asset name should contain an OS, got: {name}"
        );
        assert!(
            name.contains("x86_64") || name.contains("aarch64") || name.contains("x86"),
            "asset name should contain an architecture, got: {name}"
        );
    }

    // ── test_find_target_release_latest ───────────────────────────────────────

    #[test]
    fn test_find_target_release_latest() {
        let releases = vec![
            make_release("v0.3.0", false, vec![]),
            make_release("v0.2.0", false, vec![]),
            make_release("v0.1.0", false, vec![]),
        ];

        let result = find_target_release(&releases, None).expect("should find latest");
        assert_eq!(result.tag_name, "v0.3.0");
    }

    // ── test_find_target_release_specific_version ─────────────────────────────

    #[test]
    fn test_find_target_release_specific_version() {
        let releases = vec![
            make_release("v0.3.0", false, vec![]),
            make_release("v0.2.0", false, vec![]),
            make_release("v0.1.0", false, vec![]),
        ];

        // Without v prefix
        let result = find_target_release(&releases, Some("0.2.0")).expect("should find 0.2.0");
        assert_eq!(result.tag_name, "v0.2.0");

        // With v prefix
        let result_v = find_target_release(&releases, Some("v0.1.0")).expect("should find v0.1.0");
        assert_eq!(result_v.tag_name, "v0.1.0");
    }

    // ── test_find_target_release_not_found ────────────────────────────────────

    #[test]
    fn test_find_target_release_not_found() {
        let releases = vec![
            make_release("v0.3.0", false, vec![]),
            make_release("v0.2.0", false, vec![]),
        ];

        let result = find_target_release(&releases, Some("9.9.9"));
        assert!(result.is_err(), "should return error for missing version");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("9.9.9"),
            "error message should mention the version"
        );
    }

    // ── test_find_target_release_empty ────────────────────────────────────────

    #[test]
    fn test_find_target_release_empty() {
        let releases: Vec<GitHubRelease> = vec![];

        let result = find_target_release(&releases, None);
        assert!(
            result.is_err(),
            "should return error for empty release list"
        );
    }

    // ── test_find_asset_found ─────────────────────────────────────────────────

    #[test]
    fn test_find_asset_found() {
        let asset = make_asset(
            "mozart-linux-x86_64",
            "https://example.com/mozart-linux-x86_64",
        );
        let release = make_release(
            "v0.2.0",
            false,
            vec![
                make_asset(
                    "mozart-macos-aarch64",
                    "https://example.com/mozart-macos-aarch64",
                ),
                asset,
            ],
        );

        let found = find_asset(&release, "mozart-linux-x86_64").expect("should find asset");
        assert_eq!(found.name, "mozart-linux-x86_64");
        assert_eq!(
            found.browser_download_url,
            "https://example.com/mozart-linux-x86_64"
        );
    }

    // ── test_find_asset_not_found ─────────────────────────────────────────────

    #[test]
    fn test_find_asset_not_found() {
        let release = make_release(
            "v0.2.0",
            false,
            vec![make_asset(
                "mozart-linux-x86_64",
                "https://example.com/mozart-linux-x86_64",
            )],
        );

        let result = find_asset(&release, "mozart-windows-x86_64.exe");
        assert!(result.is_err(), "should return error for missing asset");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mozart-windows-x86_64.exe"),
            "error message should mention the asset name"
        );
    }

    // ── test_get_data_dir_from_env ────────────────────────────────────────────

    #[test]
    fn test_get_data_dir_from_env() {
        let dir = tempdir().unwrap();
        let expected = dir.path().to_path_buf();

        // SAFETY: test-only env mutation
        unsafe { std::env::set_var("MOZART_DATA_DIR", &expected) };

        let result = get_data_dir().expect("should succeed with MOZART_DATA_DIR set");
        assert_eq!(result, expected);

        unsafe { std::env::remove_var("MOZART_DATA_DIR") };
    }

    // ── test_get_data_dir_default ─────────────────────────────────────────────

    #[test]
    fn test_get_data_dir_default() {
        // Ensure MOZART_DATA_DIR is not set
        unsafe { std::env::remove_var("MOZART_DATA_DIR") };

        let result = get_data_dir().expect("should succeed when HOME is set");
        let path_str = result.to_string_lossy();
        assert!(
            path_str.ends_with(".local/share/mozart"),
            "default data dir should end with .local/share/mozart, got: {path_str}"
        );
    }

    // ── test_find_latest_backup ───────────────────────────────────────────────

    #[test]
    fn test_find_latest_backup() {
        let dir = tempdir().unwrap();

        // Create two backup files; the second one is newer
        let old_backup = dir.path().join("mozart-0.1.0.old");
        let new_backup = dir.path().join("mozart-0.2.0.old");
        fs::write(&old_backup, b"old binary").unwrap();
        // Ensure the newer file has a later mtime
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&new_backup, b"new binary").unwrap();

        let found = find_latest_backup(dir.path()).expect("should find latest backup");
        assert_eq!(found, new_backup);
    }

    // ── test_find_latest_backup_empty ─────────────────────────────────────────

    #[test]
    fn test_find_latest_backup_empty() {
        let dir = tempdir().unwrap();
        // No backup files present
        let result = find_latest_backup(dir.path());
        assert!(result.is_err(), "should return error when no backups found");
    }

    // ── test_clean_backups ────────────────────────────────────────────────────

    #[test]
    fn test_clean_backups() {
        let dir = tempdir().unwrap();

        let backup1 = dir.path().join("mozart-0.1.0.old");
        let backup2 = dir.path().join("mozart-0.2.0.old");
        let keep = dir.path().join("somefile.txt");

        fs::write(&backup1, b"binary").unwrap();
        fs::write(&backup2, b"binary").unwrap();
        fs::write(&keep, b"keep me").unwrap();

        clean_backups(dir.path(), None).expect("clean_backups should succeed");

        assert!(!backup1.exists(), "backup1 should be removed");
        assert!(!backup2.exists(), "backup2 should be removed");
        assert!(keep.exists(), "non-backup file should remain");
    }

    // ── test_clean_backups_with_except ────────────────────────────────────────

    #[test]
    fn test_clean_backups_with_except() {
        let dir = tempdir().unwrap();

        let backup1 = dir.path().join("mozart-0.1.0.old");
        let backup2 = dir.path().join("mozart-0.2.0.old");

        fs::write(&backup1, b"binary").unwrap();
        fs::write(&backup2, b"binary").unwrap();

        clean_backups(dir.path(), Some(&backup2)).expect("clean_backups should succeed");

        assert!(!backup1.exists(), "backup1 should be removed");
        assert!(backup2.exists(), "backup2 should be preserved (excepted)");
    }

    // ── test_effective_channel ────────────────────────────────────────────────

    #[test]
    fn test_effective_channel() {
        assert_eq!(effective_channel(false), "stable");
        assert_eq!(effective_channel(true), "preview");
    }

    // ── test_version_from_backup ──────────────────────────────────────────────

    #[test]
    fn test_version_from_backup() {
        let path = PathBuf::from("/some/dir/mozart-0.3.1.old");
        assert_eq!(version_from_backup(&path), "0.3.1");

        let bad_path = PathBuf::from("/some/dir/unknown-file.txt");
        assert_eq!(version_from_backup(&bad_path), "unknown");
    }
}
